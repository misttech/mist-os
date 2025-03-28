// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "kernel/scheduler.h"

#include <assert.h>
#include <debug.h>
#include <inttypes.h>
#include <lib/arch/intrin.h>
#include <lib/counters.h>
#include <lib/kconcurrent/chainlock.h>
#include <lib/kconcurrent/chainlock_transaction.h>
#include <lib/ktrace.h>
#include <lib/sched/affine.h>
#include <lib/zircon-internal/macros.h>
#include <platform.h>
#include <stdio.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/listnode.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <new>

#include <arch/interrupt.h>
#include <arch/ops.h>
#include <arch/thread.h>
#include <ffl/string.h>
#include <kernel/auto_lock.h>
#include <kernel/auto_preempt_disabler.h>
#include <kernel/cpu.h>
#include <kernel/lockdep.h>
#include <kernel/mp.h>
#include <kernel/percpu.h>
#include <kernel/scheduler_internal.h>
#include <kernel/scheduler_state.h>
#include <kernel/thread.h>
#include <ktl/algorithm.h>
#include <ktl/forward.h>
#include <ktl/limits.h>
#include <ktl/move.h>
#include <ktl/pair.h>
#include <ktl/span.h>
#include <ktl/tuple.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <vm/vm.h>

#include <ktl/enforce.h>

using ffl::Round;

// Counts the number of times we set the preemption timer to fire at a point
// that's prior to the time at which the CPU last entered the scheduler.  See
// also the comment where this counter is used.
KCOUNTER(counter_preempt_past, "scheduler.preempt_past")

// Counts the number of times we dequeue a deadline thread whose finish_time is
// earlier than its eligible_time.
KCOUNTER(counter_deadline_past, "scheduler.deadline_past")

// Counts the number of times a thread's effective profile or affinity changed
// while transitioning from READY to RUNNING.
KCOUNTER(counter_update_in_transition, "scheduler.update_in_transition")

// Counts the number of times we had to run FindTargetCpu again because our
// selected Target became in-active after we chose it.
KCOUNTER(counter_find_target_cpu_retries, "scheduler.find_target_cpu.retries")

// Counts the number of times the fair timeline was snapped forward to make a
// fair thread eligible to run.
KCOUNTER(counter_fair_timeline_snap_count, "scheduler.fair-timeline-snap.count")

// Counts the amount of variable time the fair timeline was snapped forward to
// make a fair thread eligible to run.
KCOUNTER(counter_fair_timeline_snap_time, "scheduler.fair-timeline-snap.time")

namespace {

// Utility operator to make expressions more succinct that update thread times
// and durations of basic types using the fixed-point counterparts.
constexpr zx_instant_mono_t& operator+=(zx_instant_mono_t& value, SchedDuration delta) {
  value += delta.raw_value();
  return value;
}

inline zx_thread_state_t UserThreadState(const Thread* thread) TA_REQ_SHARED(thread->get_lock()) {
  switch (thread->state()) {
    case THREAD_INITIAL:
    case THREAD_READY:
      return ZX_THREAD_STATE_NEW;
    case THREAD_RUNNING:
      return ZX_THREAD_STATE_RUNNING;
    case THREAD_BLOCKED:
    case THREAD_BLOCKED_READ_LOCK:
    case THREAD_SLEEPING:
      return ZX_THREAD_STATE_BLOCKED;
    case THREAD_SUSPENDED:
      return ZX_THREAD_STATE_SUSPENDED;
    case THREAD_DEATH:
      return ZX_THREAD_STATE_DEAD;
    default:
      return UINT32_MAX;
  }
}

constexpr int32_t kIdleWeight = ktl::numeric_limits<int32_t>::min();

// Writes a context switch record to the ktrace buffer. This is always enabled
// so that user mode tracing can track which threads are running.
inline void TraceContextSwitch(const Thread* current_thread, const Thread* next_thread,
                               cpu_num_t current_cpu)
    TA_REQ_SHARED(current_thread->get_lock(), next_thread->get_lock()) {
  const SchedulerState& current_state = current_thread->scheduler_state();
  const SchedulerState& next_state = next_thread->scheduler_state();
  KTRACE_CONTEXT_SWITCH(
      "kernel:sched", current_cpu, UserThreadState(current_thread), current_thread->fxt_ref(),
      next_thread->fxt_ref(),
      ("outgoing_weight", current_thread->IsIdle() ? kIdleWeight : current_state.weight()),
      ("incoming_weight", next_thread->IsIdle() ? kIdleWeight : next_state.weight()));
}

// Writes a thread wakeup record to the ktrace buffer. This is always enabled
// so that user mode tracing can track which threads are waking.
inline void TraceWakeup(const Thread* thread, cpu_num_t target_cpu)
    TA_REQ_SHARED(thread->get_lock()) {
  const SchedulerState& state = thread->scheduler_state();
  KTRACE_THREAD_WAKEUP("kernel:sched", target_cpu, thread->fxt_ref(),
                       ("weight", thread->IsIdle() ? kIdleWeight : state.weight()));
}

// Returns a delta value to additively update a predictor. Compares the given
// sample to the current value of the predictor and returns a delta such that
// the predictor either exponentially peaks or decays toward the sample. The
// rate of decay depends on the alpha parameter, while the rate of peaking
// depends on the beta parameter. The predictor is not permitted to become
// negative.
//
// A single-rate exponential moving average is updated as follows:
//
//   Sn = Sn-1 + a * (Yn - Sn-1)
//
// This function updates the exponential moving average using potentially
// different rates for peak and decay:
//
//   D  = Yn - Sn-1
//        [ Sn-1 + a * D      if D < 0
//   Sn = [
//        [ Sn-1 + b * D      if D >= 0
//
template <typename T, typename Alpha, typename Beta>
constexpr T PeakDecayDelta(T value, T sample, Alpha alpha, Beta beta) {
  const T delta = sample - value;
  return ktl::max<T>(delta >= 0 ? T{beta * delta} : T{alpha * delta}, -value);
}

// Reset this CPU's preemption timer to fire at |deadline|.
//
// |current_cpu| is the current CPU.
//
// |now| is the time that was latched when the CPU entered the scheduler.
//
// Must be called with interrupts disabled.
void PreemptReset(cpu_num_t current_cpu, zx_instant_mono_t now, zx_instant_mono_t deadline) {
  ktrace::Scope trace =
      LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "preempt_reset", ("now", now), ("deadline", deadline));
  // Setting a preemption time that's prior to the point at which the CPU
  // entered the scheduler indicates at worst a bug, or at best a wasted
  // reschedule.
  //
  // What do we mean by wasted reschedule?  When leaving the scheduler, if the
  // preemption timer was set to a point prior to entering the scheduler, it
  // will immediately fire once we've re-enabled interrupts.  When it fires,
  // we'll then re-enter the scheduler (provided that preemption is not
  // disabled) without running the the current ask for any appreciable amount of
  // time.  Instead, we should have set the preemption timer to a point that's
  // after the latched entry time and avoided an unnecessary interrupt and trip
  // through the scheduler.
  //
  // TODO(https://fxbug.dev/42182770): For now, simply count the number of times
  // this happens.  Once we have eliminated all causes of "preemption time in
  // the past" (whether they are bugs or simply missed optimization
  // opportunities) replace this counter with a DEBUG_ASSERT.
  //
  // Aside from simple bugs, how can we end up with a preemption time that's
  // earlier than the time at which we last entered the scheduler?  Consider the
  // case where the current thread, A, is blocking and the run queue contains
  // just one thread, B, that's deadline scheduled and whose finish time has
  // already elapsed.  In other words, B's finish time is earlier than the time
  // at which we last entered the scheduler.  In this case, we'll end up
  // scheduling B and setting the preemption timer to B's finish time.  Longer
  // term, we should consider resetting or reactivating all eligible tasks whose
  // finish times would be earlier than the point at which we entered the
  // scheduler.  See also |counter_deadline_past|.
  if (deadline < now) {
    kcounter_add(counter_preempt_past, 1);
  }

  percpu::Get(current_cpu).timer_queue.PreemptReset(deadline);
}

}  // anonymous namespace

void Scheduler::CountUpdateInTransition() { counter_update_in_transition.Add(1); }

fxt::StringRef<fxt::RefType::kId> Scheduler::ToStringRef(Placement placement) {
  using fxt::operator""_intern;
  switch (placement) {
    case Placement::Insertion:
      return "Insertion"_intern;
    case Placement::Adjustment:
      return "Adjustment"_intern;
    case Placement::Preemption:
      return "Preemption"_intern;
    case Placement::Migration:
      return "Migration"_intern;
    case Placement::Association:
      return "Association"_intern;
    default:
      return "Invalid"_intern;
  }
}

// Records details about the threads entering/exiting the run queues for various
// CPUs, as well as which task on each CPU is currently active. These events are
// used for trace analysis to compute statistics about overall utilization,
// taking CPU affinity into account.
inline void Scheduler::TraceThreadQueueEvent(const fxt::InternedString& name,
                                             const Thread* thread) const {
  // Traces marking the end of a queue/dequeue operation have arguments encoded
  // as follows:
  //
  // arg0[ 0..64] : TID
  //
  // arg1[ 0..15] : CPU availability mask.
  // arg1[16..19] : CPU_ID of the affected queue.
  // arg1[20..27] : Number of runnable tasks on this CPU after the queue event.
  // arg1[28..28] : 1 == fair, 0 == deadline
  // arg1[29..29] : 1 == eligible, 0 == ineligible
  // arg1[30..30] : 1 == idle thread, 0 == normal thread
  //
  if constexpr (SCHEDULER_QUEUE_TRACING_ENABLED) {
    const zx_instant_mono_t now = current_mono_time();  // TODO(johngro): plumb this in from above
    const bool fair = IsFairThread(thread);
    const bool eligible = fair || (thread->scheduler_state().start_time_ <= now);
    const size_t cnt = fair_run_queue_.size() + deadline_run_queue_.size() +
                       ((active_thread_ && !active_thread_->IsIdle()) ? 1 : 0);

    const uint64_t arg0 = thread->tid();
    const uint64_t arg1 =
        (thread->scheduler_state().GetEffectiveCpuMask(PeekActiveMask()) & 0xFFFF) |
        (ktl::clamp<uint64_t>(this_cpu_, 0, 0xF) << 16) |
        (ktl::clamp<uint64_t>(cnt, 0, 0xFF) << 20) | ((fair ? 1 : 0) << 28) |
        ((eligible ? 1 : 0) << 29) | ((thread->IsIdle() ? 1 : 0) << 30);

    KTrace::Probe(KTrace::Context::Cpu, name, arg0, arg1);
  }
}

void Scheduler::Dump(FILE* output_target, bool queue_state_only) {
  // We're about to acquire the |queue_lock_| and fprintf some things.
  // Depending on the FILE, calling fprintf may end up calling |DLog::Write|,
  // which may call |Event::Signal|, which may re-enter the Scheduler.  If that
  // happens while we're holding the |queue_lock_| that would be bad.
  // |DLog::Write| has a hack that allows it to defer the Signal operation when
  // there's an active chain lock transaction.  So even though we don't *need* a
  // chain lock transaction, we establish one anyway in order to leverage the
  // deferred Signal behavior and avoid a re-entrancy issue.
  //
  // TODO(https://fxbug.dev/331847876): Remove this hack once we have a better
  // solution to scheduler re-entrancy issues.
  const auto do_transaction =
      [&]() TA_REQ(chainlock_transaction_token) -> ChainLockTransaction::Result<> {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&queue_lock_, SOURCE_TAG};

    const SchedTime now = CurrentTime();
    const SchedTime variable_now = MonotonicToVariable(now);

    SchedWeight min_weight{0};
    SchedTime variable_eligible_time{SchedTime::Max()};
    if (!fair_run_queue_.is_empty()) {
      const Thread& root = *fair_run_queue_.croot();
      AssertInScheduler(root);
      min_weight =
          ktl::min(active_thread_weight_, root.scheduler_state().subtree_invariants_.min_weight);

      const Thread& front = fair_run_queue_.front();
      AssertInScheduler(front);
      variable_eligible_time = front.scheduler_state().start_time();
    }
    const SchedTime monotonic_eligible_time = VariableToMonotonic(variable_eligible_time);
    const SchedUtilization min_utilization =
        weight_total_ != 0 ? SchedUtilization{min_weight / weight_total_} : SchedUtilization{0};

    fprintf(output_target,
            "\tw_sum=%s w_min=%s T_v=%" PRId64 " count=%d ema_sum=%" PRId64
            "\n\tU_{c,sum}=%s U_{v,min}=%s"
            "\n\tnow=%" PRId64 " vnow=%" PRId64 " eligible=%" PRId64 " veligible=%" PRId64
            "\n\tmono_ref=%" PRId64 " var_ref=%" PRId64 " slope=%s\n",
            Format(weight_total_).c_str(), Format(min_weight).c_str(), fair_period_.raw_value(),
            runnable_task_count_, total_expected_runtime_ns_.raw_value(),
            Format(total_deadline_utilization_).c_str(), Format(min_utilization).c_str(),
            now.raw_value(), variable_now.raw_value(), monotonic_eligible_time.raw_value(),
            variable_eligible_time.raw_value(),
            fair_affine_transform_.monotonic_reference_time().raw_value(),
            fair_affine_transform_.variable_reference_time().raw_value(),
            Format(fair_affine_transform_.slope()).c_str());

    if (queue_state_only) {
      return ChainLockTransaction::Done;
    }

    if (active_thread_ != nullptr) {
      AssertInScheduler(*active_thread_);
      const SchedulerState& state = const_cast<const Thread*>(active_thread_)->scheduler_state();
      const EffectiveProfile& ep = state.effective_profile();
      if (ep.IsFair()) {
        fprintf(output_target,
                "\t-> name=%s weight=%s start=%" PRId64 " finish=%" PRId64 " ts=%" PRId64
                " ema=%" PRId64 " T_e=%" PRId64 "\n",
                active_thread_->name(), Format(ep.weight()).c_str(), state.start_time_.raw_value(),
                state.finish_time_.raw_value(), state.time_slice_ns_.raw_value(),
                state.expected_runtime_ns_.raw_value(), state.effective_period().raw_value());
      } else {
        fprintf(output_target,
                "\t-> name=%s deadline=(%" PRId64 ", %" PRId64 ") start=%" PRId64 " finish=%" PRId64
                " ts=%" PRId64 " ema=%" PRId64 " T_e=%" PRId64 "\n",
                active_thread_->name(), ep.deadline().capacity_ns.raw_value(),
                ep.deadline().deadline_ns.raw_value(), state.start_time_.raw_value(),
                state.finish_time_.raw_value(), state.time_slice_ns_.raw_value(),
                state.expected_runtime_ns_.raw_value(), state.effective_period().raw_value());
      }
    }

    for (const Thread& thread : deadline_run_queue_) {
      AssertInScheduler(thread);
      const SchedulerState& state = thread.scheduler_state();
      const EffectiveProfile& ep = state.effective_profile();
      fprintf(output_target,
              "\t   name=%s deadline=(%" PRId64 ", %" PRId64 ") start=%" PRId64 " finish=%" PRId64
              " ts=%" PRId64 " ema=%" PRId64 " T_e=%" PRId64 "\n",
              thread.name(), ep.deadline().capacity_ns.raw_value(),
              ep.deadline().deadline_ns.raw_value(), state.start_time_.raw_value(),
              state.finish_time_.raw_value(), state.remaining_time_slice_ns().raw_value(),
              state.expected_runtime_ns_.raw_value(), state.effective_period().raw_value());
    }

    for (const Thread& thread : fair_run_queue_) {
      AssertInScheduler(thread);
      const SchedulerState& state = thread.scheduler_state();
      const EffectiveProfile& ep = state.effective_profile();
      fprintf(output_target,
              "\t   name=%s weight=%s start=%" PRId64 " finish=%" PRId64 " ts=%" PRId64
              " ema=%" PRId64 " T_e=%" PRId64 "\n",
              thread.name(), Format(ep.weight()).c_str(),
              VariableToMonotonic(state.start_time_).raw_value(),
              VariableToMonotonic(state.finish_time_).raw_value(),
              state.remaining_time_slice_ns().raw_value(), state.expected_runtime_ns_.raw_value(),
              state.effective_period().raw_value());
    }
    return ChainLockTransaction::Done;
  };
  ChainLockTransaction::UntilDone(NoIrqSaveOption, CLT_TAG("Scheduler::Dump"), do_transaction);
}

