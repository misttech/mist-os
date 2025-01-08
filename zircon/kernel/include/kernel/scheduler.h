// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_H_

#include <lib/fit/function.h>
#include <lib/fxt/interned_string.h>
#include <lib/power-management/energy-model.h>
#include <lib/power-management/power-state.h>
#include <lib/relaxed_atomic.h>
#include <lib/zircon-internal/macros.h>
#include <platform.h>
#include <stdint.h>
#include <zircon/syscalls/scheduler.h>
#include <zircon/syscalls/system.h>
#include <zircon/types.h>

#include <fbl/intrusive_pointer_traits.h>
#include <fbl/intrusive_wavl_tree.h>
#include <fbl/wavl_tree_best_node_observer.h>
#include <ffl/fixed.h>
#include <kernel/auto_lock.h>
#include <kernel/dpc.h>
#include <kernel/mp.h>
#include <kernel/owned_wait_queue.h>
#include <kernel/scheduler_state.h>
#include <kernel/spinlock.h>
#include <kernel/thread.h>
#include <kernel/wait.h>
#include <ktl/optional.h>

// Forward declarations.
struct percpu;

// Ensure this define has a value when not defined globally by the build system.
#ifndef SCHEDULER_TRACING_LEVEL
#define SCHEDULER_TRACING_LEVEL 0
#endif

// Ensure this define has a value when not defined globally by the build system.
#ifndef SCHEDULER_QUEUE_TRACING_ENABLED
#define SCHEDULER_QUEUE_TRACING_ENABLED false
#endif

// Performance scale of a CPU relative to the highest performance CPU in the
// system.
using SchedPerformanceScale = ffl::Fixed<int64_t, 31>;

// Converts a userspace CPU performance scale to a SchedPerformanceScale value.
constexpr SchedPerformanceScale ToSchedPerformanceScale(zx_cpu_performance_scale_t value) {
  const size_t FractionaBits = sizeof(value.fractional_part) * 8;
  return ffl::FromRaw<FractionaBits>(uint64_t{value.integral_part} << FractionaBits |
                                     value.fractional_part);
}

// Converts a SchedPerformanceScale value to a userspace CPU performance scale.
constexpr zx_cpu_performance_scale_t ToUserPerformanceScale(SchedPerformanceScale value) {
  using UserScale = ffl::Fixed<uint64_t, 32>;
  const UserScale user_scale{value};
  const uint64_t integral = user_scale.Integral().raw_value() >> UserScale::Format::FractionalBits;
  const uint64_t fractional = user_scale.Fraction().raw_value();
  return {.integral_part = uint32_t(integral), .fractional_part = uint32_t(fractional)};
}

// Implements fair and deadline scheduling algorithms and manages the associated
// per-CPU state.
class Scheduler {
 public:
  // fwd decl of a helper class used to implement various PI operations.  See
  // //zircon/kernel/kernel/scheduler_pi.cc to see how it is used.
  template <typename Op, typename TargetType>
  class PiOperation;

  // Default minimum granularity of time slices.
  static constexpr SchedDuration kDefaultMinimumGranularity = SchedMs(1);

  // Default target latency for a scheduling period.
  static constexpr SchedDuration kDefaultTargetLatency = SchedMs(8);

  // The threshold for cross-cluster work stealing. Queues with an estimated
  // runtime below this value are not stolen from if the target and destination
  // CPUs are in different logical clusters. In a performance-balanced system,
  // this tunable value approximates the cost of cross-cluster migration due to
  // cache misses, assuming a task has high cache affinity in its current
  // cluster. This tunable may be increased to limit cross-cluster spill over.
  static constexpr SchedDuration kInterClusterThreshold = SchedMs(2);

  // The threshold for early termination when searching for a CPU to place a
  // task. Queues with an estimated runtime below this value are sufficiently
  // unloaded. In a performance-balanced system, this tunable value approximates
  // the cost of intra-cluster migration due to cache misses, assuming a task
  // has high cache affinity with the last CPU it ran on. This tunable may be
  // increased to limit intra-cluster spill over.
  static constexpr SchedDuration kIntraClusterThreshold = SchedUs(25);

  // The per-CPU deadline utilization limit to attempt to honor when selecting a
  // CPU to place a task. It is up to userspace to ensure that the total set of
  // deadline tasks can honor this limit. Even if userspace ensures the total
  // set of deadline utilizations is within the total available processor
  // resources, when total utilization is high enough it may not be possible to
  // honor this limit due to the bin packing problem.
  static constexpr SchedUtilization kCpuUtilizationLimit{1};

  // The maximum deadline utilization permitted for a single thread. This limit
  // is applied when scaling the utilization of a deadline task to the relative
  // performance of a candidate target processor -- placing a task on a
  // processor that would cause the scaled thread utilization to exceed this
  // value is avoided if possible.
  static constexpr SchedUtilization kThreadUtilizationMax{1};

  // The adjustment rates of the exponential moving averages tracking the
  // expected runtimes of each thread.
  static constexpr ffl::Fixed<int32_t, 2> kExpectedRuntimeAlpha = ffl::FromRatio(1, 4);
  static constexpr ffl::Fixed<int32_t, 0> kExpectedRuntimeBeta = ffl::FromRatio(1, 1);

  Scheduler() = default;
  ~Scheduler() = default;

  Scheduler(const Scheduler&) = delete;
  Scheduler& operator=(const Scheduler&) = delete;

  // Accessors for total weight and number of runnable tasks.
  SchedWeight GetTotalWeight() const TA_EXCL(queue_lock_);
  size_t GetRunnableTasks() const TA_EXCL(queue_lock_);

  // Dumps the state of the run queue to the specified output target.  Note that
  // interrupts must be disabled in order to call this method.
  void Dump(FILE* output_target = stdout) TA_EXCL(queue_lock_);

  // Dumps info about the currently active thread (if any).  Note that
  // interrupts must be disabled in order to call this method.
  void DumpActiveThread(FILE* output_target = stdout) TA_EXCL(queue_lock_);

  // Returns the number of the CPU this scheduler instance is associated with.
  cpu_num_t this_cpu() const { return this_cpu_; }

  // Returns the index of the logical cluster of the CPU this scheduler instance
  // is associated with.
  size_t cluster() const { return cluster_; }

  // Returns the lock-free value of the predicted queue time for the CPU this
  // scheduler instance is associated with.
  SchedDuration predicted_queue_time_ns() const {
    return exported_total_expected_runtime_ns_.load();
  }

  // Returns the lock-free value of the predicted deadline utilization for the
  // CPU this scheduler instance is associated with.
  SchedUtilization predicted_deadline_utilization() const {
    return exported_total_deadline_utilization_.load();
  }

  // Returns the performance scale of the CPU this scheduler instance is
  // associated with.
  SchedPerformanceScale performance_scale() const TA_REQ(queue_lock_) { return performance_scale_; }

  // Returns the reciprocal performance scale of the CPU this scheduler instance
  // is associated with.
  SchedPerformanceScale performance_scale_reciprocal() const TA_REQ(queue_lock_) {
    return performance_scale_reciprocal_;
  }