void Scheduler::DumpActiveThread(FILE* output_target) {
  // See comment in |Scheduler::Dump|.
  //
  // TODO(https://fxbug.dev/331847876): Remove this hack once we have a better
  // solution to scheduler re-entrancy issues.
  const auto do_transaction =
      [&]() TA_REQ(chainlock_transaction_token) -> ChainLockTransaction::Result<> {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&queue_lock_, SOURCE_TAG};

    if (active_thread_ != nullptr) {
      AssertInScheduler(*active_thread_);
      fprintf(output_target, "thread: pid=%lu tid=%lu\n", active_thread_->pid(),
              active_thread_->tid());
      ThreadDispatcher* user_thread = active_thread_->user_thread();
      if (user_thread != nullptr) {
        ProcessDispatcher* process = user_thread->process();
        char name[ZX_MAX_NAME_LEN]{};
        [[maybe_unused]] zx_status_t status = process->get_name(name);
        DEBUG_ASSERT(status == ZX_OK);
        fprintf(output_target, "process: name=%s\n", name);
      }
    }
    return ChainLockTransaction::Done;
  };
  ChainLockTransaction::UntilDone(NoIrqSaveOption, CLT_TAG("Scheduler::DumpActiveThread"),
                                  do_transaction);
}

SchedWeight Scheduler::GetTotalWeight() const {
  Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
  return weight_total_;
}

size_t Scheduler::GetRunnableTasks() const {
  Guard<MonitoredSpinLock, IrqSave> guard{&queue_lock_, SOURCE_TAG};
  return static_cast<size_t>(runnable_task_count_);
}

// Performs an augmented binary search for the task with the earliest finish
// time that also has a start time equal to or later than the given eligible
// time. An optional predicate may be supplied to filter candidates based on
// additional conditions.
//
// The tree is ordered by start time and is augmented by maintaining an
// additional invariant: each task node in the tree stores the minimum finish
// time of its descendents, including itself, in addition to its own start and
// finish time. The combination of these three values permits traversinng the
// tree along a perfect partition of minimum finish times with eligible start
// times.
//
// See fbl/wavl_tree_best_node_observer.h for an explanation of how the
// augmented invariant is maintained.
Thread* Scheduler::FindEarliestEligibleThread(RunQueue* run_queue, SchedTime eligible_time) {
  return FindEarliestEligibleThread(run_queue, eligible_time,
                                    [](const auto& iter) { return true; });
}

template <typename Predicate>
Thread* Scheduler::FindEarliestEligibleThread(RunQueue* run_queue, SchedTime eligible_time,
                                              Predicate&& predicate) {
  const auto scheduler_state = [this](const Thread& t)
                                   TA_REQ(this->queue_lock_) -> const SchedulerState& {
    AssertInScheduler(t);
    return t.scheduler_state();
  };
  const auto subtree_state = [this](const Thread& t) TA_REQ(
                                 this->queue_lock_) -> const SchedulerState::SubtreeInvariants& {
    AssertInScheduler(t);
    return t.scheduler_state().subtree_invariants_;
  };

  // Early out if there is no eligible thread.
  if (run_queue->is_empty() || scheduler_state(run_queue->front()).start_time_ > eligible_time) {
    return nullptr;
  }

  // Deduces either Predicate& or const Predicate&, preserving the const
  // qualification of the predicate.
  decltype(auto) accept = ktl::forward<Predicate>(predicate);

  auto node = run_queue->root();
  auto subtree = run_queue->end();
  auto path = run_queue->end();

  // Descend the tree, with |node| following the path from the root to a leaf,
  // such that the path partitions the tree into two parts: the nodes on the
  // left represent eligible tasks, while the nodes on the right represent tasks
  // that are not eligible. Eligible tasks are both in the left partition and
  // along the search path, tracked by |path|.
  while (node) {
    const SchedulerState& node_state = scheduler_state(*node);

    if (node_state.start_time_ <= eligible_time) {
      if (!path || scheduler_state(*path).finish_time_ > node_state.finish_time_) {
        path = node;
      }

      if (auto left = node.left(); !subtree || (left && (subtree_state(*subtree).min_finish_time >
                                                         subtree_state(*left).min_finish_time))) {
        subtree = left;
      }

      node = node.right();
    } else {
      node = node.left();
    }
  }

  if (!subtree) {
    return path && accept(path) ? path.CopyPointer() : nullptr;
  }

  const SchedTime subtree_min_finish_time = subtree_state(*subtree).min_finish_time;
  if ((subtree_min_finish_time >= scheduler_state(*path).finish_time_) && accept(path)) {
    return path.CopyPointer();
  }

  // Find the node with the earliest finish time among the descendants of the
  // subtree with the smallest minimum finish time.
  node = subtree;
  do {
    if ((subtree_min_finish_time == scheduler_state(*node).finish_time_) && accept(node)) {
      return node.CopyPointer();
    }

    if (auto left = node.left();
        left && (subtree_state(*node).min_finish_time == subtree_state(*left).min_finish_time)) {
      node = left;
    } else {
      node = node.right();
    }
  } while (node);

  return nullptr;
}

Scheduler* Scheduler::Get() { return Get(arch_curr_cpu_num()); }

Scheduler* Scheduler::Get(cpu_num_t cpu) { return &percpu::Get(cpu).scheduler; }

void Scheduler::InitializeThread(Thread* thread, const SchedulerState::BaseProfile& profile) {
  new (&thread->scheduler_state()) SchedulerState{profile};
  thread->scheduler_state().expected_runtime_ns_ =
      profile.IsFair() ? kMinimumFairCapacity : profile.deadline.capacity_ns;
}

// Initialize the first thread to run on the current CPU.  Called from
// thread_construct_first, this method will initialize the thread's scheduler
// state, then mark the thread as being "active" in its cpu's scheduler.
void Scheduler::InitializeFirstThread(Thread* thread) {
  cpu_num_t current_cpu = arch_curr_cpu_num();

  // Construct our scheduler state and assign a "priority"
  InitializeThread(thread, SchedulerState::BaseProfile{HIGHEST_PRIORITY});

  // Fill out other details about the thread, making sure to assign it to the
  // current CPU with hard affinity.
  SchedulerState& state = thread->scheduler_state();
  state.state_ = THREAD_RUNNING;
  state.curr_cpu_ = current_cpu;
  state.last_cpu_ = current_cpu;
  state.hard_affinity_ = cpu_num_to_mask(current_cpu);

  // Finally, make sure that the thread is the active thread for the scheduler,
  // and that the weight_total bookkeeping is accurate.
  {
    Scheduler* scheduler = Get(current_cpu);
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler->queue_lock_, SOURCE_TAG};
    scheduler->AssertInScheduler(*thread);

    SchedulerQueueState& queue_state = thread->scheduler_queue_state();
    queue_state.OnInsert();
    scheduler->active_thread_ = thread;

    scheduler->weight_total_ = state.effective_profile_.weight();
    scheduler->runnable_task_count_++;
    scheduler->UpdateTotalExpectedRuntime(state.expected_runtime_ns_);
  }
}

// Remove the impact of a CPUs first thread from the scheduler's bookkeeping.
//
// During initial startup, threads are not _really_ being scheduled, yet they
// can still do things like obtain locks and block, resulting in profile
// inheritance.  In order to hold the scheduler's bookkeeping invariants, we
// assign these threads a fair weight, and include it in the total fair weight
// tracked by the scheduler instance.  When the thread either becomes the idle
// thread (as the boot CPU first thread does), or exits (as secondary CPU first
// threads do), it is important that we remove this weight from the total
// bookkeeping.  However, this is not as simple as just changing the thread's
// weight via ChangeWeight, as idle threads are special cases who contribute no
// weight to the total.
//
// So, this small method simply fixes up the bookkeeping before allowing the
// thread to move on to become the idle thread (boot CPU), or simply exiting
// (secondary CPU).
void Scheduler::RemoveFirstThread(Thread* thread) {
  cpu_num_t current_cpu = arch_curr_cpu_num();
  Scheduler* scheduler = Get(current_cpu);
  SchedulerState& state = thread->scheduler_state();

  // Since this is becoming an idle thread, it must have been one of the CPU's
  // first threads.  It should already be bound to this core with hard affinity.
  // Assert this.
  DEBUG_ASSERT(state.last_cpu_ == current_cpu);
  DEBUG_ASSERT(state.curr_cpu_ == current_cpu);
  DEBUG_ASSERT(state.hard_affinity_ == cpu_num_to_mask(current_cpu));

  {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler->queue_lock_, SOURCE_TAG};
    scheduler->AssertInScheduler(*thread);
    SchedulerQueueState& queue_state = thread->scheduler_queue_state();

    // We are becoming the idle thread.  We should currently be running with a
    // fair (not deadline) profile, and we should not be holding any locks
    // (therefore, we should not be inheriting any profile pressure).
    DEBUG_ASSERT(state.base_profile_.IsFair());
    DEBUG_ASSERT(state.inherited_profile_values_.total_weight == SchedWeight{0});
    DEBUG_ASSERT(state.inherited_profile_values_.uncapped_utilization == SchedUtilization{0});

    // We should also be the currently active thread on this core, but no
    // longer.  We are about to either exit, or "UnblockIdle".
    DEBUG_ASSERT(scheduler->active_thread_ == thread);
    DEBUG_ASSERT(scheduler->runnable_task_count_ > 0);
    queue_state.OnRemove(INVALID_CPU);  // Set active to false.
    scheduler->active_thread_ = nullptr;
    scheduler->weight_total_ -= state.effective_profile_.weight();
    scheduler->runnable_task_count_--;
    scheduler->UpdateTotalExpectedRuntime(-state.expected_runtime_ns_);

    state.base_profile_.fair.weight = SchedulerState::ConvertPriorityToWeight(IDLE_PRIORITY);
    state.effective_profile_.MarkBaseProfileChanged();
    state.RecomputeEffectiveProfile();
  }
}

// Removes the thread at the head of the first eligible run queue. If there is
// an eligible deadline thread, it takes precedence over available fair
// threads. If there is no eligible work, attempt to steal work from other busy
// CPUs.
Scheduler::DequeueResult Scheduler::DequeueThread(
    SchedTime now, Guard<MonitoredSpinLock, NoIrqSave>& queue_guard) {
  percpu& self = percpu::Get(this_cpu_);
  if (self.idle_power_thread.pending_power_work()) {
    return &self.idle_power_thread.thread();
  }

  if (IsDeadlineThreadEligible(now)) {
    return DequeueDeadlineThread(now);
  }
  if (likely(!fair_run_queue_.is_empty())) {
    return DequeueFairThread(now);
  }

  // Release the queue lock while attempting to steal work, leaving IRQs
  // disabled.  Latch our scale up factor to use while determining whether or
  // not we can steal a given thread before we drop our lock.
  // TODO(eieio): Since the performance scale is CPU-local and not accessed
  // across processors, it should be unnecessary to latch the value. Change the
  // annotations to allow this.
  DequeueResult result;
  SchedPerformanceScale scale_up_factor = performance_scale_reciprocal_.load();
  queue_guard.CallUnlocked([&] {
    ChainLockTransaction::AssertActive();
    result = StealWork(now, scale_up_factor);
  });

  if (result) {
    return result;
  }

  return &self.idle_power_thread.thread();
}

// Attempts to steal work from other busy CPUs and move it to the local run
// queues. Returns a pointer to the stolen thread that is now associated with
// the local Scheduler instance, or nullptr is no work was stolen.
Scheduler::DequeueResult Scheduler::StealWork(SchedTime now,
                                              SchedPerformanceScale scale_up_factor) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "StealWork");

  const cpu_num_t current_cpu = this_cpu();
  const cpu_mask_t current_cpu_mask = cpu_num_to_mask(current_cpu);
  const cpu_mask_t active_cpu_mask = PeekActiveMask();

  const CpuSearchSet& search_set = percpu::Get(current_cpu).search_set;
  for (const auto& entry : search_set.const_iterator()) {
    if (entry.cpu != current_cpu && active_cpu_mask & cpu_num_to_mask(entry.cpu)) {
      Scheduler* const scheduler = Get(entry.cpu);

      // Only steal across clusters if the target is above the load threshold.
      if (cluster() != entry.cluster &&
          scheduler->predicted_queue_time_ns() <= kInterClusterThreshold) {
        continue;
      }

      Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler->queue_lock_, SOURCE_TAG};

      // Only attempt to steal from CPUs that have more than one task, avoiding
      // unnecessary overhead when a task was just added to an idle CPU but the
      // CPU has not started running the new task (i.e. when this CPU wins the
      // race to acquire the target CPU's queue lock after the singular task is
      // queued).
      if (scheduler->runnable_task_count_ <= 1) {
        continue;
      }

      // Note that in the lambdas below we will be making use of both the
      // MarkHasSchedulerAccess (but only on |queue|) and
      // MarkHasOwnedThreadAccess.  We just acquired |queue|'s queue lock, and
      // each thread we will be examining is a member of one of |queue|'s run
      // queues (either fair or deadline).  This should satisfy the requirements
      // of both no-op checks.

      // Returns true if the given thread can run on this CPU.  Static analysis
      // needs to be disabled, but this should be fine.  We only need R/O access
      // to the thread's scheduler state, which we have because we are holding
      // the queue lock for a thread which belongs to that queue (see below).
      const auto check_affinity = [current_cpu_mask,
                                   active_cpu_mask](const Thread& thread) -> bool {
        MarkHasOwnedThreadAccess(thread);
        return current_cpu_mask & thread.scheduler_state().GetEffectiveCpuMask(active_cpu_mask);
      };

      // Common routine for stealing from a run queue.
      const auto steal_from_queue = [current_cpu, check_affinity, scheduler, now](
                                        RunQueue& run_queue,
                                        const auto& predicate) -> DequeueResult {
        ktrace::Scope trace_steal = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_steal");

        DEBUG_ASSERT((&run_queue == &scheduler->fair_run_queue_) ||
                     (&run_queue == &scheduler->deadline_run_queue_));
        MarkHasSchedulerAccess(*scheduler);
        const bool is_fair_run_queue = &run_queue == &scheduler->fair_run_queue_;

        // Convert the monotonic eligible time to variable time and potentially
        // snap the virtual timeline forward when stealing from the fair queue.
        SchedTime eligible_time = now;
        if (is_fair_run_queue) {
          scheduler->MonotonicToVariableInPlace(eligible_time);

          if (!run_queue.is_empty()) {
            const Thread& earliest_thread = run_queue.front();
            MarkHasOwnedThreadAccess(earliest_thread);

            const SchedTime earliest_start = earliest_thread.scheduler_state().start_time();
            if (eligible_time < earliest_start) {
              counter_fair_timeline_snap_count.Add(1);
              counter_fair_timeline_snap_time.Add(Round<int64_t>(earliest_start - eligible_time));

              scheduler->fair_affine_transform_.Snap(now, earliest_start);
              eligible_time = earliest_start;
            }
          }
        }

        Thread* thread =
            scheduler->FindEarliestEligibleThread(&run_queue, eligible_time, predicate);
        if (!thread) {
          return nullptr;
        }

        MarkHasOwnedThreadAccess(*thread);

        ktl::ignore = check_affinity;  // Silence compiler when debug asserts are disabled.
        DEBUG_ASSERT(check_affinity(*thread));
        DEBUG_ASSERT(!thread->has_migrate_fn());
        DEBUG_ASSERT(thread->disposition() == Disposition::Enqueued);

        // Remove the thread from the source run queue and record that it was
        // stolen by this CPU.
        run_queue.erase(*thread);
        scheduler->Remove(now, thread, current_cpu);
        scheduler->TraceThreadQueueEvent("tqe_deque_steal_work"_intern, thread);
        DEBUG_ASSERT(thread->disposition() == Disposition::Stolen);

        if (is_fair_run_queue) {
          const SchedulerState& state = const_cast<const Thread*>(thread)->scheduler_state();
          const SchedTime mono_start_time = scheduler->VariableToMonotonic(state.start_time());
          const SchedTime mono_finish_time = scheduler->VariableToMonotonic(state.finish_time());
          return DequeueResult{thread, mono_start_time, mono_finish_time};
        }

        return thread;
      };

      // Returns true if the given thread in the run queue meets the criteria to
      // run on this CPU.
      const auto deadline_predicate = [check_affinity, scale_up_factor](const auto& iter) -> bool {
        const Thread& thread = *iter;
        MarkHasOwnedThreadAccess(thread);

        const SchedulerState& state = thread.scheduler_state();
        const EffectiveProfile& ep = state.effective_profile_;
        const SchedUtilization scaled_utilization = ep.deadline().utilization * scale_up_factor;
        const bool is_scheduleable = scaled_utilization <= kThreadUtilizationMax;

        return check_affinity(thread) && is_scheduleable && !thread.has_migrate_fn();
      };

      // Attempt to find a deadline thread that can run on this CPU.
      if (DequeueResult result =
              steal_from_queue(scheduler->deadline_run_queue_, deadline_predicate)) {
        return result;
      }

      // Returns true if the given thread in the run queue meets the criteria to
      // run on this CPU.
      const auto fair_predicate = [check_affinity](const auto& iter) -> bool {
        const Thread& thread = *iter;
        MarkHasOwnedThreadAccess(thread);
        return check_affinity(thread) && !thread.has_migrate_fn();
      };

      // Attempt to find a fair thread that can run on this CPU.
      if (DequeueResult result = steal_from_queue(scheduler->fair_run_queue_, fair_predicate)) {
        return result;
      }
    }
  }

  return nullptr;
}

Scheduler::DequeueResult Scheduler::DequeueFairThread(SchedTime monotonic_eligible_time) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "dequeue_fair_thread");

  // Convert monotonic eligible time to variable time to compare with fair run
  // queue values.
  SchedTime variable_eligible_time =
      fair_affine_transform_.MonotonicToVariable(monotonic_eligible_time);

  // Use MarkHasOwnedThreadAccess to satisfy static annotations for shared
  // access to the thread's COVs and exclusive access to the thread's SOVs. This
  // is acceptable because the queue lock is required by this method and the
  // thread is in a queue protectect by the same lock.
  const auto& earliest_thread = fair_run_queue_.front();
  MarkHasOwnedThreadAccess(earliest_thread);

  // If there are no eligible fair threads, snap the variable timeline to the
  // earliest start time.
  const SchedTime earliest_start = earliest_thread.scheduler_state().start_time_;
  if (variable_eligible_time < earliest_start) {
    counter_fair_timeline_snap_count.Add(1);
    counter_fair_timeline_snap_time.Add(Round<int64_t>(earliest_start - variable_eligible_time));

    fair_affine_transform_.Snap(monotonic_eligible_time, earliest_start);
    variable_eligible_time = earliest_start;
  }

  Thread* const eligible_thread =
      FindEarliestEligibleThread(&fair_run_queue_, variable_eligible_time);
  DEBUG_ASSERT_MSG(
      eligible_thread != nullptr,
      "vearliest_start=%" PRId64 ", veligible_time=%" PRId64 " , vstart_time=%" PRId64
      ", vfinish_time=%" PRId64 ", vmin_finish_time=%" PRId64 "!",
      earliest_start.raw_value(), variable_eligible_time.raw_value(),
      earliest_thread.scheduler_state().start_time_.raw_value(),
      earliest_thread.scheduler_state().finish_time_.raw_value(),
      earliest_thread.scheduler_state().subtree_invariants_.min_finish_time.raw_value());

  // Same argument as before for the use of the no-op assert.  We are holding
  // the queue lock, and we just found this thread in the scheduler.
  MarkHasOwnedThreadAccess(*eligible_thread);
  fair_run_queue_.erase(*eligible_thread);
  TraceThreadQueueEvent("tqe_deque_fair"_intern, eligible_thread);
  DEBUG_ASSERT(eligible_thread->disposition() == Disposition::Associated);

  const SchedulerState& state = const_cast<const Thread*>(eligible_thread)->scheduler_state();
  const SchedTime mono_start_time_ns =
      fair_affine_transform_.VariableToMonotonic(state.start_time());
  const SchedTime mono_finish_time_ns =
      fair_affine_transform_.VariableToMonotonic(state.finish_time());

  trace = KTRACE_END_SCOPE(("var_start_time", state.start_time().raw_value()),
                           ("var_finish_time", state.finish_time().raw_value()),
                           ("mono_start_time", mono_start_time_ns.raw_value()),
                           ("mono_finish_time", mono_finish_time_ns.raw_value()));

  return DequeueResult{eligible_thread, mono_start_time_ns, mono_finish_time_ns};
}

Scheduler::DequeueResult Scheduler::DequeueDeadlineThread(SchedTime eligible_time) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "dequeue_deadline_thread");

  Thread* const eligible_thread = FindEarliestEligibleThread(&deadline_run_queue_, eligible_time);
  DEBUG_ASSERT_MSG(eligible_thread != nullptr, "eligible_time=%" PRId64, eligible_time.raw_value());

  // Use MarkHasOwnedThreadAccess to satisfy static annotations for shared
  // access to the thread's COVs and exclusive access to the thread's SOVs. This
  // is acceptable because the queue lock is required by this method and the
  // thread is in a queue protectect by the same lock.
  MarkHasOwnedThreadAccess(*eligible_thread);
  deadline_run_queue_.erase(*eligible_thread);
  TraceThreadQueueEvent("tqe_deque_deadline"_intern, eligible_thread);
  DEBUG_ASSERT(eligible_thread->disposition() == Disposition::Associated);

  const SchedulerState& state = const_cast<const Thread*>(eligible_thread)->scheduler_state();
  if (state.finish_time_ <= eligible_time) {
    kcounter_add(counter_deadline_past, 1);
  }
  trace = KTRACE_END_SCOPE(("start time", Round<uint64_t>(state.start_time_)),
                           ("finish time", Round<uint64_t>(state.finish_time_)));
  return eligible_thread;
}

Thread* Scheduler::FindEarlierFairThread(SchedTime eligible_time, SchedTime finish_time) {
  // Convert monotonic to variable times to compare with fair run queue values.
  const SchedTime variable_eligible_time = MonotonicToVariable(eligible_time);
  const SchedTime variable_finish_time = MonotonicToVariable(finish_time);

  Thread* const eligible_thread =
      FindEarliestEligibleThread(&fair_run_queue_, variable_eligible_time);

  if (eligible_thread != nullptr) {
    // Use MarkHasOwnedThreadAccess to satisfy static annotations for shared
    // access to the thread's COVs and exclusive access to the thread's SOVs.
    // This is acceptable because the queue lock is required by this method and
    // the thread is in a queue protectect by the same lock.
    MarkHasOwnedThreadAccess(*eligible_thread);
    if (const_cast<const Thread*>(eligible_thread)->scheduler_state().finish_time() <
        variable_finish_time) {
      return eligible_thread;
    }
  }

  return nullptr;
}

Thread* Scheduler::FindEarlierDeadlineThread(SchedTime eligible_time, SchedTime finish_time) {
  Thread* const eligible_thread = FindEarliestEligibleThread(&deadline_run_queue_, eligible_time);

  if (eligible_thread != nullptr) {
    // Use MarkHasOwnedThreadAccess to satisfy static annotations for shared
    // access to the thread's COVs and exclusive access to the thread's SOVs.
    // This is acceptable because the queue lock is required by this method and
    // the thread is in a queue protectect by the same lock.
    MarkHasOwnedThreadAccess(*eligible_thread);
    if (const_cast<const Thread*>(eligible_thread)->scheduler_state().finish_time() < finish_time) {
      return eligible_thread;
    }
  }

  return nullptr;
}

// Returns the time that the next deadline task will become eligible or infinite
// if there are no ready deadline tasks.
SchedTime Scheduler::GetNextEligibleTime() {
  if (deadline_run_queue_.is_empty()) {
    return SchedTime{ZX_TIME_INFINITE};
  }

  // Use MarkHasOwnedThreadAccess to satisfy static annotations for shared
  // access to the thread's COVs and exclusive access to the thread's SOVs. This
  // is acceptable because the queue lock is required by this method and the
  // thread is in a queue protectect by the same lock.
  const Thread& front = deadline_run_queue_.front();
  MarkHasOwnedThreadAccess(front);
  return front.scheduler_state().start_time_;
}

Scheduler::DequeueResult Scheduler::DequeueEarlierFairThread(SchedTime eligible_time,
                                                             SchedTime finish_time) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "dequeue_earlier_fair_thread");
  Thread* const eligible_thread = FindEarlierFairThread(eligible_time, finish_time);

  if (eligible_thread != nullptr) {
    // Use MarkHasOwnedThreadAccess to satisfy static annotations for shared
    // access to the thread's COVs and exclusive access to the thread's SOVs.
    // This is acceptable because the queue lock is required by this method and
    // the thread is in a queue protectect by the same lock.
    MarkHasOwnedThreadAccess(*eligible_thread);
    fair_run_queue_.erase(*eligible_thread);
    TraceThreadQueueEvent("tqe_deque_earlier_fair"_intern, eligible_thread);
    DEBUG_ASSERT(eligible_thread->disposition() == Disposition::Associated);

    const SchedulerState& state = const_cast<const Thread*>(eligible_thread)->scheduler_state();
    const SchedTime mono_start_time_ns =
        fair_affine_transform_.VariableToMonotonic(state.start_time());
    const SchedTime mono_finish_time_ns =
        fair_affine_transform_.VariableToMonotonic(state.finish_time());

    return DequeueResult{eligible_thread, mono_start_time_ns, mono_finish_time_ns};
  }

  return nullptr;
}

Scheduler::DequeueResult Scheduler::DequeueEarlierDeadlineThread(SchedTime eligible_time,
                                                                 SchedTime finish_time) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "dequeue_earlier_deadline_thread");
  Thread* const eligible_thread = FindEarlierDeadlineThread(eligible_time, finish_time);

  if (eligible_thread != nullptr) {
    // Use MarkHasOwnedThreadAccess to satisfy static annotations for shared
    // access to the thread's COVs and exclusive access to the thread's SOVs.
    // This is acceptable because the queue lock is required by this method and
    // the thread is in a queue protectect by the same lock.
    MarkHasOwnedThreadAccess(*eligible_thread);
    deadline_run_queue_.erase(*eligible_thread);
    TraceThreadQueueEvent("tqe_deque_earlier_deadline"_intern, eligible_thread);
    DEBUG_ASSERT(eligible_thread->disposition() == Disposition::Associated);
  }

  return eligible_thread;
}

bool Scheduler::NeedsMigration(Thread* thread) {
  const cpu_num_t current_cpu = arch_curr_cpu_num();
  const cpu_mask_t current_cpu_mask = cpu_num_to_mask(current_cpu);
  const cpu_mask_t active_mask = PeekActiveMask();
  // Threads that are not ready should not be migrated.
  if (thread->state() != THREAD_READY) {
    return false;
  }

  // If we have no active CPUs, then we cannot migrate this thread, so return false.
  if (active_mask == 0) {
    return false;
  }

  // We need to migrate if the set of CPUs this thread can run on does not include the current CPU.
  return (thread->scheduler_state().GetEffectiveCpuMask(active_mask) & current_cpu_mask) == 0;
}

Scheduler::DequeueResult Scheduler::EvaluateNextThread(
    SchedTime now, Thread* current_thread, bool current_is_migrating,
    Guard<MonitoredSpinLock, NoIrqSave>& queue_guard) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "find_thread");

  const bool is_idle = current_thread->IsIdle();
  const bool is_runnable = current_thread->state() == THREAD_READY;
  const bool is_deadline = IsDeadlineThread(current_thread);

  if (is_runnable && !is_idle && !current_is_migrating) {
    // Start a new activation, if necessary, before comparing with other threads.
    UpdateActivation(current_thread, now);

    // Determine eligibility after updating the activation period.
    const bool is_eligible = current_thread->scheduler_state().start_time() <= now;

    if (is_deadline && !is_eligible) {
      // Ineligible deadline threads must wait until they become eligible again.
      return DequeueThread(now, queue_guard);
    }

    if (IsDeadlineThreadEligible(now)) {
      if (is_deadline) {
        // The current thread is deadline and eligible. Select the eligible
        // thread with the earliest finish time, which may still be the current
        // thread.
        const SchedTime finish_time = current_thread->scheduler_state().finish_time();
        if (DequeueResult earlier_thread_result = DequeueEarlierDeadlineThread(now, finish_time)) {
          return earlier_thread_result;
        }

        // The current thread is eligible and has the earliest finish time.
        return current_thread;
      }

      // The current thread is fair. An eligible deadline thread should preempt
      // the current thread.
      return DequeueDeadlineThread(now);
    }

    if (!is_deadline) {
      if (is_eligible) {
        if (IsFairThreadEligible(now)) {
          // Select the eligible fair thread with the earliest finish time,
          // which may still be the current thread.
          const SchedTime finish_time = current_thread->scheduler_state().finish_time();
          if (DequeueResult earlier_thread_result = DequeueEarlierFairThread(now, finish_time)) {
            return earlier_thread_result;
          }
        }

        // The current fair thread has the earliest eligible finish time.
        return current_thread;
      }

      if (IsFairThreadEligible(now)) {
        // Select an eligible fair thread.
        return DequeueFairThread(now);
      }

      // No fair threads are currently eligible. Temporarily add the current
      // thread to the run queue, before snapping the variable timeline to the
      // earliest eligible time, to simplify selection in the updated timeline.
      // Use Placement::Adjustment to avoid emitting inconsistent trace events.
      QueueThread(current_thread, Placement::Adjustment);

      // Snap the variable timeline to the earliest eligible time and select the
      // thread with the earliest eligible finish time.
      DequeueResult next_thread = DequeueFairThread(now);
      DEBUG_ASSERT(next_thread);

      // If the current thread is not selected to run again, it must be removed
      // from the run queue to avoid being stolen during lock juggling. It will
      // be returned to the run queue after the lock juggling is completed and
      // before switching to the next thread.
      if (next_thread.thread() != current_thread) {
        MarkHasOwnedThreadAccess(*current_thread);
        EraseFromQueue(current_thread);
      }

      return next_thread;
    }

    // The current deadline thread has remaining time and no eligible contender.
    DEBUG_ASSERT(is_deadline);
    DEBUG_ASSERT(is_eligible);
    return current_thread;
  }

  // The thread is either no longer runnable, migrating, or the idle thread.
  return DequeueThread(now, queue_guard);
}