  // Returns a pointer to the currently running thread, if any.
  //
  // TODO(johngro); This is not particularly safe.  As soon as we returned this
  // thread's pointer, it could (in theory) exit.  We probably need to require
  // that the scheduler's queue lock be held in order to peek at the active
  // thread member.
  Thread* active_thread() const TA_EXCL(queue_lock_) {
    Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
    return active_thread_;
  }

  // Public entry points.

  // See the comment in Thread::CreateEtc for why InitializeThread does not take
  // a stance on whether or not we should be in an active chain-lock
  // transaction. On the surface, demanding that we hold a threads lock without
  // also demanding that we also be involved in an active chain-lock transaction
  // is an apparent contradiction of some fundamental invariants (how can a
  // chain lock be held without being in a transaction?), but when used
  // exclusively from Thread::CreateEtc, it should be OK.
  static void InitializeThread(Thread* thread, const SchedulerState::BaseProfile& profile)
      TA_REQ(thread->get_lock());
  static void InitializeFirstThread(Thread* thread)
      TA_REQ(chainlock_transaction_token, thread->get_lock());
  static void RemoveFirstThread(Thread* thread)
      TA_REQ(chainlock_transaction_token, thread->get_lock());
  static void Block(Thread* const current_thread)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock());
  static void Yield(Thread* const current_thread)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock());

  // Note; no locks should be held when calling preempt.  The thread's lock will be obtained
  // unconditionally in the process.
  static void Preempt() TA_EXCL(chainlock_transaction_token);

  static void Reschedule(Thread* const current_thread)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock());
  static void RescheduleInternal(Thread* const current_thread)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock());

  // Finish unblocking a thread.  This function expects the thread to be locked
  // when called.  It will:
  //
  // 1) Select a scheduler compatible with the thread.
  // 2) Place the thread into that scheduler's run queue.
  // 3) Drop the thread's lock.
  // 4) Reschedule if needed/permitted.
  //
  static void Unblock(Thread* thread) TA_REQ(chainlock_transaction_token)
      TA_REL(thread->get_lock());

  // Unblock list expects to receive a list of threads, all of whose
  // locks are currently held.  It will drop each thread's lock after
  // successfully assigning it to a scheduler.
  static void Unblock(Thread::UnblockList thread_list) TA_REQ(chainlock_transaction_token);

  // UnblockIdle is used in the process of creation of the idle thread.  It
  // simply asserts that the thread is (in fact) flagged as the idle thread, and
  // that it has hard affinity for exactly one CPU (its CPU).  It then sets the
  // thread to be ready, and ensures that its curr_cpu_ state is correct.
  static void UnblockIdle(Thread* idle_thread)
      TA_REQ(chainlock_transaction_token, idle_thread->get_lock());

  // Called when something has changed about a thread which may result in it
  // needing to be migrated to a different CPU.  In particular, a change to
  // either its soft or hard affinity mask.
  static void Migrate(Thread* thread) TA_REQ(chainlock_transaction_token)
      TA_REL(thread->get_lock());
  static void MigrateUnpinnedThreads();

  // TimerTick is called when the preemption timer for a CPU has fired.
  //
  // This function is logically private and should only be called by timer.cc.
  static void TimerTick(SchedTime now);

  // Releases the lock held by the previous and current threads after a context
  // switch. This must be called by trampoline routines at some point before
  // jumping to the current thread's entry point.
  static void TrampolineLockHandoff();

  // Called when the base profile of |thread| has been changed by a program. The
  // thread must be either running or runnable when this method is called. If
  // the thread had been blocked instead, the change of the base profile would
  // have been managed at the WaitQueue level, perhaps eventually propagating to
  // the scheduler via UpstreamThreadBaseProfileChanged.
  static void ThreadBaseProfileChanged(Thread& thread)
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(thread), preempt_disabled_token);

  // Called when the base profile of a thread (|upstream|) which exists upstream
  // of |target| has changed its base profile.  It is possible for the rules
  // regarding scheduling penalties base profile changes to be slightly
  // different for when a thread's effective profile changes as a result of its
  // own base profile changing, vs the base profile of a thread which exists
  // upstream of it in a PI graph.  Because of this, the two scenarios are
  // handled separately using two different methods.
  template <typename TargetType>
  static void UpstreamThreadBaseProfileChanged(const Thread& upstream, TargetType& target)
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(upstream),
             ChainLockable::GetLock(target), preempt_disabled_token);

  // Called when a thread (|upstream|) blocks in an OwnedWaitQueue, and the
  // target of that OWQ (|target|) is running or runnable.  At the point that
  // this method is called, propagation of the static profile pressure
  // parameters (weight, deadline utilization, and minimum deadline) should have
  // already been propagated to |target|, and its EffectiveProfile should be
  // dirty.  This method is called in order to update the dynamic scheduling
  // parameters (start time, finish time, time slice remaining) of |target| as
  // needed because of the join operation.
  template <typename UpstreamType, typename TargetType>
  static void JoinNodeToPiGraph(const UpstreamType& upstream, TargetType& target)
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(upstream),
             ChainLockable::GetLock(target), preempt_disabled_token);

  // Called when a thread (|upstream|) unblocks from an OwnedWaitQueue and
  // leaves the PI graph whose running or runnable target is |target|.
  //
  // At the point that this method is called, propagation of the static profile
  // pressure parameters (weight, deadline utilization, and minimum deadline)
  // should have already been propagated to |target|, and its EffectiveProfile
  // should be dirty.  This method is called in order to update the dynamic
  // scheduling parameters (start time, finish time, time slice remaining) in
  // both |target| and |upstream| as needed because of the split operation.
  template <typename UpstreamType, typename TargetType>
  static void SplitNodeFromPiGraph(const UpstreamType& upstream, TargetType& target)
      TA_REQ(chainlock_transaction_token, ChainLockable::GetLock(upstream),
             ChainLockable::GetLock(target), preempt_disabled_token);

  // Updates the performance scales of the requested CPUs and returns the
  // effective values in place, which may be different than the requested values
  // if they are below the minimum safe values for the respective CPUs.
  //
  // Requires |count| <= num CPUs.
  static void UpdatePerformanceScales(zx_cpu_performance_info_t* info, size_t count)
      TA_EXCL(queue_lock_);

  // Gets the performance scales of up to count CPUs. Returns the last values
  // requested by userspace, even if they have not yet taken effect.
  //
  // Requires |count| <= num CPUs.
  static void GetPerformanceScales(zx_cpu_performance_info_t* info, size_t count)
      TA_EXCL(queue_lock_);

  // Gets the default performance scales of up to count CPUs. Returns the
  // initial values determined by the system topology, or 1.0 when no topology
  // is available.
  //
  // Requires |count| <= num CPUs.
  static void GetDefaultPerformanceScales(zx_cpu_performance_info_t* info, size_t count)
      TA_EXCL(queue_lock_);

  // Get the mask of valid CPUs that thread may run on. If a new mask
  // is set, the thread will be migrated to satisfy the new constraint.
  template <Affinity AffinityType>
  static cpu_mask_t SetCpuAffinity(Thread& thread, cpu_mask_t affinity)
      TA_EXCL(chainlock_transaction_token, thread.get_lock());

  // Run the passed callable while holding the specified scheduler's lock.
  // Currently used only by the threadload debug function in kernel/debug.cc
  template <typename Callable>
  static auto RunInLockedScheduler(cpu_num_t which_cpu, Callable callable) {
    // The call do Get will eventually DEBUG_ASSERT if our cpu ID is out of range.
    Scheduler& scheduler = *Get(which_cpu);
    Guard<MonitoredSpinLock, IrqSave> queue_guard{&scheduler.queue_lock_, SOURCE_TAG};
    return callable();
  }

  // Run the passed callable while holding the current CPU's scheduler's lock.
  // Used by routines in thread.cc who need to change the current thread's
  // state, but need to be holding the current scheduler's lock in order to do
  // so.
  template <typename Callable>
  static void RunInLockedCurrentScheduler(Callable callable) {
    DEBUG_ASSERT(arch_ints_disabled());
    Scheduler& scheduler = *Get();
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler.queue_lock_, SOURCE_TAG};
    callable();
  }

  // Run the passed callable while holding the specified thread's scheduler's
  // lock, if any. Used by SetMigrateFn in thread.cc.
  template <typename Callable>
  static void RunInThreadsSchedulerLocked(Thread* thread, Callable callable)
      TA_REQ(thread->get_lock()) {
    const cpu_num_t thread_cpu = thread->scheduler_state().curr_cpu();
    if (thread_cpu != INVALID_CPU) {
      Scheduler& scheduler = *Get(thread_cpu);
      Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler.queue_lock_, SOURCE_TAG};
      callable();
    } else {
      callable();
    }
  }

  // Set/clears the current cpu's bit in the global mp_active_mask.  Holds the
  // scheduler's queue lock while the bit is changed, enforcing the invariant
  // that a scheduler's active bit can only ever change while holding that
  // scheduler's lock.
  static void SetCurrCpuActive(bool is_active) {
    Scheduler& scheduler = *Get();
    Guard<MonitoredSpinLock, IrqSave> guard{&scheduler.queue_lock_, SOURCE_TAG};
    scheduler.SetIsActive(is_active);
  }

  // Peek at the current active mask for all schedulers.
  //
  // Note that this is just a lockless atomic load.  Baring special
  // circumstances, the set of active schedulers can change at any time.
  static cpu_mask_t PeekActiveMask() { return active_schedulers_.load(ktl::memory_order_relaxed); }

  // Peek at the current active mask state for a given CPU's scheduler.
  //
  // Note that this is just a lockless atomic load.  Baring special
  // circumstances, the set of active schedulers can change at any time.
  static bool PeekIsActive(cpu_num_t cpu) {
    const cpu_mask_t mask = cpu_num_to_mask(cpu);
    return (PeekActiveMask() & mask) != 0;
  }

  // Accessors for the "idle" state mask; similar to the active state mask.
  void SetIdle(bool is_idle) {
    const cpu_mask_t mask = cpu_num_to_mask(this_cpu_);
    if (is_idle) {
      idle_schedulers_.fetch_or(mask, ktl::memory_order_relaxed);
    } else {
      idle_schedulers_.fetch_and(~mask, ktl::memory_order_relaxed);
    }
  }
  static cpu_mask_t PeekIdleMask() { return idle_schedulers_.load(ktl::memory_order_relaxed); }
  static bool PeekIsIdle(cpu_num_t cpu) { return (PeekIdleMask() & cpu_num_to_mask(cpu)) != 0; }

  using PowerDomain = power_management::PowerDomain;

  // Sets the power domain for this scheduler instance, returning the previous domain. Called by
  // kernel tests and sys_system_set_processor_power_domain.
  fbl::RefPtr<PowerDomain> ExchangePowerDomain(fbl::RefPtr<PowerDomain> domain)
      TA_EXCL(queue_lock_) {
    Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
    return power_level_control_.ExchangePowerDomain(ktl::move(domain));
  }

  // Updates the current power level for this CPU with the value reported by the power level
  // controller. Called by kernel tests and sys_system_set_processor_power_state.
  zx::result<> SetPowerLevel(uint8_t power_level) TA_EXCL(queue_lock_) {
    Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
    return power_level_control_.UpdatePowerLevel(power_level);
  }

  // Returns the current active power level, if any.
  ktl::optional<uint8_t> GetPowerLevel() const TA_EXCL(queue_lock_) {
    Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
    return power_level_control_.GetPowerLevel();
  }
  // Sends a power level request through this scheduler's power level controller for testing. This
  // method is called from thread and IRQ context to ensure that the underlying machinery can be
  // safely called within Block, Unblock, and other scheduling operations.
  void RequestPowerLevelForTesting(uint8_t power_level) TA_EXCL(queue_lock_);

  // Returns the current power domain for this scheduler instance.
  fbl::RefPtr<PowerDomain> GetPowerDomainForTesting() TA_EXCL(queue_lock_) {
    Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
    return power_level_control_.domain();
  }

  // Returns the current active power coefficient.
  ktl::optional<uint64_t> GetActivePowerCoefficientNwForTesting() const TA_EXCL(queue_lock_) {
    Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
    return power_level_control_.active_power_coefficient_nw();
  }
  // Returns the max idle power coefficient.
  ktl::optional<uint64_t> GetMaxIdlePowerCoefficientNwForTesting() const TA_EXCL(queue_lock_) {
    Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
    return power_level_control_.max_idle_power_coefficient_nw();
  }

 private:
  friend struct Thread;

  // Allow percpu to init our cpu number and performance scale.
  friend struct percpu;
  // Load balancer test.
  friend struct LoadBalancerTestAccess;
  // Allow tests to modify our state.
  friend class LoadBalancerTest;
  // Diagnostics used by LockupDetector
  friend void DumpSchedulerActiveThreadDetails(Scheduler& scheduler);

  // EffectiveProfile and BaseProfile are both inner classes of SchedulerState which
  // are also frequently used by the methods in Scheduler.  Make a couple of handy
  // private aliases to keep us from needing to say
  // SchedulerState::(Effective|Base)Profile in all of the Scheduler methods.
  using EffectiveProfile = SchedulerState::EffectiveProfile;
  using BaseProfile = SchedulerState::BaseProfile;

  using SavedState = concurrent::ChainLockTransactionCommon::SavedState;

  static void LockHandoffInternal(SavedState saved_state, Thread* current_thread)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock());

  // Sets the initial values of the CPU performance scales for this Scheduler
  // instance.
  void InitializePerformanceScale(SchedPerformanceScale scale);

  static inline void RescheduleMask(cpu_mask_t cpus_to_reschedule_mask);
  static void PreemptLocked(Thread* current_thread)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock());

  // Unconditionally perform an expensive check of as many scheduler invariants
  // as we can.  Usually, this is not called directly; the ValidateInvariants
  // version (which is gated on a default-off build option) is preferred.
  void ValidateInvariantsUnconditional() const TA_REQ(queue_lock_, chainlock_transaction_token);
  void ValidateInvariants() const TA_REQ(queue_lock_, chainlock_transaction_token) {
    if constexpr (kSchedulerExtraInvariantValidation) {
      ValidateInvariantsUnconditional();
    }
  }

  // Runs the migration functions of threads in the save_state_list_, placing
  // the threads in their target RunQueues when done.
  //
  // Note that this method is called while holding the special "sched token"
  // used during reschedule operations. As a result, it is an absolute
  // requirement that any threads we lock in this method are not discoverable
  // and lockable from another scheduler that is also holding the "sched token."
  // Failure to meet this requirement will result in deadlock.
  void ProcessSaveStateList(SchedTime now) TA_REQ(chainlock_transaction_token);

  // Returns true if the given thread needs to be migrated from one CPU to another.
  static bool NeedsMigration(Thread* thread)
      TA_REQ(chainlock_transaction_token, thread->get_lock());

  // Lockup detector needs to be able to hold a scheduler's lock in order to
  // print diagnostics
  auto& queue_lock() TA_RET_CAP(queue_lock_) { return queue_lock_; }

  // Specifies how to associate a thread with a Scheduler instance, update
  // metadata, and whether/where to place the thread in a run queue.
  enum class Placement {
    // Selects a place in the queue based on the current insertion time and
    // thread weight or deadline.
    Insertion,

    // Selects a place in the queue based on the original insertion time and
    // the updated (inherited or changed) weight or deadline on the same CPU.
    Adjustment,

    // Selects a place in the queue based on the original insertion time and
    // the updated time slice due to being preempted by another thread.
    Preemption,

    // Selects a place in the queue based on the insertion time in the original
    // queue adjusted for the new queue.
    Migration,

    // Updates the metadata to account for a stolen thread that was just taken
    // from a different queue. This is distinct from Migration in that the
    // thread is not also enqueued, it is run immediately by the new CPU.
    Association,
  };

  // Returns the current system time as a SchedTime value.
  static SchedTime CurrentTime() { return SchedTime{current_mono_time()}; }

  // Returns the Scheduler instance for the current CPU.
  static Scheduler* Get();

  // Returns the Scheduler instance for the given CPU.
  static Scheduler* Get(cpu_num_t cpu);

  // Find an appropriate CPU for a thread to run on, then lock that scheduler,
  // confirm that it is still active, and invoke the user-supplied callback with
  // the scheduler's queue_lock held.
  //
  // This method may need to search for a target CPU for the thread more than
  // once if the initially chosen scheduler becomes deactivated after it was
  // selected, but before it was locked.
  template <typename Callable>
  static cpu_num_t FindActiveSchedulerForThread(Thread* thread, Callable action)
      TA_REQ_SHARED(thread->get_lock()) {
    while (true) {
      const cpu_num_t target_cpu = FindTargetCpu(thread);
      Scheduler* const target = Get(target_cpu);
      Guard<MonitoredSpinLock, NoIrqSave> target_queue_guard{&target->queue_lock_, SOURCE_TAG};

      if (target->IsLockedSchedulerActive()) {
        action(thread, target);
        return target_cpu;
      }

      IncFindTargetCpuRetriesKcounter();
    }
  }

  // A helper function used by FindActiveSchedulerForThread.  Returns a CPU to
  // run the given thread on, but does not lock that scheduler, meaning that the
  // selected scheduler can become de-activated before the caller can lock it.
  //
  // Typically, users will want to use FindActiveSchedulerForThread instead,
  // which will supply a locked scheduler which is guaranteed to be active.
  //
  static cpu_num_t FindTargetCpu(Thread* thread) TA_REQ_SHARED(thread->get_lock());

  // Increment the kcounter which tracks the number of times that an extra
  // attempt to find an active scheduler was needed during
  // FindActiveSchedulerForThread.  Sadly, the method cannot increment the
  // counter directly because of the way that kcounters need to be declared, in
  // a translation unit in an anonymous namespace, with no ability to fwd
  // declare the counter.
  static void IncFindTargetCpuRetriesKcounter();

  // Change this scheduler's CPU's active state. We require that the scheduler's
  // queue_lock be held during the operation. This enforces the invariant that
  // "the scheduler's active bit only changes while holding the scheduler's
  // queue lock".
  //
  // This means that things like "FindTargetCpu" are free to check the global
  // active mask to see whether or not we _think_ that a given scheduler is
  // likely to be active for selection purposes, but when it comes time to
  // actually add a thread to a scheduler's queues, we check the active state of
  // the scheduler's assigned CPU from within the safety of the queue lock using
  // IsLockedSchedulerActive()
  void SetIsActive(bool is_active) TA_REQ(queue_lock_) {
    const cpu_mask_t mask = cpu_num_to_mask(this_cpu_);
    if (is_active) {
      active_schedulers_.fetch_or(mask, ktl::memory_order_relaxed);
    } else {
      active_schedulers_.fetch_and(~mask, ktl::memory_order_relaxed);
    }
    arch_set_blocking_disallowed(!is_active);
  }

  // Check to see if |this| scheduler is currently active while holding its
  // queue_lock, ensuring that it will stay in its current state until after we
  // drop the lock.
  bool IsLockedSchedulerActive() TA_REQ(queue_lock_) { return PeekIsActive(this_cpu_); }

  using EndTraceCallback = fit::inline_function<void(), sizeof(void*)>;

  // Common logic for reschedule API.
  void RescheduleCommon(Thread* const current_thread, SchedTime now,
                        EndTraceCallback end_outer_trace = nullptr) TA_EXCL(queue_lock_)
      TA_REQ(chainlock_transaction_token, current_thread->get_lock());

  // Evaluates the schedule and returns the thread that should execute,
  // updating the run queue as necessary.
  Thread* EvaluateNextThread(SchedTime now, Thread* current_thread, bool timeslice_expired,
                             bool current_is_migrating, SchedDuration total_runtime_ns,
                             Guard<MonitoredSpinLock, NoIrqSave>& queue_guard)
      TA_REQ(chainlock_transaction_token, queue_lock_, current_thread->get_lock());

  // Adds a thread to the run queue tree. The thread must be active on this
  // CPU.
  void QueueThread(Thread* thread, Placement placement, SchedTime now = SchedTime{0},
                   SchedDuration total_runtime_ns = SchedDuration{0})
      TA_REQ(queue_lock_, thread->get_lock());

  // Removes the thread at the head of the first eligible run queue, or attempt
  // to steal a thread from another scheduler if we have no eligible thread in
  // our current run queues.  If none of these attempts succeed, we will fall
  // back on returning this scheduler's Idle thread.
  //
  // In all cases, the thread being returned will not be locked, and will not be
  // a member of any current run queue.  One of the following *must* happen to
  // the thread after this.
  //
  // 1) The thread is added back to one of these scheduler's run queues (this
  //    is common during a PI interaction involving the thread).
  // 2) The thread is flagged as "reschedule_in_progress" in its scheduler-owned
  //    variables, before becoming the active thread in its scheduler (after
  //    dropping the scheduler lock and re-obtaining the thread's lock
  //    exclusively).  This happens in RescheduleCommon when EvaluateNextThread
  //    chooses a new thread to run.
  // 3) The thread is flagged as "steal_in_progress" after being removed from
  //    another scheduler's run queue, but before being locked exclusively and
  //    re-inserted into its new scheduler's run queue.
  //
  // Note that these rules apply to *all* the versions of Dequeue listed below.
  Thread* DequeueThread(SchedTime now, Guard<MonitoredSpinLock, NoIrqSave>& queue_guard)
      TA_REQ(chainlock_transaction_token, queue_lock_);

  // Removes the thread at the head of the fair run queue and returns it.
  Thread* DequeueFairThread() TA_REQ(queue_lock_);

  // Removes the eligible thread with the earliest deadline in the deadline run
  // queue and returns it.
  Thread* DequeueDeadlineThread(SchedTime eligible_time) TA_REQ(queue_lock_);

  // Removes the eligible thread with a deadline earlier than the given deadline
  // and returns it or nullptr if one does not exist.
  Thread* DequeueEarlierDeadlineThread(SchedTime eligible_time, SchedTime finish_time)
      TA_REQ(queue_lock_);

  // Returns the eligible thread in the run queue with a deadline earlier than
  // the given deadline, or nullptr if one does not exist.
  //
  // See the note in |FindEarliestEligibleThread| for lifecycle rules for the
  // returned pointer (if any)
  Thread* FindEarlierDeadlineThread(SchedTime eligible_time, SchedTime finish_time)
      TA_REQ(queue_lock_);

  // Attempts to steal work from other busy CPUs. Returns nullptr if no work was
  // stolen, otherwise returns a pointer to the stolen thread that is now
  // associated with the local Scheduler instance.
  Thread* StealWork(SchedTime now, SchedPerformanceScale scale_up_factor)
      TA_REQ(chainlock_transaction_token) TA_EXCL(queue_lock_);

  // Returns the time that the next deadline task will become eligible or infinite
  // if there are no ready deadline tasks.
  SchedTime GetNextEligibleTime() TA_REQ(queue_lock_);

  // Calculates the timeslice of the thread based on the current run queue
  // state.
  SchedDuration CalculateTimeslice(const Thread* thread) TA_REQ(queue_lock_)
      TA_REQ_SHARED(thread->get_lock());

  // Returns the completion time clamped to the start of the earliest deadline
  // thread that will become eligible in that time frame.
  SchedTime ClampToDeadline(SchedTime completion_time) TA_REQ(queue_lock_);

  // Returns the completion time clamped to the start of the earliest deadline
  // thread that will become eligible in that time frame and also has an earlier
  // deadline than the given finish time.
  SchedTime ClampToEarlierDeadline(SchedTime completion_time, SchedTime finish_time)
      TA_REQ(queue_lock_);

  // Updates the timeslice of the thread based on the current run queue state.
  // Returns the absolute deadline for the next time slice, which may be earlier
  // than the completion of the time slice if other threads could preempt the
  // given thread before the time slice is exhausted.
  SchedTime NextThreadTimeslice(Thread* thread, SchedTime now)
      TA_REQ(queue_lock_, thread->get_lock());

  // Updates the scheduling period based on the number of active threads.
  void UpdatePeriod() TA_REQ(queue_lock_);

  // Updates the global virtual timeline.
  void UpdateTimeline(SchedTime now) TA_REQ(queue_lock_);

  static void SetThreadMigrateFnInternal(Thread* thread, Thread::MigrateFn migrate_fn)
      TA_REQ(thread->get_lock(), Thread::get_list_lock());

  // Makes a thread active on this CPU's scheduler and inserts it into the
  // run queue tree.
  void Insert(SchedTime now, Thread* thread, Placement placement = Placement::Insertion)
      TA_REQ(queue_lock_, thread->get_lock(), thread->get_scheduler_variable_lock());

  // Removes the thread from this CPU's scheduler as it transitions to another
  // CPU (either because it is being stolen, or migrated). The thread's lock
  // cannot be held exclusively at this point, so some of the thread's state
  // will remain in a transient state until the thread can be re-inserted in its
  // new home.  Specifically, this includes:
  //
  // 1) The thread's current CPU id.
  // 2) The thread's start time (if it is a fair thread).
  // 3) The thread's finish time (if it is a fair thread).
  //
  // This state will be cleaned up by FinishTransition once the thread has been
  // locked exclusively.
  void RemoveForTransition(Thread* thread, SchedulerQueueState::TransientState target_state)
      TA_REQ(queue_lock_, thread->get_scheduler_variable_lock()) TA_REQ_SHARED(thread->get_lock());

  void FinishTransition(SchedTime now, Thread* thread)
      TA_REQ(queue_lock_, thread->get_lock(), thread->get_scheduler_variable_lock());

  // Removes the thread from this CPU's scheduler. The thread must not be in
  // the run queue tree.  This is the same operation as RemoveForTransition, but it
  // requires that the thread's lock is held exclusively, which allows the CPU
  // id, start time, and finish time to be updated.
  void Remove(Thread* thread, SchedulerQueueState::TransientState reason =
                                  SchedulerQueueState::TransientState::None)
      TA_REQ(queue_lock_, thread->get_lock(), thread->get_scheduler_variable_lock()) {
    SchedulerState& state = thread->scheduler_state();

    RemoveForTransition(thread, reason);
    state.curr_cpu_ = INVALID_CPU;
    if (IsFairThread(thread)) {
      state.start_time_ = SchedNs(0);
      state.finish_time_ = SchedNs(0);
    }
  }

  // Removes a specific thread from its current RunQueue.  Note that the thread
  // must currently exist in its queue, it is an error otherwise.
  void EraseFromQueue(Thread* thread) TA_REQ(queue_lock_, thread->get_scheduler_variable_lock())
      TA_REQ_SHARED(thread->get_lock()) {
    DEBUG_ASSERT(thread->scheduler_queue_state().InQueue());
    const SchedulerState& state = const_cast<const Thread*>(thread)->scheduler_state();
    RunQueue& queue =
        state.discipline() == SchedDiscipline::Fair ? fair_run_queue_ : deadline_run_queue_;
    queue.erase(*thread);
  }

  // Returns true if there is at least one eligible deadline thread in the
  // run queue.
  inline bool IsDeadlineThreadEligible(SchedTime eligible_time) TA_REQ(queue_lock_) {
    if (deadline_run_queue_.is_empty()) {
      return false;
    }

    const Thread& front = deadline_run_queue_.front();
    AssertInRunQueue(front);
    return front.scheduler_state().start_time_ <= eligible_time;
  }

  // Updates the total expected runtime estimator and exports the atomic shadow
  // variable for cross-CPU readers.
  inline void UpdateTotalExpectedRuntime(SchedDuration delta_ns) TA_REQ(queue_lock_);

  // Updates to total deadline utilization estimator and exports the atomic
  // shadow variable for cross-CPU readers.
  inline void UpdateTotalDeadlineUtilization(SchedUtilization delta_ns) TA_REQ(queue_lock_);

  // Computes the estimated energy consumed since the last reschedule and
  // updates the current thread and CPU energy accumulators.
  inline void UpdateEstimatedEnergyConsumption(Thread* current_thread,
                                               SchedDuration actual_runtime_ns)
      TA_REQ(current_thread->get_lock(), queue_lock_);

  // Utilities to scale up or down the given value by the performance scale of the CPU.
  template <typename T>
  inline T ScaleUp(T value) const TA_REQ(queue_lock_);
  template <typename T>
  inline T ScaleDown(T value) const TA_REQ(queue_lock_);

  // Update trace counters which track the total number of runnable threads for a CPU
  inline void TraceTotalRunnableThreads() const TA_REQ(queue_lock_);

  // Returns a new flow id when flow tracing is enabled, zero otherwise.
  inline static uint64_t NextFlowId();

  // Traits type to adapt the WAVLTree to Thread with node state in the
  // scheduler_state member.
  struct TaskTraits {
    using KeyType = SchedulerState::KeyType;
    static KeyType GetKey(const Thread& thread) TA_NO_THREAD_SAFETY_ANALYSIS {
      return thread.scheduler_state().key();
    }
    static bool LessThan(KeyType a, KeyType b) { return a < b; }
    static bool EqualTo(KeyType a, KeyType b) { return a == b; }
    static auto& node_state(Thread& thread) TA_NO_THREAD_SAFETY_ANALYSIS {
      return thread.scheduler_queue_state().run_queue_node;
    }
  };

  // Observer that maintains the subtree invariant min_finish_time as nodes are
  // added to and removed from the run queue.
  struct SubtreeMinTraits {
    static SchedTime GetValue(const Thread& node) TA_NO_THREAD_SAFETY_ANALYSIS {
      return node.scheduler_state().finish_time_;
    }

    static SchedTime GetSubtreeBest(const Thread& node) TA_NO_THREAD_SAFETY_ANALYSIS {
      return node.scheduler_state().min_finish_time_;
    }

    static bool Compare(SchedTime a, SchedTime b) { return a < b; }

    static void AssignBest(Thread& node, SchedTime val) TA_NO_THREAD_SAFETY_ANALYSIS {
      node.scheduler_state().min_finish_time_ = val;
    }

    static void ResetBest(Thread& target) {}
  };

  using SubtreeMinObserver = fbl::WAVLTreeBestNodeObserver<SubtreeMinTraits>;

  // Alias of the WAVLTree type for the run queue.
  using RunQueue = fbl::WAVLTree<TaskTraits::KeyType, Thread*, TaskTraits, fbl::DefaultObjectTag,
                                 TaskTraits, SubtreeMinObserver>;

  // Finds the next eligible thread in the given run queue.
  //
  // Note: Any Thread* returned by this method may only be used as long as the
  // queue lock is held. Dropping the queue lock allows the thread to be removed
  // from the scheduler, and potentially exit, destroying the Thread and
  // invalidating the thread pointer in the process.
  Thread* FindEarliestEligibleThread(RunQueue* run_queue, SchedTime eligible_time)
      TA_REQ(queue_lock_);

  // Finds the next eligible thread in the given run queue that also passes the
  // given predicate.
  //
  // See the note in |FindEarliestEligibleThread| for lifecycle rules for the
  // returned pointer (if any)
  template <typename Predicate>
  Thread* FindEarliestEligibleThread(RunQueue* run_queue, SchedTime eligible_time,
                                     Predicate&& predicate) TA_REQ(queue_lock_);

  // Emits queue event tracers for trace-based scheduler performance analysis.
  inline void TraceThreadQueueEvent(const fxt::InternedString& name, const Thread* thread) const
      TA_REQ(queue_lock_) TA_REQ_SHARED(thread->get_lock());

  // Returns true if the given thread is fair scheduled.
  static bool IsFairThread(const Thread* thread) TA_REQ_SHARED(thread->get_lock()) {
    return thread->scheduler_state().effective_profile_.IsFair();
  }

  // Returns true if the given thread is deadline scheduled.
  static bool IsDeadlineThread(const Thread* thread) TA_REQ_SHARED(thread->get_lock()) {
    return thread->scheduler_state().effective_profile_.IsDeadline();
  }

  // Returns true if the given thread's time slice is adjustable under changes to
  // the fair scheduler demand on the CPU.
  static bool IsThreadAdjustable(const Thread* thread) TA_REQ_SHARED(thread->get_lock()) {
    // Checking the thread state avoids unnecessary adjustments on a thread that
    // is no longer competing.
    return !thread->IsIdle() && IsFairThread(thread) && thread->state() == THREAD_READY;
  }

  // Protects run queues and associated metadata for this Scheduler instance.
  // The queue lock is the bottom most lock in the system for the CPU it is
  // associated with: no other locks may be nested within a queue lock. A queue
  // lock may be acquired across CPUs, however, the no nesting rule still
  // applies.
  //
  // Note: while the queue_lock_ is the bottom-most lock in the system from the
  // scheduler's perspective, it is not (formally) a leaf-lock right now.
  // Currently, there is one place in the system (DumpThreadLocked) where the
  // queue_lock_ needs to be held, while fprintf is called.
  //
  // This happens when the lockup-detector is unhappy, and asking the Scheduler
  // to print information about its run queue state.  Most of the printf targets
  // which could be passed to the scheduler also involve holding locks.  If the
  // queue_lock_ is flagged as a leaf lock, and lockdep is enabled, and the
  // lockup-detector attempt to report a hang, it will cause lock-dep to
  // complain about obtaining a lock while holding a leaf-lock, which will cause
  // more printing, which causes more complaints, etc...
  //
  // See https://fxbug.dev/42070437 for more details.
  mutable DECLARE_SPINLOCK_WITH_TYPE(Scheduler, MonitoredSpinLock) queue_lock_
      TA_ACQ_AFTER(Thread::get_list_lock());

  // Alias of the queue lock wrapper type.
  using QueueLock = decltype(queue_lock_);

  // Simplified static assertion on the wrapped queue lock type to avoid the more verbose template
  // parameters required by lockdep::AssertHeld.
  static void AssertHeld(const QueueLock& lock) TA_ASSERT(lock) TA_ASSERT(lock.lock()) {
    lock.lock().AssertHeld();
  }

  // Asserts that a given thread belongs to |this| Scheduler instance.
  // Requires holding the scheduler's queue_lock_.
  //
  void AssertInScheduler(const Thread& t) const TA_REQ(queue_lock_) TA_ASSERT_SHARED(t.get_lock())
      TA_ASSERT(t.get_scheduler_variable_lock()) {
    [&]() TA_NO_THREAD_SAFETY_ANALYSIS {
      // TODO(johngro): Come back here and remove the `&t == active_thread_`
      // clause once other system-wide invariants have been cleaned up.  See
      // https://fxbug.dev/340551498 for details.
      DEBUG_ASSERT_MSG((t.scheduler_state().curr_cpu() == this_cpu_) || (&t == active_thread_),
                       "Thread %lu expected to be a member of scheduler %u, but curr_cpu says %u",
                       t.tid(), this_cpu_, t.scheduler_state().curr_cpu());
    }();
  }

  // It would be nice to be able to make a stronger assertion here (that the
  // thread was not just in a run queue, but the specific run queue we expect it
  // to be in), but unfortunately, there is no inexpensive way of doing that.
  //
  // Thankfully, most of the time that we need to invoke an assertion like this,
  // it is pretty obvious from the context that the assertion is true.  It might
  // be reasonably to (someday) consider removing even this weak runtime check,
  // and just leaving this as a static analysis workaround, and nothing more.
  inline void AssertInRunQueue(const Thread& t) const TA_REQ(queue_lock_)
      TA_ASSERT_SHARED(t.get_lock()) TA_ASSERT(t.get_scheduler_variable_lock()) {
    [&]() TA_NO_THREAD_SAFETY_ANALYSIS {
      DEBUG_ASSERT((t.scheduler_state().curr_cpu_ == this_cpu_) &&
                   (t.scheduler_queue_state().InQueue()));
    }();
  }

  // Add a couple of small no-op helpers which inform the static analyzer of
  // some properties which are very difficult to apply to the lambdas used in
  // the functional programming patterns below.  Neither of these methods
  // actually _check_ anything, so they must be used with *extreme caution*.
  //
  // They should only ever be used when the locking invariants are trivially
  // verifiable from the immediate context.
  //
  // MarkHasSchedulerAccess tells the static analyzer that we are exclusively
  // holding a scheduler's queue lock.  It's primary use case is in lambda's
  // which are declared inside of the scope of a queue_lock_ guard, and are used
  // for some sort of functional programming pattern.
  //
  // MarkHasOwnedThreadAccess tells the static analyzer that we have exclusive
  // access to a thread's scheduler-variables, and shared (read-only) access to
  // variables protected by a thread's lock (eg; the scheduler_state). The
  // conditions for this operation are that we are holding the queue_lock_ for
  // the scheduler that the thread is currently a member of (see
  // AssertInScheduler, above).
  //
  // MarkInFindActiveSchedulerForThreadCbk tells the static analyzer that we are
  // in the callback for an FindActiveSchedulerForThread operation, meaning that
  // we must hold both the thread's lock and the queue_lock for the thread and
  // scheduler involved in the operation.
  //
  // See StealWork for a practical example of where these methods are useful.
  //
  static inline void MarkHasSchedulerAccess(const Scheduler& s) TA_ASSERT(s.queue_lock_) {}
  static inline void MarkHasOwnedThreadAccess(const Thread& t)
      TA_ASSERT(t.get_scheduler_variable_lock()) TA_ASSERT_SHARED(t.get_lock()) {}
  static inline void MarkInFindActiveSchedulerForThreadCbk(const Thread& t, const Scheduler& s)
      TA_ASSERT(t.get_scheduler_variable_lock()) TA_ASSERT(t.get_lock()) TA_ASSERT(s.queue_lock_) {}

  // A mask of all of the currently active scheduler's in the system.  An
  // "active" scheduler is one who will accept threads to be executed.  An
  // "in-active" scheduler will not, and is typically the scheduler for a CPU
  // which is in the process of being taken offline.
  //
  // Note that to change the active state of a scheduler for CPU X (eg; to
  // toggle the state of bit X in the active_schedulers_ mask) that scheduler's
  // queue_lock_ must be held.  Therefore, observations of the schedulers'
  // active mask must typically be assumed to be volatile.  As soon as the mask
  // is observed, it can immediately change.
  static inline ktl::atomic<cpu_mask_t> active_schedulers_{0};

  // A mask of all of the currently idle scheduler's in the system.  An
  // "idle" scheduler is one which last selected the idle/power thread to run,
  // while a "busy" scheduler is one which is running a non-idle thread.
  //
  // Like the active mask, the idle mask is volatile and subject to change at
  // any time barring special circumstances.
  static inline ktl::atomic<cpu_mask_t> idle_schedulers_{0};

  // Flow id counter for sched_latency flow events.
  inline static RelaxedAtomic<uint64_t> next_flow_id_{1};

  // The run queue of fair scheduled threads ready to run, but not currently running.
  TA_GUARDED(queue_lock_)
  RunQueue fair_run_queue_;

  // The run queue of deadline scheduled threads ready to run, but not currently running.
  TA_GUARDED(queue_lock_)
  RunQueue deadline_run_queue_;

  // The list of threads that need to run the Save stage of their migrate functions.
  //
  // There are several invariants that must be maintained when interacting with this
  // list:
  // 1. Other CPUs can put threads into this save_state_list_, but only this Scheduler (and thus
  //    this CPU) can remove threads from it. Removal is currently handled in ProcessSaveStateList.
  // 2. Interrupts must be disabled while removing threads from this list, effectively ensuring
  //    that only one call to ProcessSaveStateList can be made at a time on a given CPU.
  // 3. Threads cannot be destroyed until they are removed from this list. This is guaranteed
  //    today by the fact that dead threads clean themselves up when they are scheduled, which is
  //    not possible if the thread is in this list.
  TA_GUARDED(queue_lock_)
  Thread::SaveStateList save_state_list_;

  // Pointer to the thread actively running on this CPU.
  TA_GUARDED(queue_lock_)
  Thread* active_thread_{nullptr};

  // Monotonically increasing counter to break ties when queuing tasks with
  // the same key. This has the effect of placing newly queued tasks behind
  // already queued tasks with the same key. This is also necessary to
  // guarantee uniqueness of the key as required by the WAVLTree container.
  TA_GUARDED(queue_lock_)
  uint64_t generation_count_{0};

  // Count of the fair threads running on this CPU, including threads in the run
  // queue and the currently running thread. Does not include the idle thread.
  TA_GUARDED(queue_lock_)
  int32_t runnable_fair_task_count_{0};

  // Count of the deadline threads running on this CPU, including threads in the
  // run queue and the currently running thread. Does not include the idle
  // thread.
  TA_GUARDED(queue_lock_)
  int32_t runnable_deadline_task_count_{0};

  // Total weights of threads running on this CPU, including threads in the
  // run queue and the currently running thread. Does not include the idle
  // thread.
  TA_GUARDED(queue_lock_)
  SchedWeight weight_total_{0};

  // The value of |weight_total_| when the current thread was scheduled.
  // Provides a reference for determining whether the total weights changed
  // since the last reschedule.
  SchedWeight scheduled_weight_total_{0};

  // The global virtual time of this run queue.
  TA_GUARDED(queue_lock_)
  SchedTime virtual_time_{0};

  // The system time since the last update to the global virtual time.
  TA_GUARDED(queue_lock_)
  SchedTime last_update_time_ns_{0};

  // The system time that the current time slice started.
  SchedTime start_of_current_time_slice_ns_{0};

  // The system time that the current thread should be preempted. Initialized to
  // ZX_TIME_INFINITE to pass the assertion now < target_preemption_time_ns_ (or
  // else the current time slice is expired) on the first entry into the
  // scheduler.
  SchedTime target_preemption_time_ns_{ZX_TIME_INFINITE};

  // The sum of the expected runtimes of all active threads on this CPU. This
  // value is an estimate of the average queuing time for this CPU, given the
  // current set of active threads.
  TA_GUARDED(queue_lock_)
  SchedDuration total_expected_runtime_ns_{0};

  // The sum of the worst case utilization of all active deadline threads on
  // this CPU.
  TA_GUARDED(queue_lock_)
  SchedUtilization total_deadline_utilization_{0};

  // Scheduling period in which every runnable task executes once in units of
  // minimum granularity.
  TA_GUARDED(queue_lock_)
  SchedDuration scheduling_period_grans_{kDefaultTargetLatency / kDefaultMinimumGranularity};

  // The smallest timeslice a thread is allocated in a single round.
  TA_GUARDED(queue_lock_)
  SchedDuration minimum_granularity_ns_{kDefaultMinimumGranularity};

  // The target scheduling period. The scheduling period is set to this value
  // when the number of tasks is low enough for the sum of all timeslices to
  // fit within this duration. This has the effect of increasing the size of
  // the timeslices under nominal load to reduce scheduling overhead.
  TA_GUARDED(queue_lock_)
  SchedDuration target_latency_grans_{kDefaultTargetLatency / kDefaultMinimumGranularity};

  // Performance scale of this CPU relative to the highest performance CPU. This
  // value is initially determined from the system topology, when available, and
  // by userspace performance/thermal management at runtime.
  TA_GUARDED(queue_lock_) SchedPerformanceScale performance_scale_ { 1 };
  TA_GUARDED(queue_lock_)
  RelaxedAtomic<SchedPerformanceScale> performance_scale_reciprocal_{SchedPerformanceScale{1}};

  // Performance scale requested by userspace. The operational performance scale
  // is updated to this value (possibly adjusted for the minimum allowed value)
  // on the next reschedule, after the current thread's accounting is updated.
  TA_GUARDED(queue_lock_) SchedPerformanceScale pending_user_performance_scale_ { 1 };

  // Default performance scale, determined from the system topology, when
  // available.
  TA_GUARDED(queue_lock_) SchedPerformanceScale default_performance_scale_ { 1 };

  // The CPU this scheduler instance is associated with.
  // NOTE: This member is not initialized to prevent clobbering the value set
  // by sched_early_init(), which is called before the global ctors that
  // initialize the rest of the members of this class.
  // TODO(eieio): Figure out a better long-term solution to determine which
  // CPU is associated with each instance of this class. This is needed by
  // non-static methods that are called from arbitrary CPUs, namely Insert().
  cpu_num_t this_cpu_;

  // The index of the logical cluster this CPU belongs to. CPUs with the same
  // logical cluster index have the best chance of good cache affinity with
  // respect to load distribution decisions.
  size_t cluster_{0};

  // Flag indicating that this Scheduler's CPU is in the process of becoming
  // inactive.  When true, threads who have a migration function which must be
  // run on this CPU are permitted to join this CPU's ready queue, even if the
  // CPU has already been flagged as inactive.  The thread who is in charge of
  // deactivating the CPU will make sure to run the migration function of the
  // thread on this CPU before finishing the deactivation process.
  ktl::atomic<bool> cpu_deactivating_{false};

  // Values exported for lock-free access across CPUs. These are mirrors of the
  // members of the same name without the exported_ prefix. This avoids
  // unnecessary atomic loads when updating the values using arithmetic
  // operations on the local CPU. These values are atomically readonly to other
  // CPUs.
  // TODO(eieio): Look at cache line alignment for these members to optimize
  // cache performance.
  RelaxedAtomic<SchedDuration> exported_total_expected_runtime_ns_{SchedNs(0)};
  RelaxedAtomic<SchedUtilization> exported_total_deadline_utilization_{SchedUtilization{0}};

  // The thread which ran just before this thread was scheduled.  Used by
  // Scheduler::LockHandoff to release the previous thread's lock after a
  // context switch operation has fully completed.
  Thread* previous_thread_{nullptr};

  // PowerLevelControl encapsulates the CPU energy model and power level control interface for
  // energy aware scheduling.
  //
  // Instances of PowerLevelControl are protected by Scheduler::queue_lock_.
  class PowerLevelControl {
   public:
    explicit PowerLevelControl(Scheduler* scheduler) : request_dpc_{DpcHandler, scheduler} {}

    // Sets the power domain associated with this scheduler.
    fbl::RefPtr<PowerDomain> ExchangePowerDomain(fbl::RefPtr<PowerDomain> domain) {
      // Clear the request to ensure that a DPC racing with a domain change cannot latch a pending
      // request intended for the previous domain and the new domain ref pointer together.
      pending_update_request_.reset();
      return power_state_.SetOrUpdateDomain(ktl::move(domain));
    }

    // Returns the current active power level.
    ktl::optional<uint8_t> GetPowerLevel() const { return power_state_.active_power_level(); }

    // Called by power level controller server (e.g. the userspace component servicing the kernel
    // power level control interface) for the domain associated with this scheduler to acknowledge
    // that a power level request has been completed.
    zx::result<> UpdatePowerLevel(uint8_t power_level) {
      return power_state_.UpdateActivePowerLevel(power_level);
    }

    // Called by the scheduler to request a power level change for the domain associated with this
    // scheduler.
    [[nodiscard("A reschedule is required when true")]] bool RequestPowerLevel(uint8_t power_level);

    // Called by RescheduleCommon to send any pending request to the registered power level
    // controller.
    void SendPendingPowerLevelRequest() {
      if (power_state_.is_serving() && pending_update_request_.has_value()) {
        request_timer_.Cancel();
        request_timer_.Set(Deadline::infinite_past(), TimerHandler, this);
      }
    }

    // Gets the power domain associated with this scheduler.
    const fbl::RefPtr<PowerDomain>& domain() const { return power_state_.domain(); }

    uint64_t active_power_coefficient_nw() const {
      return power_state_.active_power_coefficient_nw();
    }

    uint64_t max_idle_power_coefficient_nw() const {
      return power_state_.max_idle_power_coefficient_nw();
    }

   private:
    Scheduler& scheduler() { return *request_dpc_.arg<Scheduler>(); }
    cpu_num_t cpu() { return scheduler().this_cpu(); }

    static void TimerHandler(Timer* timer, zx_instant_mono_t now, void* arg);
    static void DpcHandler(Dpc* dpc);

    power_management::PowerState power_state_;
    ktl::optional<power_management::PowerLevelUpdateRequest> pending_update_request_;
    Timer request_timer_;
    Dpc request_dpc_;
  };

  TA_GUARDED(queue_lock_) PowerLevelControl power_level_control_ { this };
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_H_