cpu_num_t Scheduler::FindTargetCpu(Thread* thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "find_target");

  // Determine the set of CPUs the thread is allowed to run on.
  const cpu_num_t current_cpu = arch_curr_cpu_num();
  const cpu_mask_t active_mask = PeekActiveMask();
  const SchedulerState& thread_state = const_cast<const Thread*>(thread)->scheduler_state();
  const cpu_mask_t available_mask = thread_state.GetEffectiveCpuMask(active_mask);
  DEBUG_ASSERT_MSG(available_mask != 0,
                   "thread=%s affinity=%#x soft_affinity=%#x active=%#x "
                   "idle=%#x arch_ints_disabled=%d",
                   thread->name(), thread_state.hard_affinity_, thread_state.soft_affinity_,
                   active_mask, PeekIdleMask(), arch_ints_disabled());

  LOCAL_KTRACE(DETAILED, "target_mask", ("online", mp_get_online_mask()), ("active", active_mask));

  // Find the best target CPU starting at the last CPU the task ran on, if any.
  // Alternatives are considered in order of best to worst potential cache
  // affinity.
  const cpu_num_t last_cpu = thread_state.last_cpu_;
  const cpu_num_t starting_cpu = last_cpu != INVALID_CPU ? last_cpu : current_cpu;
  const CpuSearchSet& search_set = percpu::Get(starting_cpu).search_set;

  // TODO(https://fxbug.dev/42180608): Working on isolating a low-frequency panic due to
  // apparent memory corruption of percpu intersecting CpuSearchSet, resulting
  // in an invalid entry pointer and/or entry count. Adding an assert to help
  // catch the corruption and include additional context. This assert is enabled
  // in non-eng builds, however, the small impact is acceptable for production.
  ASSERT_MSG(search_set.cpu_count() <= SMP_MAX_CPUS,
             "current_cpu=%u starting_cpu=%u active_mask=%x thread=%p search_set=%p cpu_count=%zu "
             "entries=%p",
             current_cpu, starting_cpu, active_mask, &thread, &search_set, search_set.cpu_count(),
             search_set.const_iterator().data());

  // A combination of a scheduler and its latched scale-up factor.  Whenever we
  // consider a scheduler, we observe its current scale factor exactly once, so
  // that we can be sure that the value remains consistent for all of the
  // comparisons we do in |compare| and |is_sufficient| (below).  Note that we
  // skip obtaining the queue lock when we observe the performance scale factor.
  //
  // Finding a target CPU is a best effort heuristic, and we would really rather
  // not be fighting over scheduler's queue lock while we do it.  Other places
  // in the scheduler code use the queue lock to protect these scale factors,
  // but here it should be sufficient to simply atomically load the scale
  // factor, which is also atomically written during its only update location in
  // RescheduleCommon, which avoids formal C++ data races.  All of these atomic
  // accesses are relaxed, however, meaning that their values have no defined
  // ordering relationship relative to other non-locked queue values (like
  // predicted queue time).
  struct CandidateQueue {
    CandidateQueue() = default;
    explicit CandidateQueue(const Scheduler* s)
        : queue{s}, scale_up_factor{LatchScaleUpFactor(s)} {}

    const Scheduler* queue{nullptr};
    SchedPerformanceScale scale_up_factor{1};

    static SchedPerformanceScale LatchScaleUpFactor(const Scheduler* s)
        TA_NO_THREAD_SAFETY_ANALYSIS {
      return s->performance_scale_reciprocal_.load();
    }
  };

  // Compares candidate queues and returns true if |queue_a| is a better
  // alternative than |queue_b|. This is used by the target selection loop to
  // determine whether the next candidate is better than the current target.
  const auto compare = [&thread_state](const CandidateQueue& a, const CandidateQueue& b) {
    const SchedDuration a_predicted_queue_time_ns = a.queue->predicted_queue_time_ns();
    const SchedDuration b_predicted_queue_time_ns = b.queue->predicted_queue_time_ns();

    ktrace::Scope trace_compare = LOCAL_KTRACE_BEGIN_SCOPE(
        DETAILED, "compare", ("predicted queue time a", Round<uint64_t>(a_predicted_queue_time_ns)),
        ("predicted queue time b", Round<uint64_t>(b_predicted_queue_time_ns)));

    const EffectiveProfile& ep = thread_state.effective_profile_;
    if (ep.IsFair()) {
      // CPUs in the same logical cluster are considered equivalent in terms of
      // cache affinity. Choose the least loaded among the members of a cluster.
      if (a.queue->cluster() == b.queue->cluster()) {
        ktl::pair a_pair{a_predicted_queue_time_ns, a.queue->predicted_deadline_utilization()};
        ktl::pair b_pair{b_predicted_queue_time_ns, b.queue->predicted_deadline_utilization()};
        return a_pair < b_pair;
      }

      // Only consider crossing cluster boundaries if the current candidate is
      // above the threshold.
      return b_predicted_queue_time_ns > kInterClusterThreshold &&
             a_predicted_queue_time_ns < b_predicted_queue_time_ns;
    } else {
      const SchedUtilization utilization = ep.deadline().utilization;
      const SchedUtilization scaled_utilization_a = utilization * a.scale_up_factor;
      const SchedUtilization scaled_utilization_b = utilization * b.scale_up_factor;

      ktl::pair a_pair{scaled_utilization_a, a_predicted_queue_time_ns};
      ktl::pair b_pair{scaled_utilization_b, b_predicted_queue_time_ns};
      ktl::pair a_prime{a.queue->predicted_deadline_utilization(), a_pair};
      ktl::pair b_prime{b.queue->predicted_deadline_utilization(), b_pair};
      return a_prime < b_prime;
    }
  };

  // Determines whether the current target is sufficiently good to terminate the
  // selection loop.
  const auto is_sufficient = [&thread_state](const CandidateQueue& q) {
    const SchedDuration candidate_queue_time_ns = q.queue->predicted_queue_time_ns();

    ktrace::Scope trace_is_sufficient = LOCAL_KTRACE_BEGIN_SCOPE(
        DETAILED, "is_sufficient",
        ("intra cluster threshold", Round<uint64_t>(kIntraClusterThreshold)),
        ("candidate q.queue time", Round<uint64_t>(candidate_queue_time_ns)));

    const EffectiveProfile& ep = thread_state.effective_profile_;
    if (ep.IsFair()) {
      return candidate_queue_time_ns <= kIntraClusterThreshold;
    }

    const SchedUtilization predicted_utilization = q.queue->predicted_deadline_utilization();
    const SchedUtilization utilization = ep.deadline().utilization;
    const SchedUtilization scaled_utilization = utilization * q.scale_up_factor;

    return candidate_queue_time_ns <= kIntraClusterThreshold &&
           scaled_utilization <= kThreadUtilizationMax &&
           predicted_utilization + scaled_utilization <= kCpuUtilizationLimit;
  };

  // Loop over the search set for CPU the task last ran on to find a suitable
  // target.
  cpu_num_t target_cpu = INVALID_CPU;
  CandidateQueue target_queue{};

  for (const auto& entry : search_set.const_iterator()) {
    const cpu_num_t candidate_cpu = entry.cpu;
    const bool candidate_available = available_mask & cpu_num_to_mask(candidate_cpu);
    const CandidateQueue candidate_queue{Get(candidate_cpu)};

    if (candidate_available &&
        (target_queue.queue == nullptr || compare(candidate_queue, target_queue))) {
      target_cpu = candidate_cpu;
      target_queue = candidate_queue;

      // Stop searching at the first sufficiently unloaded CPU.
      if (is_sufficient(target_queue)) {
        break;
      }
    }
  }

  DEBUG_ASSERT(target_cpu != INVALID_CPU);
  trace = KTRACE_END_SCOPE(("last_cpu", last_cpu), ("target_cpu", target_cpu));
  return target_cpu;
}

void Scheduler::IncFindTargetCpuRetriesKcounter() { counter_find_target_cpu_retries.Add(1u); }

void Scheduler::ProcessSaveStateList(SchedTime now) {
  DEBUG_ASSERT(arch_ints_disabled());

  // Move the save_state_list_ to a local, stack allocated list. This allows us to relinquish the
  // queue_lock_ for the rest of the method.
  Thread::SaveStateList local_save_state_list;
  {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&queue_lock_, SOURCE_TAG};
    if (save_state_list_.is_empty()) {
      return;
    }
    local_save_state_list = ktl::move(save_state_list_);
  }

  ChainLockTransaction& active_clt = ChainLockTransaction::ActiveRef();
  active_clt.Restart(CLT_TAG("Scheduler::ProcessSaveStateList (restart)"));

  cpu_mask_t cpus_to_reschedule_mask = 0;
  while (!local_save_state_list.is_empty()) {
    Thread* to_migrate = local_save_state_list.pop_front();
    // Acquire the thread's lock without backing off using an explicit retry
    // loop because the current thread's lock is held and ChainLock::Acquire
    // asserts that no chain locks are currently held to avoid misuse in the
    // general case. We also currently hold the special "sched token", meaning
    // that we will win arbitration against any other chainlock transaction.
    //
    // This is safe to do here because we can guarantee that no other scheduler
    // holds or is attempting to acquire the locks of any of the threads in this
    // save_state_list_. The only context in which schedulers attempt to acquire
    // the locks of threads in other schedulers is during the `StealWork`
    // routine. However, that routine only looks for threads in the RunQueues
    // (not save_state_lists) and also does not steal threads that have
    // migration functions. Therefore, it is impossible for another scheduler to
    // be fighting over this thread's lock.
    while (!to_migrate->get_lock().TryAcquire(ChainLock::AllowFinalized::No,
                                              ChainLock::RecordBackoff::No)) {
      arch::Yield();
    }

    // We now have the thread's lock, so run the migrate function.
    to_migrate->CallMigrateFnLocked(Thread::MigrateStage::Save);

    // TODO(rudymathu): Threads placed in the save_state_list_ via an Unblock
    // operation already performed a `FindTargetCpu` call. In a future
    // optimization, we should be able to reuse that value here and avoid
    // another CPU search, provided that the CPU is still online and load
    // balancing constraints still hold.
    const cpu_num_t target_cpu =
        FindActiveSchedulerForThread(to_migrate, [now](Thread* thread, Scheduler* target) {
          MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
          target->Insert(now, thread, Placement::Migration);
        });
    cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu);
    to_migrate->get_lock().Release();
  }
  ChainLockTransaction::Finalize();

  // Issue reschedule IPIs to CPUs with migrated threads.
  if (cpus_to_reschedule_mask) {
    mp_reschedule(cpus_to_reschedule_mask, 0);
  }
}

inline void Scheduler::UpdateEstimatedEnergyConsumption(Thread* current_thread,
                                                        SchedDuration actual_runtime_ns) {
  // Time in a low-power idle state should only accrue when running the idle
  // thread.
  const SchedDuration idle_processor_time_ns{IdlePowerThread::TakeProcessorIdleTime()};

  // TODO(https://fxbug.dev/379576294): Re-enable this assertion once we understand why the
  // idle_processor_time_ns is occasionally larger than actual_runtime_ns. Once the assertion
  // is back in place, the ktl::max below that clamps active_processor_time_ns to 0 can be
  // removed.
  // DEBUG_ASSERT_MSG(
  //  idle_processor_time_ns <= actual_runtime_ns,
  //  "idle_processor_time_ns=%" PRId64 " actual_runtime_ns=%" PRId64 " current_thread=%s",
  //  idle_processor_time_ns.raw_value(), actual_runtime_ns.raw_value(), current_thread->name());

  // Subtract any time the processor spent in the low-power idle state from the
  // runtime to ensure that active vs. idle power consumption is attributed
  // correctly. Processors can accumulate both active or idle power consumption,
  // but threads, including the idle power thread, accumulate only active power
  // consumption.
  const SchedDuration active_processor_time_ns =
      ktl::max<SchedDuration>(actual_runtime_ns - idle_processor_time_ns, SchedDuration(0));

  cpu_stats& stats = percpu::GetCurrent().stats;

  // The dynamic and leakage contributions are split between the active and max
  // idle (e.g. clock gating idle) power coefficients, respectively.
  if (power_level_control_.active_power_coefficient_nw() > 0) {
    const uint64_t active_energy_consumption_nj =
        (power_level_control_.active_power_coefficient_nw() +
         power_level_control_.max_idle_power_coefficient_nw()) *
        active_processor_time_ns.raw_value();

    stats.active_energy_consumption_nj += active_energy_consumption_nj;
    current_thread->scheduler_state().estimated_energy_consumption_nj +=
        active_energy_consumption_nj;
  }

  // TODO(https://fxbug.dev/377583571): Select the correct power coefficient
  // when deeper idle states are implemented. For now the max idle power
  // coefficient corresponds to the most general arch idle state (e.g. WFI,
  // halt).
  if (power_level_control_.max_idle_power_coefficient_nw() > 0 && idle_processor_time_ns > 0) {
    const uint64_t idle_energy_consumption_nj =
        power_level_control_.max_idle_power_coefficient_nw() * idle_processor_time_ns.raw_value();
    stats.idle_energy_consumption_nj += idle_energy_consumption_nj;
  }
}

void Scheduler::RescheduleCommon(Thread* const current_thread, SchedTime now,
                                 EndTraceCallback end_outer_trace) {
  ktrace::Scope trace =
      LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "reschedule_common", ("now", Round<uint64_t>(now)));

  DEBUG_ASSERT(current_thread == Thread::Current::Get());
  const cpu_num_t current_cpu = arch_curr_cpu_num();
  SchedulerState* const current_state = &current_thread->scheduler_state();

  // No spinlocks should be held as we come into a reschedule operation.  Only
  // the current thread's lock should be held.
  DEBUG_ASSERT_MSG(const uint32_t locks_held = arch_num_spinlocks_held();
                   locks_held == 0, "arch_num_spinlocks_held() == %u\n", locks_held);
  DEBUG_ASSERT(!arch_blocking_disallowed());
  DEBUG_ASSERT_MSG(const cpu_num_t tcpu = this_cpu();
                   current_cpu == tcpu, "current_cpu=%u this_cpu=%u", current_cpu, tcpu);
  ChainLockTransaction& active_clt = ChainLockTransaction::ActiveRef();
  active_clt.AssertNumLocksHeld(1);

  CPU_STATS_INC(reschedules);

  // Prepare for rescheduling by backing up the chain lock context and switching
  // to the special "sched token" which we use during reschedule operations in
  // order to guarantee that we will win arbitration with other active
  // transactions which are not reschedule operations.  We will restore this
  // context at the end of the operation (whether we context switched or not).
  const auto saved_transaction_state = ChainLockTransaction::SaveStateAndUseReservedToken();
  current_thread->get_lock().SyncToken();

  // Process all threads in the save_state_list_. We do this before performing any
  // other reschedule operation.
  ProcessSaveStateList(now);

  Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&queue_lock_, SOURCE_TAG};

  // When calling into reschedule, the current thread is only allowed to be in a
  // limited number of states, depending on where it came from.
  //
  // RUNNING   : Thread is the currently active thread on this CPU.
  // BLOCKED*  : Thread has been inserted into a wait queue and is in the
  //             process of de-activating.
  // SUSPENDED : Thread is in the process of suspending (but is not in any queue).
  // DEAD      : Thread is in the process of dying, this is its final reschedule.
  //
  AssertInScheduler(*current_thread);
  DEBUG_ASSERT(current_thread->state() != THREAD_READY);
  if (current_thread->state() == THREAD_RUNNING) {
    DEBUG_ASSERT(current_thread->disposition() == Disposition::Associated);
    current_thread->set_ready();
  }

  // TODO(https://fxbug.dev/381899402): Find the root cause of small negative values in the actual
  // runtime calculation.
  const SchedDuration actual_runtime_ns =
      ktl::max<SchedDuration>(now - current_state->last_started_running_, SchedDuration{0});
  current_state->last_started_running_ = now;

  const SchedDuration total_runtime_ns = now - start_of_current_time_slice_ns_;

  current_state->runtime_ns_ += actual_runtime_ns;
  current_thread->UpdateRuntimeStats(current_thread->state());

  // Update the energy consumption accumulators for the current task and
  // processor.
  UpdateEstimatedEnergyConsumption(current_thread, actual_runtime_ns);

  // Update the used time slice before evaluating the next task. Scale the
  // actual runtime of a deadline task by the relative performance of the CPU,
  // effectively increasing the capacity of the task in proportion to the
  // performance ratio. The remaining time slice may become negative due to
  // scheduler overhead.
  current_state->time_slice_used_ns_ +=
      IsDeadlineThread(current_thread) ? ScaleDown(actual_runtime_ns) : actual_runtime_ns;

  // Adjust the rate of the current thread when fair bandwidth demand changes.
  if (IsThreadAdjustable(current_thread) && weight_total_ != scheduled_weight_total_ &&
      current_state->remaining_time_slice_ns() > 0 && current_state->finish_time() > now) {
    ktrace::Scope trace_adjust_rate =
        LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "adjust_rate", ("total runtime", total_runtime_ns));
    scheduled_weight_total_ = weight_total_;
    AdjustFairBandwidth(current_thread, now);
    target_preemption_time_ns_ = ktl::clamp<SchedTime>(
        now + current_state->remaining_time_slice_ns(), now, current_state->finish_time());
  }

  // Use a small epsilon to avoid tripping the consistency check below due to
  // rounding when scaling the time slice.
  const SchedDuration time_slice_epsilon{100};
  const bool timeslice_expired = now >= current_state->finish_time_ ||
                                 current_state->remaining_time_slice_ns() <= time_slice_epsilon;

  // Check the consistency of the target preemption time and the current time
  // slice.
  [[maybe_unused]] const auto& ep = current_state->effective_profile_;
  DEBUG_ASSERT_MSG(
      now < target_preemption_time_ns_ || timeslice_expired,
      "cpu=%u capacity_ns=%" PRId64 " deadline_ns=%" PRId64
      "\nfinish_time                   =%16" PRId64 "\nnow                           =%16" PRId64
      "\ntarget_preemption_time_ns     =%16" PRId64 "\nstart_of_current_time_slice_ns=%16" PRId64
      "\ntotal_runtime_ns              =%16" PRId64 "\nactual_runtime_ns             =%16" PRId64
      "\ntime_slice_ns                 =%16" PRId64 "\ntime_slice_used_ns            =%16" PRId64
      "\nremaining_time_slice_ns       =%16" PRId64,
      this_cpu(), IsDeadlineThread(current_thread) ? ep.deadline().capacity_ns.raw_value() : 0,
      IsDeadlineThread(current_thread) ? ep.deadline().deadline_ns.raw_value() : 0,
      current_state->finish_time_.raw_value(), now.raw_value(),
      target_preemption_time_ns_.raw_value(), start_of_current_time_slice_ns_.raw_value(),
      total_runtime_ns.raw_value(), actual_runtime_ns.raw_value(),
      current_state->time_slice_ns_.raw_value(), current_state->time_slice_used_ns().raw_value(),
      current_state->remaining_time_slice_ns().raw_value());

  // If the current thread needs to be migrated, remove it for migration.
  const bool current_is_migrating = NeedsMigration(current_thread);
  if (current_is_migrating) {
    Remove(now, current_thread);
  }

  // Select the next thread to run.
  DequeueResult next_thread_result =
      EvaluateNextThread(now, current_thread, current_is_migrating, queue_guard);
  DEBUG_ASSERT(next_thread_result);
  DEBUG_ASSERT(!current_thread->scheduler_queue_state().in_queue());

  // Flush pending preemptions.
  mp_reschedule(current_thread->preemption_state().preempts_pending(), 0);
  current_thread->preemption_state().preempts_pending_clear();

  // If the next_thread is not the same as the current thread, we will need to
  // (briefly) drop the scheduler's queue lock in order to acquire the next
  // thread's lock exclusively.  If we have selected the same thread as before,
  // we can skip this step.
  //
  // If we do need to acquire next_thread's lock, we use the same chain lock
  // token that was used to acquire current_thread's lock.  We know this is safe
  // because:
  //
  // TODO(johngro): THIS IS NOT SAFE - FIX THIS COMMENT
  //
  // This turned out to not be safe after all.  The deadlock potential below is
  // why we need to switch to the special "scheduler" token at the start of the
  // operation.
  //
  // While there cannot be any cycles in this individual operation (trivial to
  // show since there are two different locks here), deadlock is still possible.
  //
  // The flow is:
  // 1) T0 is running on CPU X, and is locked entering a reschedule operation.
  // 2) T1 is selected as the next thread to run, and is being locked
  //    exclusively (after dropping the scheduler's lock).  At the same time:
  // 3) T2 is running on CPU Y, and is blocking on an OWQ which currently has T0
  //    as the owner.  T1 has been selected as the new owner.
  // 4) T2 and the queue have been locked by the BAAO operation.
  // 5) T1 is the start of the new owner chain and is locked.
  // 6) The next step in the BAAO locking is to lock the old owner chain,
  //    starting from T0.
  //
  // So:
  // X has T0 and wants T1.
  // Y has T1 and wants T0.
  //
  // CPU Y is in a sequence which obeys the backoff protocol, so as long as its
  // token value > X's token value, Y will back off and we will be OK.  If not,
  // however, we deadlock.  The reschedule operation cannot backoff and drop
  // T0's lock, so it is stuck trying to get T1 from Y.
  //
  // TODO(johngro): END TODO
  //
  // 1) current_thread is currently running.  Because of this, we know that it
  //    is the target node of it's PI graph.
  // 2) next thread is currently READY, which means it is also the target node
  //    of it's PI graph.
  // 3) PI graphs always have exactly one target node.
  // 4) Therefore, current and next cannot be members of the same graph, and can
  //    both be acquired unconditionally.
  //
  if (current_thread != next_thread_result.thread()) {
    // The ChainLockTransaction we were in when we entered RescheduleCommon has
    // already been finalized.  We need to restart it in order to obtain new
    // locks.
    active_clt.Restart(CLT_TAG("Scheduler::RescheduleCommon (restart)"));

    // Release the queue lock, acquire the next thread lock, and re-acquire the
    // queue lock. While the queue lock is unlocked, a PI operation or affinity
    // change can update the next thread's state, since these operations could
    // already hold the next thread's lock while waiting for the queue lock.
    // These operations are obligated to reschedule this CPU if they invalidate
    // the next thread's state, such that a different thread should run or the
    // current CPU is no longer in the affinity mask. However, since the next
    // thread is no longer in the run queue, it cannot be discovered and stolen
    // by another CPU while the queue lock is unlocked.
    queue_guard.CallUnlocked(
        [&next_thread_result]() TA_ACQ(next_thread_result.thread()->get_lock()) {
          ChainLockTransaction::AssertActive();
          // Acquire the lock without backing off using an explicit retry loop
          // because the current thread's lock is held and ChainLock::Acquire
          // asserts that no chain locks are currently held to avoid misuse in
          // the general case.
          while (!next_thread_result.thread()->get_lock().TryAcquire(
              ChainLock::AllowFinalized::No, ChainLock::RecordBackoff::No)) {
            arch::Yield();
          }
        });

    // All of the locks needed to complete the reschedule are held.
    ChainLockTransaction::Finalize();
  }

  // Update the state of the current and next thread.
  Thread* const next_thread = next_thread_result.thread();
  next_thread->get_lock().AssertHeld();
  AssertInScheduler(*next_thread);

  const SchedulerQueueState& current_queue_state = current_thread->scheduler_queue_state();
  SchedulerState* const next_state = &next_thread->scheduler_state();

  // Convert from variable time to monotonic time if necessary.
  next_thread_result.thread()->get_lock().MarkHeld();
  next_thread_result.UpdateThreadTimeline();

  // If the next thread was just stolen from another CPU, finish associating it
  // with this CPU and scheduler.
  if (next_thread->disposition() == Disposition::Stolen) {
    DEBUG_ASSERT(next_thread->state() == THREAD_READY);
    DEBUG_ASSERT(next_state->curr_cpu() != current_cpu);
    DEBUG_ASSERT(next_thread->scheduler_queue_state().stolen_by() == current_cpu);
    DEBUG_ASSERT(!next_thread_result.is_updated_pending());
    Insert(now, next_thread, Placement::Association);
  }
  DEBUG_ASSERT(next_thread->disposition() == Disposition::Associated);

  if (current_thread != next_thread) {
    // Update stall contributions of the current and next thread.
    StallAccumulator::ApplyContextSwitch(current_thread, next_thread);

    // If the current thread is not the idle thread and not migrating, return it
    // to the run queue or remove its bookkeeping, depending on whether it is
    // READY.
    if (!current_thread->IsIdle() && !current_is_migrating) {
      if (current_thread->state() == THREAD_READY) {
        QueueThread(current_thread,
                    timeslice_expired ? Placement::Insertion : Placement::Preemption, now);
      } else {
        RemoveThreadLocked(now, current_thread);
      }
    }
  }

  next_thread->set_running();
  next_state->last_cpu_ = current_cpu;
  DEBUG_ASSERT(next_state->curr_cpu_ == current_cpu);
  active_thread_ = next_thread;

  // Keep track of the weight of the active thread, since it is not in the run
  // queue contributing to the min_weight subtree invariant.
  active_thread_weight_ = !next_thread->IsIdle() && IsFairThread(next_thread)
                              ? next_state->effective_profile().weight()
                              : SchedWeight::Max();

  // Handle any pending migration work.
  next_thread->CallMigrateFnLocked(Thread::MigrateStage::Restore);

  // Update the expected runtime of the current thread and the per-CPU total.
  // Only update the thread and aggregate values if the current thread is still
  // associated with this CPU or is no longer ready.
  const bool current_is_associated =
      !current_queue_state.active() || current_state->curr_cpu_ == current_cpu;
  if (!current_thread->IsIdle() && current_is_associated &&
      (timeslice_expired || current_thread != next_thread)) {
    ktrace::Scope trace_update_ema = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "update_expected_runtime");

    // Adjust the runtime for the relative performance of the CPU to account for
    // different performance levels in the estimate. The relative performance
    // scale is in the range (0.0, 1.0], such that the adjusted runtime is
    // always less than or equal to the monotonic runtime.
    const SchedDuration adjusted_total_runtime_ns = ScaleDown(total_runtime_ns);
    current_state->banked_runtime_ns_ += adjusted_total_runtime_ns;

    if (timeslice_expired || !current_queue_state.active()) {
      const SchedDuration delta_ns =
          PeakDecayDelta(current_state->expected_runtime_ns_, current_state->banked_runtime_ns_,
                         kExpectedRuntimeAlpha, kExpectedRuntimeBeta);
      current_state->expected_runtime_ns_ += delta_ns;
      current_state->banked_runtime_ns_ = SchedDuration{0};

      // Adjust the aggregate value by the same amount. The adjustment is only
      // necessary when the thread is still active on this CPU.
      if (current_queue_state.active()) {
        UpdateTotalExpectedRuntime(delta_ns);
      }
    }
  }

  // Update the current performance scale only after any uses in the reschedule
  // path above to ensure the scale is applied consistently over the interval
  // between reschedules (i.e. not earlier than the requested update).
  //
  // Updating the performance scale also results in updating the target
  // preemption time below when the current thread is deadline scheduled.
  //
  // TODO(eieio): Apply a minimum value threshold to the userspace value.
  // TODO(eieio): Shed load when total utilization is above kCpuUtilizationLimit.
  const bool performance_scale_updated = performance_scale_ != pending_user_performance_scale_;
  if (performance_scale_updated) {
    performance_scale_ = pending_user_performance_scale_;
    performance_scale_reciprocal_ = 1 / performance_scale_;
  }

  // Flush any pending power level request.
  power_level_control_.SendPendingPowerLevelRequest();

  SetIdle(next_thread->IsIdle());

  if (current_thread->IsIdle()) {
    percpu::Get(current_cpu).stats.idle_time += actual_runtime_ns;
  }

  if (next_thread->IsIdle()) {
    ktrace::Scope trace_stop_preemption = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "idle");
    next_state->last_started_running_ = now;

    // If there are no tasks to run in the future or there is idle/power work to
    // perform, disable the preemption timer.  Otherwise, set the preemption
    // time to the earliest eligible time.
    target_preemption_time_ns_ = percpu::Get(this_cpu_).idle_power_thread.pending_power_work()
                                     ? SchedTime(ZX_TIME_INFINITE)
                                     : GetNextEligibleTime();
    PreemptReset(current_cpu, now.raw_value(), target_preemption_time_ns_.raw_value());
  } else if (timeslice_expired || current_thread != next_thread) {
    ktrace::Scope trace_start_preemption = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "next_slice");

    // Re-compute the time slice and ideal preemption time for the new thread
    // based on the latest state.
    target_preemption_time_ns_ = NextThreadTimeslice(next_thread, now);

    // Update the thread's runtime stats to record the amount of time that it spent in the run
    // queue.
    next_thread->UpdateRuntimeStats(next_thread->state());

    next_state->last_started_running_ = now;
    start_of_current_time_slice_ns_ = now;
    scheduled_weight_total_ = weight_total_;

    // Adjust the preemption time to account for a deadline thread becoming
    // eligible before the current time slice expires.
    const SchedTime preemption_time_ns =
        IsFairThread(next_thread)
            ? ClampToDeadline(target_preemption_time_ns_)
            : ClampToEarlierDeadline(target_preemption_time_ns_, next_state->finish_time_);
    DEBUG_ASSERT(preemption_time_ns <= target_preemption_time_ns_);

    PreemptReset(current_cpu, now.raw_value(), preemption_time_ns.raw_value());
    trace_start_preemption =
        KTRACE_END_SCOPE(("preemption_time", Round<uint64_t>(preemption_time_ns)),
                         ("target preemption time", Round<uint64_t>(target_preemption_time_ns_)));

    // Emit a flow end event to match the flow begin event emitted when the
    // thread was enqueued. Emitting in this scope ensures that thread just
    // came from the run queue (and is not the idle thread).
    LOCAL_KTRACE_FLOW_END(FLOW, "sched_latency", next_state->flow_id(),
                          ("tid", next_thread->tid()));
  } else {
    ktrace::Scope trace_continue = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "continue");
    DEBUG_ASSERT(current_thread == next_thread);

    // Update the target preemption time for consistency with the updated CPU
    // performance scale.
    if (performance_scale_updated && IsDeadlineThread(next_thread)) {
      target_preemption_time_ns_ = NextThreadTimeslice(next_thread, now);
    }

    // The current thread should continue to run. A throttled deadline thread
    // might become eligible before the current time slice expires. Figure out
    // whether to set the preemption time earlier to switch to the newly
    // eligible thread.
    //
    // The preemption time should be set earlier when either:
    //   * Current is a fair thread and a deadline thread will become eligible
    //     before its time slice expires.
    //   * Current is a deadline thread and a deadline thread with an earlier
    //     deadline will become eligible before its time slice expires.
    //
    // Note that the target preemption time remains set to the ideal
    // preemption time for the current task, even if the preemption timer is set
    // earlier. If a task that becomes eligible is stolen before the early
    // preemption is handled, this logic will reset to the original target
    // preemption time.
    const SchedTime preemption_time_ns =
        IsFairThread(next_thread)
            ? ClampToDeadline(target_preemption_time_ns_)
            : ClampToEarlierDeadline(target_preemption_time_ns_, next_state->finish_time_);
    DEBUG_ASSERT(preemption_time_ns <= target_preemption_time_ns_);

    PreemptReset(current_cpu, now.raw_value(), preemption_time_ns.raw_value());
    trace_continue = KTRACE_END_SCOPE(("preemption_time", preemption_time_ns),
                                      ("target preemption time", target_preemption_time_ns_));
  }

  // Assert that there is no path beside running the idle thread can leave the
  // preemption timer unarmed. However, the preemption timer may or may not be
  // armed when running the idle thread.
  // TODO(eieio): In the future, the preemption timer may be canceled when there
  // is only one task available to run. Revisit this assertion at that time.
  DEBUG_ASSERT(next_thread->IsIdle() || percpu::Get(current_cpu).timer_queue.PreemptArmed());

  // Almost done, we need to handle the actual context switch (if any).
  if (current_thread != next_thread) {
    LOCAL_KTRACE(
        DETAILED, "switch_threads", ("total threads", runnable_task_count_),
        ("total weight", weight_total_.raw_value()),
        ("current thread time slice", current_thread->scheduler_state().remaining_time_slice_ns()),
        ("next thread time slice", next_thread->scheduler_state().remaining_time_slice_ns()));

    TraceThreadQueueEvent("tqe_astart"_intern, next_thread);

    // Release queue lock before context switching.
    queue_guard.Release();

    // Finish the migration of the current thread, if pending.
    if (current_is_migrating) {
      current_thread->CallMigrateFnLocked(Thread::MigrateStage::Save);

      const cpu_num_t target_cpu = FindActiveSchedulerForThread(
          current_thread, [now, this](Thread* thread, Scheduler* target) {
            MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
            DEBUG_ASSERT(target != this);
            target->Insert(now, thread, Placement::Migration);
          });

      mp_reschedule(cpu_num_to_mask(target_cpu), 0);
    }

    // Trace the activation of the next thread before context switching.
    TraceContextSwitch(current_thread, next_thread, current_cpu);

    // We invoke the context switch functions before context switching, so that
    // they have a chance to correctly perform the actions required. Doing so
    // after context switching may lead to an invalid CPU state.
    current_thread->CallContextSwitchFnLocked();
    next_thread->CallContextSwitchFnLocked();

    // Notes about Thread aspace rules:
    //
    // Typically, it is only safe for the current thread to access its aspace
    // member directly, as only a running thread can change its own aspace, and
    // if a thread is running, then its process must also be alive and therefore
    // its aspace must also be alive.
    //
    // Context switching is a bit of an edge case.  The current thread is
    // becoming the next thread.  Both aspaces must still be alive (even if the
    // current thread is in the process of becoming rescheduled for the very
    // last time as it exits), and neither one can change its own aspace right
    // now (they are not really running).
    //
    // Because of this, it should be OK for us to directly access the aspaces
    // during the context switch, without needing to either check the thread's
    // state, or add any references to the VmAstate object.
    [&]() TA_NO_THREAD_SAFETY_ANALYSIS {
      if (current_thread->aspace() != next_thread->aspace()) {
        vmm_context_switch(current_thread->aspace(), next_thread->aspace());
      }
    }();

    CPU_STATS_INC(context_switches);

    // Prevent the scheduler durations from spanning the context switch.
    // Some context switches do not resume within this method on the other
    // thread, which results in unterminated durations. All of the callers
    // with durations tail-call this method, so terminating the duration
    // here should not cause significant inaccuracy of the outer duration.
    trace.End();
    if (end_outer_trace) {
      end_outer_trace();
    }

    if constexpr (SCHEDULER_QUEUE_TRACING_ENABLED) {
      const uint64_t arg0 = 0;
      const uint64_t arg1 = (ktl::clamp<uint64_t>(this_cpu_, 0, 0xF) << 16);
      KTrace::Probe(KTrace::Context::Cpu, "tqe_afinish"_intern, arg0, arg1);
    }

    // Time for the context switch.  Before actually performing the switch,
    // record the current thread as the "previous thread" for the next thread
    // becoming scheduled.  The next thread needs to know which thread preceded
    // it in order to drop its lock before unwinding, and our current stack is
    // not going to be available to it.
    DEBUG_ASSERT(previous_thread_ == nullptr);
    previous_thread_ = current_thread;
    arch_context_switch(current_thread, next_thread);

    // We made it to the other side of the switch.  We have switched stacks, so
    // the local variable meanings have become redefined.
    //
    // ++ The value of current_thread after the context switch is the value of
    //    next_thread before the switch.
    // ++ The value of next_thread after the context switch is the thread that
    //    the new current_thread context-switched to the last time it ran
    //    through here.  We don't even know if this thread exists now, so its
    //    value is junk.
    // ++ The value of current_thread before the switch has been stashed in
    //    the new current thread's scheduler_state's previous thread member.
    //
    // We need to drop the old current thread's lock, while continuing to hold
    // the new current thread's lock as we unwind.
    // Scheduler::LockHandoffInternal will take care of dropping the previous
    // current_thread's lock, but at this point, the static analyzer is going to
    // be extremely confused because it does not (and cannot) know about the
    // stack-swap which just happened. It thinks that we are still holding
    // next_thread's lock (and we are; it is now current thread) and that we
    // need to drop it (it is wrong about this, we need to drop the previous
    // current thread's lock).  So, just lie to it. Tell it that we have dropped
    // next_thread's lock using a no-op assert (we don't actually want to
    // examine the state of the lock in any way, since next thread is no longer
    // valid).
    Scheduler::LockHandoffInternal(saved_transaction_state, current_thread);
    next_thread->get_lock().MarkReleased();
  } else {
    // Restore the transaction state and drop the queue guard before unwinding.
    ChainLockTransaction::RestoreState(saved_transaction_state);
    current_thread->get_lock().SyncToken();
    queue_guard.Release();
  }
}

void Scheduler::TrampolineLockHandoff() {
  ChainLockTransaction::AssertActive();

  // Construct the fake chainlock transaction we will be restoring.  It should:
  //
  // 1) Not attempt to register itself with the per-cpu data structure.  There
  //    should already be a registered CLT, and attempting to instantiate a new
  //    one using the typical constructor would trigger an ASSERT.  This should
  //    look like a transaction which had been sitting on a stack, but swapped
  //    out using PrepareForReschedule at some point in the past.
  // 2) Report that there are currently two locks held (because there are).
  //    We have the previously running thread's lock, and the current (new)
  //    thread's lock held (obtained by Scheduler::RescheduleCommon).
  // 3) Report that the transaction has been finalized, as it would have been
  //    had we come in via RescheduleCommon.
  //
  ChainLockTransaction transaction =
      ChainLockTransaction::MakeBareTransaction(CLT_TAG("Trampoline"), 2, true);
  // Assert that the current thread's lock was "acquired" here.  It was
  // actually acquired during the context switch from
  // Scheduler::RescheduleCommon, but we now need to release it before letting
  // the new thread run.  The currently registered CLT should be using the
  // scheduler token that was used to acquire the new thread's lock during
  // reschedule common.
  Thread* const current_thread = Thread::Current::Get();
  current_thread->get_lock().AssertAcquired();

  // Now perform our lock handoff.  This will release our previous
  // thread's lock, and restore the transaction we just instantiated as
  // the active transaction, and replace the current thread's lock's
  // token with the new non-scheduler token.
  Scheduler::LockHandoffInternal(transaction.SaveStateInitial(), current_thread);

  // Finally, we just need to release the current thread's lock and we
  // are finished.
  current_thread->get_lock().Release();
}

void Scheduler::LockHandoffInternal(SavedState saved_state, Thread* const current_thread) {
  DEBUG_ASSERT(arch_ints_disabled());
  Scheduler* scheduler = Scheduler::Get();
  Thread* const previous_thread = scheduler->previous_thread_;
  DEBUG_ASSERT(previous_thread != nullptr);

  scheduler->previous_thread_ = nullptr;
  previous_thread->get_lock().AssertAcquired();
  ChainLockTransaction::Active()->AssertNumLocksHeld(2);
  ChainLockTransaction::RestoreState(saved_state);
  current_thread->get_lock().SyncToken();
  previous_thread->get_lock().SyncToken();
  previous_thread->get_lock().Release();
}

SchedDuration Scheduler::CalculateTimeslice(const Thread* thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "calculate_timeslice");
  const SchedulerState& state = thread->scheduler_state();
  const EffectiveProfile& ep = state.effective_profile_;

  const SchedUtilization fair_utilization = ep.weight() / weight_total_;
  const SchedDuration time_slice_ns = fair_period_ * fair_utilization;

  trace = KTRACE_END_SCOPE(
      ("fair utilization", Round<uint64_t>(fair_utilization * 1000)), ("time slice", time_slice_ns),
      ("weight", ep.weight().raw_value()),
      ("total weight", KTRACE_ANNOTATED_VALUE(AssertHeld(queue_lock_), weight_total_.raw_value())));
  return time_slice_ns;
}

SchedTime Scheduler::ClampToDeadline(SchedTime completion_time) {
  return ktl::min(completion_time, GetNextEligibleTime());
}

SchedTime Scheduler::ClampToEarlierDeadline(SchedTime completion_time, SchedTime finish_time) {
  const Thread* const thread = FindEarlierDeadlineThread(completion_time, finish_time);

  if (thread != nullptr) {
    AssertInScheduler(*thread);
    return ktl::min(completion_time, thread->scheduler_state().start_time_);
  }

  return completion_time;
}

SchedTime Scheduler::NextThreadTimeslice(Thread* thread, SchedTime now) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "next_timeslice");
  SchedulerState* const state = &thread->scheduler_state();

  if (IsFairThread(thread)) {
    state->time_slice_ns_ = CalculateTimeslice(thread);
  }

  // Calculate the deadline when the remaining time slice is completed. The
  // time slice is maintained by the deadline queuing logic, no need to update
  // it here. The target preemption time is based on the time slice scaled by
  // the performance of the CPU and clamped to the deadline. This increases
  // capacity on slower processors, however, bandwidth isolation is preserved
  // because CPU selection attempts to keep scaled total capacity below one.
  const SchedDuration scaled_time_slice_ns = IsDeadlineThread(thread)
                                                 ? ScaleUp(state->remaining_time_slice_ns())
                                                 : state->remaining_time_slice_ns();
  const SchedTime target_preemption_time_ns =
      ktl::min<SchedTime>(now + scaled_time_slice_ns, state->finish_time_);

  trace = KTRACE_END_SCOPE(("scaled time slice", scaled_time_slice_ns),
                           ("target preemption time", target_preemption_time_ns));
  return target_preemption_time_ns;
}

void Scheduler::UpdateActivation(Thread* thread, SchedTime now) {
  DEBUG_ASSERT(thread->state() == THREAD_READY);
  DEBUG_ASSERT(!thread->IsIdle());

  SchedulerState* const state = &thread->scheduler_state();
  EffectiveProfile& ep = state->effective_profile_;

  ktrace::Scope update_trace = LOCAL_KTRACE_BEGIN_SCOPE(
      DETAILED, "update", ("remaining timeslice before", state->remaining_time_slice_ns()));

  // Determine how much time is left before the finish time. This might be less
  // than the remaining time slice or negative if the thread blocked.
  const SchedDuration time_until_finish_time_ns = state->finish_time_ - now;
  if (time_until_finish_time_ns <= 0 || state->remaining_time_slice_ns() <= 0) {
    // Start a new activation and update the period and capacity with the latest
    // fair or deadline bandwidth parameters. Although the fair capacity is
    // determined dynamically during reschedules by CalculateTimeslice, use a
    // non-zero initial value here to ensure that multiple successive calls to
    // UpdateActivation do not incorrectly start new activations with start
    // times further into the future.
    //
    // For example, such a succession can occur when a fair thread wakes after
    // its current activation has expired, is re-activated as it enters the run
    // queue, and is later stolen and potentially re-activated again. Both wake
    // and steal operations can occur after a thread's activation has expired
    // and must start a new activation. Using a non-zero value for the new
    // activation's capacity ensures that the second call to UpdateActivation
    // during stealing does not enter this conditional block unless enough time
    // has passed in the run queue for the finish time to expire.
    const SchedDuration period_ns = IsFairThread(thread) ? fair_period_ : ep.deadline().deadline_ns;
    const SchedDuration capacity_ns = IsFairThread(thread) ? period_ns : ep.deadline().capacity_ns;
    const SchedTime period_finish_ns = state->start_time_ + period_ns;

    state->start_time_ = now >= period_finish_ns ? now : period_finish_ns;
    state->finish_time_ = state->start_time_ + period_ns;
    state->time_slice_ns_ = capacity_ns;
    state->time_slice_used_ns_ = SchedDuration{0};
  }
  DEBUG_ASSERT(state->remaining_time_slice_ns() > 0);

  update_trace = KTRACE_END_SCOPE(
      ("remaining time slice after", state->remaining_time_slice_ns()),
      ("time until finish time", time_until_finish_time_ns), ("start time", state->start_time()),
      ("finish time", state->finish_time()),
      ("effective period", SchedDuration{state->finish_time() - state->start_time()}),
      ("eligible", state->start_time() <= now));
}

void Scheduler::QueueThread(Thread* thread, Placement placement, SchedTime now) {
  ktrace::Scope trace =
      LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "queue_thread", ("placement", ToStringRef(placement)));

  DEBUG_ASSERT(thread->state() == THREAD_READY);
  DEBUG_ASSERT(!thread->IsIdle());
  DEBUG_ASSERT(placement != Placement::Association);

  SchedulerState* const state = &thread->scheduler_state();

  // Check the consistency of the start and finish times before adding the
  // thread to the run queue.
  DEBUG_ASSERT_MSG(
      state->start_time() < state->finish_time(),
      "now=%" PRId64 " start=%" PRId64 " finish=%" PRId64 " remaining_time_slice=%" PRId64
      " fair_period=%" PRId64 " mono_ref=%" PRId64 " var_ref=%" PRId64 " slope=%s\n",
      now.raw_value(), state->start_time().raw_value(), state->finish_time().raw_value(),
      state->remaining_time_slice_ns().raw_value(), fair_period_.raw_value(),
      fair_affine_transform_.monotonic_reference_time().raw_value(),
      fair_affine_transform_.variable_reference_time().raw_value(),
      Format(fair_affine_transform_.slope()).c_str());

  // Only update the enqueue time and emit a flow event if this is an insertion,
  // preemption, or migration. In contrast, an adjustment only changes the queue
  // position in the same queue due to a parameter change and should not perform
  // these actions.
  if (placement != Placement::Adjustment) {
    if (placement == Placement::Migration) {
      // Connect the flow into the previous queue to the new queue.
      LOCAL_KTRACE_FLOW_STEP(FLOW, "sched_latency", state->flow_id(), ("tid", thread->tid()));
    } else {
      // Reuse this member to track the time the thread enters the run queue. It
      // is not read outside of the scheduler unless the thread state is
      // THREAD_RUNNING.
      state->last_started_running_ = now;
      state->flow_id_ = NextFlowId();
      LOCAL_KTRACE_FLOW_BEGIN(FLOW, "sched_latency", state->flow_id(), ("tid", thread->tid()));
    }
  }

  if (IsFairThread(thread)) {
    // Convert start and finish times to variable time before inserting.
    // TODO(eieio): Convert from standard period to local period.
    MonotonicToVariableInPlace(state->start_time_);
    MonotonicToVariableInPlace(state->finish_time_);
    fair_run_queue_.insert(thread);
  } else {
    deadline_run_queue_.insert(thread);
  }

  if (placement != Placement::Adjustment) {
    TraceThreadQueueEvent("tqe_enque"_intern, thread);
  }

  trace =
      KTRACE_END_SCOPE(("start time", state->start_time_), ("finish time", state->finish_time_));
}

// Evaluates the fair bandwidth demand and updates the period scale, preventing fair bandwidth
// time slices from becoming too small, and causing thrashing, by ensuring that threads with the
// minimum weight in the queue get at least the minimum capacity.
//
// To guarantee that a fair bandwidth thread receives at least a minimum capacity, the following
// relationship must hold:
//
//   w_min / w_total >= C_min / T_default
//
// Where:
//
//   w_min is the minimum weight of threads in the run queue and the active thread.
//   w_total is the total weight of the run queue.
//   C_min is the minimum capacity.
//   T_default is the default fair task period.
//
// If this relationship does not hold, a scale factor is applied to expand the fair task period
// and ensure that tasks with the minimum weight receive the minimum capacity:
//
//   s = C_min / T_default * w_total / w_min
//
void Scheduler::UpdateFairBandwidthPeriod(SchedTime now) {
  // Returns the minimum weight of the run queue, including the active thread.
  const auto get_min_weight = [this]() TA_REQ(queue_lock_) {
    if (!fair_run_queue_.is_empty()) {
      const Thread& root = *fair_run_queue_.croot();
      AssertInRunQueue(root);
      return ktl::min(active_thread_weight_, root.scheduler_state().subtree_invariants_.min_weight);
    }
    return active_thread_weight_;
  };

  const SchedWeight min_weight = get_min_weight();

  if (min_weight < weight_total_ * kMinFairUtilization) {
    const SchedUtilization min_utilization = min_weight / weight_total_;
    const Affine::Slope slope =
        ktl::max<Affine::Slope>(min_utilization / kMinFairUtilization, Affine::kMinSlope);

    if (fair_affine_transform_.slope() != slope) {
      fair_affine_transform_.ChangeSlopeAtMonotonicTime(now, slope);
      fair_period_ = kDefaultFairPeriod / slope;
    }
  } else {
    if (fair_affine_transform_.slope() != Affine::kMaxSlope) {
      fair_affine_transform_.ChangeSlopeAtMonotonicTime(now, Affine::kMaxSlope);
      fair_period_ = kDefaultFairPeriod;
    }
  }
}

void Scheduler::AdjustFairBandwidth(Thread* thread, SchedTime now) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "adjust_bandwidth");
  SchedulerState& state = thread->scheduler_state();
  DEBUG_ASSERT(IsFairThread(thread));
  DEBUG_ASSERT(state.remaining_time_slice_ns() > 0 && state.finish_time() > now);

  // Calculate the new remaining time slice from the current remaining time
  // slice and the changes to period and utilization, such that lag is
  // conserved, keeping the same finish time.
  const SchedDuration time_until_finish_time_ns = state.finish_time() - now;
  const SchedDuration period_delta = fair_period_ - state.effective_period();
  const SchedDuration time_slice_ns = CalculateTimeslice(thread);

  const SchedUtilization old_utilization = state.time_slice_ns() / state.effective_period();
  const SchedUtilization new_utilization = time_slice_ns / fair_period_;
  const SchedUtilization utilization_delta = new_utilization - old_utilization;

  const SchedDuration old_remaining_time_slice_ns = state.remaining_time_slice_ns();
  const SchedDuration new_remaining_time_slice =
      old_remaining_time_slice_ns + utilization_delta * time_until_finish_time_ns;

  // Update the dynamic parameters, clamping the remaining time slice to the end
  // of the period.
  state.time_slice_ns_ = time_slice_ns;
  state.time_slice_used_ns_ =
      time_slice_ns - ktl::min(new_remaining_time_slice, time_until_finish_time_ns);
  state.start_time_ = state.finish_time() - fair_period_;

  trace =
      KTRACE_END_SCOPE(("r_i", old_remaining_time_slice_ns), ("r_i'", new_remaining_time_slice),
                       ("r_{i,e}", state.remaining_time_slice_ns()),
                       ("dU_i", Round<int64_t>(utilization_delta * 1000)), ("dT_i", period_delta),
                       ("d_i", time_until_finish_time_ns), ("eligible", state.start_time() <= now));
}

void Scheduler::Insert(SchedTime now, Thread* thread, Placement placement) {
  DEBUG_ASSERT(thread->state() == THREAD_READY);
  DEBUG_ASSERT(!thread->IsIdle());
  DEBUG_ASSERT(IsSchedulerActiveLocked());

  SchedulerQueueState& queue_state = thread->scheduler_queue_state();
  SchedulerState& state = thread->scheduler_state();
  const EffectiveProfile& ep = state.effective_profile_;

  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(
      DETAILED, "insert", ("start time", state.start_time()), ("finish time", state.finish_time()),
      ("expired finish time", now >= state.finish_time()));

  // Ensure insertion happens only once, even if Unblock is called multiple times.
  if (queue_state.OnInsert()) {
    // Insertion can happen from a different CPU. Set the thread's current
    // CPU to the one this scheduler instance services.
    state.curr_cpu_ = this_cpu();

    UpdateTotalExpectedRuntime(state.expected_runtime_ns_);

    if (IsFairThread(thread)) {
      weight_total_ += ep.weight();
      DEBUG_ASSERT(weight_total_ > 0);
      UpdateFairBandwidthPeriod(now);
      if (state.remaining_time_slice_ns() > 0 && state.finish_time() > now) {
        AdjustFairBandwidth(thread, now);
      }
    } else {
      UpdateTotalDeadlineUtilization(ep.deadline().utilization);
    }
    runnable_task_count_++;
    DEBUG_ASSERT(runnable_task_count_ > 0);
    TraceTotalRunnableThreads();

    // Start a new activation, if necessary.
    UpdateActivation(thread, now);

    if (placement != Placement::Association) {
      QueueThread(thread, placement, now);
    } else {
      // Connect the flow into the previous queue to the new queue.
      LOCAL_KTRACE_FLOW_STEP(FLOW, "sched_latency", state.flow_id(), ("tid", thread->tid()));
    }
  }
}

void Scheduler::Remove(SchedTime now, Thread* thread, cpu_num_t stolen_by) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "remove");
  DEBUG_ASSERT(!thread->IsIdle());

  SchedulerQueueState& queue_state = thread->scheduler_queue_state();
  const SchedulerState& state = const_cast<const Thread*>(thread)->scheduler_state();
  const EffectiveProfile& effective_profile = state.effective_profile_;

  DEBUG_ASSERT(!queue_state.in_queue());

  // Ensure that removal happens only once, even if Block() is called multiple times.
  if (queue_state.OnRemove(stolen_by)) {
    UpdateTotalExpectedRuntime(-state.expected_runtime_ns_);

    if (IsFairThread(thread)) {
      weight_total_ -= effective_profile.weight();
      DEBUG_ASSERT(weight_total_ >= 0);
      UpdateFairBandwidthPeriod(now);
    } else {
      UpdateTotalDeadlineUtilization(-effective_profile.deadline().utilization);
    }
    DEBUG_ASSERT(runnable_task_count_ > 0);
    runnable_task_count_--;
    TraceTotalRunnableThreads();
  }
}

void Scheduler::RemoveThreadLocked(SchedTime now, Thread* thread) {
  Remove(now, thread);
  thread->scheduler_state().curr_cpu_ = INVALID_CPU;
}

void Scheduler::ValidateInvariantsUnconditional() const {
  using ProfileDirtyFlag = SchedulerState::ProfileDirtyFlag;

  auto ObserveFairThread = [&](const Thread& t) TA_REQ(this->queue_lock_,
                                                       chainlock_transaction_token) {
    // We are holding the scheduler's queue lock, and observing thread which are
    // either active, or members of our queues.  Tell the static analyzer that
    // it is OK for us to access a thread's effective profile in a read-only
    // fashion, even though we don't explicitly hold the thread's lock.
    [&t]() {
      MarkHasOwnedThreadAccess(t);
      const auto& ss = t.scheduler_state();
      const auto& ep = ss.effective_profile();
      ASSERT_MSG(ep.IsFair(), "Fair thread %" PRIu64 " has non-fair effective profile", t.tid());
    }();

    // If we have dirty tracking enabled for effective profiles (only enabled in
    // builds with extra scheduler state validation enabled) we can try to make
    // some extra checks.
    //
    // 1) If we know that the current base profile is clean, we can assert that
    //    the base profile is fair.
    // 2) If we know that the current IPVs are clean, we can assert that the IPV
    //    utilization is zero.
    //
    // In order to safely observe these things, however, we need to be holding
    // the thread's lock.  Threads can change their base profile or IPVs (and
    // the flags which indicate if they are dirty or nor) while only holding
    // their lock.  They do not need to be holding the scheduler's queue lock if
    // the thread happens to be running or ready.
    //
    // So, _try_ to get the thread's lock if we don't already hold it, and if we
    // succeed, go ahead a perform our checks.  If we fail to get the lock for
    // any reason, just skip the checks this time.
    if constexpr (EffectiveProfile::kDirtyTrackingEnabled) {
      auto ExtraChecks = [&t]() TA_REQ(t.get_lock()) -> void {
        const auto& ss = t.scheduler_state();
        const auto& ep = ss.effective_profile();
        const auto& bp = ss.base_profile_;
        const auto& ipv = ss.inherited_profile_values_;

        if (!(ep.dirty_flags() & ProfileDirtyFlag::BaseDirty)) {
          ASSERT_MSG(bp.IsFair(), "Fair thread %" PRIu64 " has clean, but non-fair, base profile",
                     t.tid());
        }
        if (!(ep.dirty_flags() & ProfileDirtyFlag::InheritedDirty)) {
          ASSERT_MSG(ipv.uncapped_utilization == SchedUtilization{0},
                     "Fair thread %" PRIu64
                     " has clean IPV, but non-zero inherited utilization (%" PRId64 ")",
                     t.tid(), ipv.uncapped_utilization.raw_value());
        }
      };

      if (t.get_lock().is_held()) {
        t.get_lock().MarkHeld();
        ExtraChecks();
      } else if (t.get_lock().TryAcquire(ChainLock::AllowFinalized::Yes)) {
        ExtraChecks();
        t.get_lock().Release();
      }
    }
  };

  auto ObserveDeadlineThread = [&](const Thread& t) TA_REQ(this->queue_lock_,
                                                           chainlock_transaction_token) {
    // See above for the locking rules for accessing the pieces of effective profile.
    [&t]() {
      MarkHasOwnedThreadAccess(t);
      const auto& ss = t.scheduler_state();
      const auto& ep = ss.effective_profile();
      ASSERT_MSG(ep.IsDeadline(), "Deadline thread %" PRIu64 " has non-deadline effective profile",
                 t.tid());
    }();

    if constexpr (EffectiveProfile::kDirtyTrackingEnabled) {
      auto ExtraChecks = [&t]() TA_REQ(t.get_lock()) -> void {
        const auto& ss = t.scheduler_state();
        const auto& ep = ss.effective_profile();
        const auto& bp = ss.base_profile_;
        const auto& ipv = ss.inherited_profile_values_;

        if (ep.dirty_flags() == ProfileDirtyFlag::Clean) {
          ASSERT_MSG(
              bp.IsDeadline() || (ipv.uncapped_utilization > SchedUtilization{0}),
              "Deadline thread %" PRIu64
              " has a clean effective profile, but neither a deadline base profile (%s), nor a "
              "non-zero inherited utilization (%" PRId64 ")",
              t.tid(), bp.IsFair() ? "Fair" : "Deadline", ipv.uncapped_utilization.raw_value());
        }
      };

      if (t.get_lock().is_held()) {
        t.get_lock().MarkHeld();
        ExtraChecks();
      } else if (t.get_lock().TryAcquire(ChainLock::AllowFinalized::Yes)) {
        ExtraChecks();
        t.get_lock().Release();
      }
    }
  };

  for (const auto& t : fair_run_queue_) {
    ObserveFairThread(t);
  }

  for (const auto& t : deadline_run_queue_) {
    ObserveDeadlineThread(t);
  }

  ASSERT(active_thread_ != nullptr);
  const Thread& active_thread = *active_thread_;
  MarkHasOwnedThreadAccess(active_thread);  // This should be safe, see above.
  if (active_thread.scheduler_state().effective_profile().IsFair()) {
    ObserveFairThread(active_thread);
  } else {
    ASSERT(active_thread.scheduler_state().effective_profile().IsDeadline());
    ObserveDeadlineThread(active_thread);
  }
}

void Scheduler::Block(Thread* const current_thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_block");

  DEBUG_ASSERT(current_thread == Thread::Current::Get());
  current_thread->canary().Assert();
  DEBUG_ASSERT(current_thread->state() != THREAD_RUNNING);

  const SchedTime now = CurrentTime();
  Scheduler::Get()->RescheduleCommon(current_thread, now, trace.Completer());
}

void Scheduler::Unblock(Thread* thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_unblock");
  ChainLockTransaction::ActiveRef().AssertFinalized();

  thread->canary().Assert();

  const SchedTime now = CurrentTime();
  cpu_num_t target_cpu = INVALID_CPU;
  while (true) {
    // TODO(rudymathu): This target_cpu should be stashed in the thread prior to
    // adding it to the save_state_list_, as that would allow us to bypass CPU
    // selection when processing the list so long as the CPU is still online.
    target_cpu = FindTargetCpu(thread);
    const cpu_num_t last_cpu = thread->scheduler_state().last_cpu();
    const bool needs_migration = (last_cpu != INVALID_CPU && target_cpu != last_cpu &&
                                  thread->has_migrate_fn() && !thread->migrate_pending());
    if (needs_migration) {
      target_cpu = last_cpu;
    }
    Scheduler* const target = Get(target_cpu);
    Guard<MonitoredSpinLock, NoIrqSave> target_queue_guard{&target->queue_lock_, SOURCE_TAG};

    if (target->IsSchedulerActiveLocked()) {
      TraceWakeup(thread, target_cpu);
      thread->set_ready();
      thread->UpdateRuntimeStats(thread->state());
      if (needs_migration) {
        MarkHasOwnedThreadAccess(*thread);
        target->save_state_list_.push_front(thread);
      } else {
        thread->scheduler_state().curr_cpu_ = target_cpu;
        target->AssertInScheduler(*thread);
        target->Insert(now, thread);
      }
      break;
    }

    IncFindTargetCpuRetriesKcounter();
  }

  trace.End();
  thread->get_lock().Release();
  RescheduleMask(cpu_num_to_mask(target_cpu));
}

void Scheduler::Unblock(Thread::UnblockList list) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_unblock_list");
  ChainLockTransaction::ActiveRef().AssertFinalized();

  const SchedTime now = CurrentTime();
  cpu_mask_t cpus_to_reschedule_mask = 0;

  Thread* thread;
  while ((thread = list.pop_back()) != nullptr) {
    thread->canary().Assert();
    DEBUG_ASSERT(!thread->IsIdle());
    thread->get_lock().AssertAcquired();

    cpu_num_t target_cpu = INVALID_CPU;
    while (true) {
      // TODO(rudymathu): This target_cpu should be stashed in the thread prior
      // to adding it to the save_state_list_, as that would allow us to bypass
      // CPU selection when processing the list so long as the CPU is still
      // online.
      target_cpu = FindTargetCpu(thread);
      const cpu_num_t last_cpu = thread->scheduler_state().last_cpu();
      const bool needs_migration = (last_cpu != INVALID_CPU && target_cpu != last_cpu &&
                                    thread->has_migrate_fn() && !thread->migrate_pending());
      if (needs_migration) {
        target_cpu = last_cpu;
      }
      Scheduler* const target = Get(target_cpu);
      Guard<MonitoredSpinLock, NoIrqSave> target_queue_guard{&target->queue_lock_, SOURCE_TAG};

      if (target->IsSchedulerActiveLocked()) {
        TraceWakeup(thread, target_cpu);
        thread->set_ready();
        thread->UpdateRuntimeStats(thread->state());
        if (needs_migration) {
          MarkHasOwnedThreadAccess(*thread);
          target->save_state_list_.push_front(thread);
        } else {
          thread->scheduler_state().curr_cpu_ = target_cpu;
          target->AssertInScheduler(*thread);
          target->Insert(now, thread);
        }
        break;
      }

      IncFindTargetCpuRetriesKcounter();
    }

    cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu);
    thread->get_lock().Release();
  }

  trace.End();
  RescheduleMask(cpus_to_reschedule_mask);
}

void Scheduler::UnblockIdle(Thread* thread) {
  SchedulerState* const state = &thread->scheduler_state();

  DEBUG_ASSERT(thread->IsIdle());
  DEBUG_ASSERT(ktl::popcount(state->hard_affinity_) == 1);

  const cpu_num_t target_cpu = lowest_cpu_set(state->hard_affinity_);
  Scheduler* const target_scheduler = Get(target_cpu);

  {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&target_scheduler->queue_lock_, SOURCE_TAG};
    thread->set_ready();
    state->curr_cpu_ = target_cpu;
    target_scheduler->AssertInScheduler(*thread);
    thread->scheduler_queue_state().OnInsert();
  }
}

void Scheduler::Yield(Thread* const current_thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_yield");
  DEBUG_ASSERT(current_thread == Thread::Current::Get());
  DEBUG_ASSERT(!current_thread->IsIdle());

  if (IsFairThread(current_thread)) {
    Scheduler& current_scheduler = *Get();
    SchedulerState& current_state = current_thread->scheduler_state();

    // If the thread is running in an eligible activation, expire the time slice
    // to push it into the next activation, potentially allowing other eligible
    // threads to run. However, if the start time is in the future, this thread
    // is running because there were no other eligible fair threads to run and
    // the eligible time was snapped forward. In that case, avoid pushing the
    // activation further into the future.
    const SchedTime now = CurrentTime();
    if (current_state.start_time() < now) {
      current_state.time_slice_used_ns_ = current_state.time_slice_ns_;
      current_scheduler.RescheduleCommon(current_thread, now, trace.Completer());
    }
  }
}

void Scheduler::Preempt() {
  Thread* current_thread = Thread::Current::Get();
  SingleChainLockGuard thread_guard{IrqSaveOption, current_thread->get_lock(),
                                    CLT_TAG("Scheduler::Preempt")};
  PreemptLocked(current_thread);
}

void Scheduler::PreemptLocked(Thread* current_thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_preempt");
  SchedulerState& current_state = current_thread->scheduler_state();
  const cpu_num_t current_cpu = arch_curr_cpu_num();

  DEBUG_ASSERT(current_state.curr_cpu_ == current_cpu);
  DEBUG_ASSERT(current_state.last_cpu_ == current_state.curr_cpu_);
  DEBUG_ASSERT(current_thread->state() == THREAD_RUNNING);

  const SchedTime now = CurrentTime();
  Get()->RescheduleCommon(current_thread, now, trace.Completer());
}

void Scheduler::Reschedule(Thread* const current_thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_reschedule");

  DEBUG_ASSERT(current_thread == Thread::Current::Get());
  SchedulerState* const current_state = &current_thread->scheduler_state();
  const cpu_num_t current_cpu = arch_curr_cpu_num();

  const bool preempt_enabled = current_thread->preemption_state().EvaluateTimesliceExtension();

  // Pend the preemption rather than rescheduling if preemption is disabled or
  // if there is more than one spinlock held.
  // TODO(https://fxbug.dev/42143537): Remove check when spinlocks imply preempt disable.
  if (!preempt_enabled || arch_num_spinlocks_held() > 1 || arch_blocking_disallowed()) {
    current_thread->preemption_state().preempts_pending_add(cpu_num_to_mask(current_cpu));
    return;
  }

  DEBUG_ASSERT(current_state->curr_cpu_ == current_cpu);
  DEBUG_ASSERT(current_state->last_cpu_ == current_state->curr_cpu_);

  const SchedTime now = CurrentTime();
  Get()->RescheduleCommon(current_thread, now, trace.Completer());
}

void Scheduler::RescheduleInternal(Thread* const current_thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_resched_internal");
  Get()->RescheduleCommon(current_thread, CurrentTime(), trace.Completer());
}

void Scheduler::MigrateUnpinnedThreads() {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_migrate_unpinned");

  const cpu_num_t current_cpu = arch_curr_cpu_num();
  const cpu_mask_t current_cpu_mask = cpu_num_to_mask(current_cpu);
  cpu_mask_t cpus_to_reschedule_mask = 0;

  // Flag this scheduler as being in the process de-activating.  If a thread
  // with a migration function unblocks and needs to become READY while we are
  // in the middle of MigrateUnpinnedThreads, we will allow it to join this
  // CPU's ready-queue knowing that we are going to immediately call the
  // thread's migration function (on this CPU, as required) before reassigning
  // the thread to a different CPU.
  //
  // Note: Because of the nature of the global thread-lock, this race is not
  // currently possible (Threads who are blocked cannot unblock while we hold
  // the global thread lock, which we currently do).  Once the thread lock has
  // been removed, however, this race will become an important edge case to
  // handle.
  const SchedTime now = CurrentTime();
  Scheduler* const current = Get(current_cpu);
  current->cpu_deactivating_.store(true, ktl::memory_order_release);

  // Flag this CPU as being inactive.  This will prevent new threads from being
  // added to this CPU's scheduler's run queues while we migrate the existing
  // threads off this scheduler.
  //
  // Note that this does _not_ prevent FindTargetCpu from _selecting_ a given
  // scheduler as potential target for thread placement, but it _does_ guarantee
  // that by the time we attempt to actually add the thread to the chosen
  // scheduler's queue, we will have had a chance to check the flag once again,
  // this time safely from with the chosen scheduler's queue lock.
  Scheduler::SetCurrCpuActive(false);

  // Call all migrate functions for threads last run on the current CPU who are
  // not currently READY.  The next time these threads unblock or wake up, the
  // will be assigned to a different CPU, and we will have already called their
  // migrate function for them.  When we are finished with this pass, the only
  // threads who have a migrate function still to be called should only exist in
  // this scheduler's READY queue, because:
  //
  // 1)  New threads or threads who have not recently run on this CPU cannot be
  //     assigned to this CPU (because we cleared our active flag)
  // 2a) Threads who were not ready but needed to have their migration function
  //     called were found and had their migration function called during
  //     CallMigrateFnForCpu  --- OR ---
  // 2b) The non-ready thread with a migration function unblocked and joined the
  //     ready queue of this scheduler while it was being deactivated.  We will
  //     call their migration functions next as we move all of the READY threads
  //     off of this scheduler and over to a different one.
  //
  Thread::CallMigrateFnForCpu(current_cpu);

  // There should no longer be any non-ready threads who need to run a
  // migration function on this CPU, and there should not be any new one until
  // we set this CPU back to being active.  We can clear the transitory
  // "cpu_deactivating_" flag now.
  DEBUG_ASSERT(current->cpu_deactivating_.load(ktl::memory_order_relaxed));
  current->cpu_deactivating_.store(false, ktl::memory_order_release);

  // Now move any READY threads who can be moved over to a temporary list,
  // flagging them as in the process of currently migrating. Also move all
  // of the threads in the save_state_list into a temporary list, as they
  // need to be migrated as well.
  Thread::UnblockList migrating_deadline_threads;
  Thread::UnblockList migrating_fair_threads;
  Thread::SaveStateList local_save_state_list;
  Affine affine_transform;
  {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&current->queue_lock_, SOURCE_TAG};

    // Save the current affine transform to update fair threads as they are migrated.
    affine_transform = current->fair_affine_transform_;

    if (!current->save_state_list_.is_empty()) {
      local_save_state_list = ktl::move(current->save_state_list_);
    }

    auto MoveThreads = [current, current_cpu_mask, now](
                           Thread::UnblockList& thread_list, RunQueue& run_queue,
                           const fxt::InternedString& trace_tag) TA_REQ(current->queue_lock_) {
      DEBUG_ASSERT((&run_queue == &current->fair_run_queue_) ||
                   (&run_queue == &current->deadline_run_queue_));

      for (RunQueue::iterator iter = run_queue.begin(); iter.IsValid();) {
        Thread& consider = *(iter++);

        // The no-op version of this assertion should be sufficient here.  We
        // are holding the scheduler's queue lock, and iterating over the
        // scheduler's run queues.  All of the threads in those queues are
        // clearly owned by this scheduler right now.
        MarkHasOwnedThreadAccess(consider);
        const SchedulerState& ss = const_cast<const Thread&>(consider).scheduler_state();

        // If the thread can run on any other CPU, take it off of this one.
        // Flag it as migrating as we do, and add it to our list of migrating
        // threads.
        if (ss.hard_affinity_ != current_cpu_mask) {
          current->TraceThreadQueueEvent(trace_tag, &consider);
          run_queue.erase(consider);
          current->Remove(now, &consider);
          thread_list.push_back(&consider);
        }
      }
    };

    MoveThreads(migrating_fair_threads, current->fair_run_queue_,
                "tqe_deque_migrate_unpinned_fair"_intern);
    MoveThreads(migrating_deadline_threads, current->deadline_run_queue_,
                "tqe_deque_migrate_unpinned_deadline"_intern);
  }

  // OK, now that we have dropped our queue lock, go over our list of migrating
  // threads and finish the migration process.  Don't make any assumptions about
  // the fair/deadline nature of the threads as we migrate.  Since we dropped
  // the scheduler's queue lock, it is possible that these threads have changed
  // effective priority since being remove from their old scheduler, but before
  // arriving at a new one.
  auto MigrateThread = [current, now, &affine_transform](Thread* thread,
                                                         bool update_timeline) -> cpu_num_t {
    SingleChainLockGuard guard{IrqSaveOption, thread->get_lock(),
                               CLT_TAG("Scheduler::MigrateUnpinnedThreads")};

    // Threads removed from the fair run queue must be transformed to the monotonic timeline
    // before migrating.
    if (update_timeline) {
      DEBUG_ASSERT(IsFairThread(thread));
      affine_transform.VariableToMonotonicInPlace(thread->scheduler_state().start_time_);
      affine_transform.VariableToMonotonicInPlace(thread->scheduler_state().finish_time_);
    }

    // Call the Save stage of the thread's migration function as it leaves
    // this CPU.
    thread->CallMigrateFnLocked(Thread::MigrateStage::Save);

    const cpu_num_t target_cpu =
        FindActiveSchedulerForThread(thread, [current, now](Thread* thread, Scheduler* target) {
          // Finish the transition and add the thread to the new target queue.  The
          // Restore stage of the migration function will be called on the new CPU the
          // next time the thread becomes scheduled.
          MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
          (void)current;  // We cannot annotate 'current' with [[maybe_unused]] in the capture
                          // list, so suppress the warning using the old school void-cast trick.
          DEBUG_ASSERT(target != current);
          target->Insert(now, thread, Placement::Migration);
        });

    return target_cpu;
  };

  while (!migrating_fair_threads.is_empty()) {
    Thread* thread = migrating_fair_threads.pop_front();
    const cpu_num_t target_cpu = MigrateThread(thread, /*update_timeline=*/true);
    cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu);
  }

  while (!migrating_deadline_threads.is_empty()) {
    Thread* thread = migrating_deadline_threads.pop_front();
    const cpu_num_t target_cpu = MigrateThread(thread, /*update_timeline=*/false);
    cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu);
  }

  while (!local_save_state_list.is_empty()) {
    Thread* thread = local_save_state_list.pop_front();
    const cpu_num_t target_cpu = MigrateThread(thread, /*update_timeline=*/false);
    cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu);
  }

  trace.End();
  RescheduleMask(cpus_to_reschedule_mask);
}

void Scheduler::TimerTick(SchedTime now) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_timer_tick");
  Thread::Current::preemption_state().PreemptSetPending();
}

void Scheduler::InitializePerformanceScale(SchedPerformanceScale scale)
    TA_NO_THREAD_SAFETY_ANALYSIS {
  // This happens early in boot, before the scheduler is actually running.  We
  // should be able to treat these assignments as if they were done in a
  // constructor and skip obtaining the queue lock.
  DEBUG_ASSERT(scale > 0);
  performance_scale_ = scale;
  default_performance_scale_ = scale;
  pending_user_performance_scale_ = scale;
  performance_scale_reciprocal_ = 1 / scale;
}

void Scheduler::UpdatePerformanceScales(zx_cpu_performance_info_t* info, size_t count) {
  DEBUG_ASSERT(count <= percpu::processor_count());
  InterruptDisableGuard irqd;

  cpu_num_t cpus_to_reschedule_mask = 0;
  for (auto& entry : ktl::span{info, count}) {
    DEBUG_ASSERT(entry.logical_cpu_number <= percpu::processor_count());

    cpus_to_reschedule_mask |= cpu_num_to_mask(entry.logical_cpu_number);
    Scheduler* scheduler = Scheduler::Get(entry.logical_cpu_number);
    Guard<MonitoredSpinLock, NoIrqSave> guard{&scheduler->queue_lock_, SOURCE_TAG};

    // TODO(eieio): Apply a minimum value threshold and update the entry if
    // the requested value is below it.
    scheduler->pending_user_performance_scale_ = ToSchedPerformanceScale(entry.performance_scale);

    // Return the original performance scale.
    entry.performance_scale = ToUserPerformanceScale(scheduler->performance_scale());
  }

  RescheduleMask(cpus_to_reschedule_mask);
}

void Scheduler::GetPerformanceScales(zx_cpu_performance_info_t* info, size_t count) {
  DEBUG_ASSERT(count <= percpu::processor_count());
  for (cpu_num_t i = 0; i < count; i++) {
    Scheduler* scheduler = Scheduler::Get(i);
    Guard<MonitoredSpinLock, IrqSave> guard{&scheduler->queue_lock_, SOURCE_TAG};
    info[i].logical_cpu_number = i;
    info[i].performance_scale = ToUserPerformanceScale(scheduler->pending_user_performance_scale_);
  }
}

void Scheduler::GetDefaultPerformanceScales(zx_cpu_performance_info_t* info, size_t count) {
  DEBUG_ASSERT(count <= percpu::processor_count());
  for (cpu_num_t i = 0; i < count; i++) {
    Scheduler* scheduler = Scheduler::Get(i);
    Guard<MonitoredSpinLock, IrqSave> guard{&scheduler->queue_lock_, SOURCE_TAG};
    info[i].logical_cpu_number = i;
    info[i].performance_scale = ToUserPerformanceScale(scheduler->default_performance_scale_);
  }
}

cpu_mask_t Scheduler::SetCpuAffinity(Thread& thread, cpu_mask_t affinity,
                                     AffinityType affinity_type) {
  DEBUG_ASSERT_MSG(
      const cpu_mask_t active_mask = PeekActiveMask();
      affinity_type == AffinityType::Soft || (affinity & active_mask) != 0,
      "Attempted to set affinity mask to %#x, which has no overlap of active CPUs %#x.", affinity,
      active_mask);

  // Utility to set the correct affinity mask based on the affinity type. Uses a
  // function pointer conversion (i.e. +) to avoid a bug with static annotations
  // on some lambdas, where the compiler complains about not holding the lambda
  // as a capability.
  const auto set_affinity = +[](Thread& thread, cpu_mask_t affinity, AffinityType affinity_type)
                                 TA_REQ(thread.get_lock()) -> cpu_mask_t {
    switch (affinity_type) {
      case AffinityType::Hard:
        ktl::swap(thread.scheduler_state().hard_affinity_, affinity);
        break;

      case AffinityType::Soft:
        ktl::swap(thread.scheduler_state().soft_affinity_, affinity);
        break;
    }
    return affinity;
  };

  const auto do_transaction =
      [&]() TA_REQ(chainlock_transaction_token) -> ChainLockTransaction::Result<cpu_mask_t> {
    thread.get_lock().AcquireFirstInChain();

    // Mutating the affinity mask in the scheduler state requires holding both
    // the thread lock and the container lock, if any.
    //
    // TODO(johngro): It is really easy to mess this requirement up. Is there a
    // better way to statically assert these requirements?
    //
    // TODO(eieio): Why does the wait queue lock need to be held when updating
    // the affinity mask of a blocked thread? Only scheduling needs to read the
    // affinity mask and make decisions based on it, and even then only tracing
    // needs to read the affinity mask while holding only the queue lock. How
    // are wait queues different than the no container case wrt affinity?

    // The thread container is a scheduler.
    if ((thread.state() == THREAD_RUNNING) || (thread.state() == THREAD_READY)) {
      ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_migrate");

      ChainLockTransaction::Finalize();
      SchedulerState& state = thread.scheduler_state();

      DEBUG_ASSERT(state.curr_cpu() != INVALID_CPU);
      const cpu_mask_t thread_cpu_mask = cpu_num_to_mask(state.curr_cpu());

      cpu_mask_t cpus_to_reschedule_mask = 0;
      bool find_new_target_cpu = false;
      cpu_mask_t previous_affinity = 0;

      // Use a lambda to prevent the AssertInScheduler from leaking out of the
      // scope of the queue lock guard. Static lock assertions can leak out of
      // nested scopes, including conditionals and loops.
      [&]() TA_REQ(thread.get_lock()) {
        Scheduler& scheduler = *Scheduler::Get(state.curr_cpu());
        Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler.queue_lock(), SOURCE_TAG};

        scheduler.AssertInScheduler(thread);
        SchedulerQueueState& queue_state = thread.scheduler_queue_state();
        const Disposition disposition = queue_state.disposition();

        // Set the affinity before determining if the thread needs to move.
        previous_affinity = set_affinity(thread, affinity, affinity_type);

        const cpu_mask_t effective_cpu_mask = state.GetEffectiveCpuMask(PeekActiveMask());
        const bool stale_curr_cpu = (thread_cpu_mask & effective_cpu_mask) == 0;

        if (thread.state() == THREAD_RUNNING) {
          DEBUG_ASSERT(disposition == Disposition::Associated);
          if (stale_curr_cpu) {
            cpus_to_reschedule_mask |= thread_cpu_mask;
          }
        } else {
          DEBUG_ASSERT(thread.state() == THREAD_READY);
          DEBUG_ASSERT(disposition != Disposition::Unassociated);

          if (disposition == Disposition::Associated) {
            // The thread is transitioning to RUNNING: curr_cpu is correct.
            CountUpdateInTransition();
            if (stale_curr_cpu) {
              cpus_to_reschedule_mask |= thread_cpu_mask;
            }
          } else if (disposition == Disposition::Stolen) {
            // The thread is being stolen: curr_cpu is incorrect, use stolen_by.
            CountUpdateInTransition();
            DEBUG_ASSERT(queue_state.stolen_by() != INVALID_CPU);
            const cpu_mask_t stealing_cpu_mask = cpu_num_to_mask(queue_state.stolen_by());
            const bool stale_stealing_cpu = (stealing_cpu_mask & effective_cpu_mask) == 0;
            if (stale_stealing_cpu) {
              cpus_to_reschedule_mask |= stealing_cpu_mask;
            }
          } else {
            // The READY thread is waiting in the run queue: curr_cpu is correct.
            DEBUG_ASSERT(disposition == Disposition::Enqueued);
            if (stale_curr_cpu) {
              scheduler.EraseFromQueue(&thread);
              scheduler.Remove(CurrentTime(), &thread);

              // Execute the migrate function inline if the thread is on the local
              // CPU. Otherwise, add it to the save state list of the last CPU it
              // ran on and reschedule that CPU to process the list.
              if (thread.has_migrate_fn() && !thread.migrate_pending() &&
                  state.last_cpu() != INVALID_CPU && state.last_cpu() != arch_curr_cpu_num()) {
                scheduler.save_state_list_.push_front(&thread);
                cpus_to_reschedule_mask |= thread_cpu_mask;
              } else {
                find_new_target_cpu = true;
              }
            }
          }
        }
      }();

      // Find a new target CPU for the thread.
      if (find_new_target_cpu) {
        DEBUG_ASSERT(thread.state() == THREAD_READY);
        // Call the migration function if the thread is migrating away from this
        // CPU. This is a no-op if there is no migration function or the Save
        // state has already been invoked.
        if (state.last_cpu() == arch_curr_cpu_num()) {
          thread.CallMigrateFnLocked(Thread::MigrateStage::Save);
        }

        // Find a new CPU for the thread and add it to the queue.
        const cpu_num_t target_cpu =
            FindActiveSchedulerForThread(&thread, [](Thread* thread, Scheduler* target) {
              MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
              target->Insert(CurrentTime(), thread, Placement::Migration);
            });

        // Reschedule both CPUs to handle the run queue changes.
        cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu) | thread_cpu_mask;
      }

      trace.End();
      thread.get_lock().Release();
      RescheduleMask(cpus_to_reschedule_mask);
      return previous_affinity;
    }

    // The thread container is a wait queue.
    // TODO(eieio): Why do we need to synchronize the affinity mask with the
    // wait queue?
    if (thread.state() == THREAD_BLOCKED) {
      WaitQueue* wait_queue = thread.wait_queue_state().blocking_wait_queue_;
      DEBUG_ASSERT(wait_queue != nullptr);

      if (!wait_queue->get_lock().AcquireOrBackoff()) {
        thread.get_lock().Release();
        return ChainLockTransaction::Action::Backoff;
      }

      ChainLockTransaction::Finalize();
      const cpu_mask_t previous_affinity = set_affinity(thread, affinity, affinity_type);

      wait_queue->get_lock().Release();
      thread.get_lock().Release();
      return previous_affinity;
    }

    // The thread has no container (e.g. SLEEPING, SUSPENDED).
    ChainLockTransaction::Finalize();
    const cpu_mask_t previous_affinity = set_affinity(thread, affinity, affinity_type);

    thread.get_lock().Release();
    return previous_affinity;
  };

  return ChainLockTransaction::UntilDone(IrqSaveOption, CLT_TAG("Scheduler::SetCpuAffinity"),
                                         do_transaction);
}

void Scheduler::RequestPowerLevelForTesting(uint8_t power_level) {
  InterruptDisableGuard interrupts_disabled;
  bool need_reschedule;
  {
    Guard<MonitoredSpinLock, NoIrqSave> guard{&queue_lock_, SOURCE_TAG};
    need_reschedule = power_level_control_.RequestPowerLevel(power_level);
  }
  if (need_reschedule) {
    RescheduleMask(cpu_num_to_mask(this_cpu()));
  }
}

bool Scheduler::PowerLevelControl::RequestPowerLevel(uint8_t power_level) {
  // If there is already a pending request, it can be updated without issuing a reschedule,
  // since this update raced with dispatch of the previous request and won.
  const bool had_pending_request = pending_update_request_.has_value();
  pending_update_request_ = power_state_.RequestTransition(cpu(), power_level);
  return !had_pending_request && pending_update_request_.has_value();
}

void Scheduler::PowerLevelControl::TimerHandler(Timer* timer, zx_instant_mono_t now, void* arg) {
  // Only queue the DPC if the timer handler is running on the expected CPU. If the timer handler
  // is running on a different CPU, the CPU it services went offline while the timer was pending
  // and its power level requests are no longer relevant.
  PowerLevelControl* power_level_control = static_cast<PowerLevelControl*>(arg);
  if (power_level_control->cpu() == arch_curr_cpu_num()) {
    DpcRunner::Enqueue(power_level_control->request_dpc_);
  }
}

void Scheduler::PowerLevelControl::DpcHandler(Dpc* dpc) {
  Scheduler* scheduler = dpc->arg<Scheduler>();
  fbl::RefPtr<power_management::PowerDomain> domain;
  ktl::optional<power_management::PowerLevelUpdateRequest> request;

  {
    Guard<MonitoredSpinLock, IrqSave> guard{&scheduler->queue_lock_, SOURCE_TAG};
    domain = scheduler->power_level_control_.power_state_.domain();
    request.swap(scheduler->power_level_control_.pending_update_request_);
  }

  if (domain && domain->controller()->is_serving() && request.has_value()) {
    const zx::result result = domain->controller()->Post(*request);
    ASSERT(result.status_value() != ZX_ERR_SHOULD_WAIT);
  }
}
