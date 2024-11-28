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

// Counts the number of times we had to run FindTargetCpu again because our
// selected Target became in-active after we chose it.
KCOUNTER(counter_find_target_cpu_retries, "scheduler.find_target_cpu.retries")

namespace {

// The minimum possible weight and its reciprocal.
constexpr SchedWeight kMinWeight = SchedulerState::ConvertPriorityToWeight(LOWEST_PRIORITY);
constexpr SchedWeight kReciprocalMinWeight = 1 / kMinWeight;

// Utility operator to make expressions more succinct that update thread times
// and durations of basic types using the fixed-point counterparts.
constexpr zx_time_t& operator+=(zx_time_t& value, SchedDuration delta) {
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
void PreemptReset(cpu_num_t current_cpu, zx_time_t now, zx_time_t deadline) {
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
    const zx_time_t now = current_time();  // TODO(johngro): plumb this in from above
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

    ktrace_probe(TraceAlways, TraceContext::Cpu, name, arg0, arg1);
  }
}

void Scheduler::Dump(FILE* output_target) {
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

    fprintf(output_target,
            "\ttweight=%s nfair=%d ndeadline=%d vtime=%" PRId64 " period=%" PRId64 " tema=%" PRId64
            " tutil=%s\n",
            Format(weight_total_).c_str(), runnable_fair_task_count_, runnable_deadline_task_count_,
            virtual_time_.raw_value(), scheduling_period_grans_.raw_value(),
            total_expected_runtime_ns_.raw_value(), Format(total_deadline_utilization_).c_str());

    if (active_thread_ != nullptr) {
      AssertInScheduler(*active_thread_);
      const SchedulerState& state = const_cast<const Thread*>(active_thread_)->scheduler_state();
      const EffectiveProfile& ep = state.effective_profile();
      if (ep.IsFair()) {
        fprintf(output_target,
                "\t-> name=%s weight=%s start=%" PRId64 " finish=%" PRId64 " ts=%" PRId64
                " ema=%" PRId64 "\n",
                active_thread_->name(), Format(ep.fair.weight).c_str(),
                state.start_time_.raw_value(), state.finish_time_.raw_value(),
                state.time_slice_ns_.raw_value(), state.expected_runtime_ns_.raw_value());
      } else {
        fprintf(output_target,
                "\t-> name=%s deadline=(%" PRId64 ", %" PRId64 ") start=%" PRId64 " finish=%" PRId64
                " ts=%" PRId64 " ema=%" PRId64 "\n",
                active_thread_->name(), ep.deadline.capacity_ns.raw_value(),
                ep.deadline.deadline_ns.raw_value(), state.start_time_.raw_value(),
                state.finish_time_.raw_value(), state.time_slice_ns_.raw_value(),
                state.expected_runtime_ns_.raw_value());
      }
    }

    for (const Thread& thread : deadline_run_queue_) {
      AssertInScheduler(thread);
      const SchedulerState& state = thread.scheduler_state();
      const EffectiveProfile& ep = state.effective_profile();
      fprintf(output_target,
              "\t   name=%s deadline=(%" PRId64 ", %" PRId64 ") start=%" PRId64 " finish=%" PRId64
              " ts=%" PRId64 " ema=%" PRId64 "\n",
              thread.name(), ep.deadline.capacity_ns.raw_value(),
              ep.deadline.deadline_ns.raw_value(), state.start_time_.raw_value(),
              state.finish_time_.raw_value(), state.time_slice_ns_.raw_value(),
              state.expected_runtime_ns_.raw_value());
    }

    for (const Thread& thread : fair_run_queue_) {
      AssertInScheduler(thread);
      const SchedulerState& state = thread.scheduler_state();
      const EffectiveProfile& ep = state.effective_profile();
      fprintf(output_target,
              "\t   name=%s weight=%s start=%" PRId64 " finish=%" PRId64 " ts=%" PRId64
              " ema=%" PRId64 "\n",
              thread.name(), Format(ep.fair.weight).c_str(), state.start_time_.raw_value(),
              state.finish_time_.raw_value(), state.time_slice_ns_.raw_value(),
              state.expected_runtime_ns_.raw_value());
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
  const int64_t total_runnable_tasks = runnable_fair_task_count_ + runnable_deadline_task_count_;
  return static_cast<size_t>(total_runnable_tasks);
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
  return FindEarliestEligibleThread(run_queue, eligible_time, [](const auto iter) { return true; });
}

template <typename Predicate>
Thread* Scheduler::FindEarliestEligibleThread(RunQueue* run_queue, SchedTime eligible_time,
                                              Predicate&& predicate) {
  auto GetSchedulerState = [this](const Thread& t)
                               TA_REQ(this->queue_lock_) -> const SchedulerState& {
    AssertInScheduler(t);
    return t.scheduler_state();
  };

  // Early out if there is no eligible thread.
  if (run_queue->is_empty() || GetSchedulerState(run_queue->front()).start_time_ > eligible_time) {
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
    const SchedulerState& node_state = GetSchedulerState(*node);

    if (node_state.start_time_ <= eligible_time) {
      if (!path || GetSchedulerState(*path).finish_time_ > node_state.finish_time_) {
        path = node;
      }

      if (auto left = node.left();
          !subtree || (left && (GetSchedulerState(*subtree).min_finish_time_ >
                                GetSchedulerState(*left).min_finish_time_))) {
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

  const SchedulerState& subtree_state = GetSchedulerState(*subtree);
  if ((subtree_state.min_finish_time_ >= GetSchedulerState(*path).finish_time_) && accept(path)) {
    return path.CopyPointer();
  }

  // Find the node with the earliest finish time among the descendants of the
  // subtree with the smallest minimum finish time.
  node = subtree;
  do {
    const SchedulerState& node_state = GetSchedulerState(*node);

    if ((subtree_state.min_finish_time_ == node_state.finish_time_) && accept(node)) {
      return node.CopyPointer();
    }

    if (auto left = node.left();
        left && (node_state.min_finish_time_ == GetSchedulerState(*left).min_finish_time_)) {
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
      profile.IsFair() ? kDefaultMinimumGranularity : profile.deadline.capacity_ns;
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
  SchedulerState& ss = thread->scheduler_state();
  ss.state_ = THREAD_RUNNING;
  ss.curr_cpu_ = current_cpu;
  ss.last_cpu_ = current_cpu;
  ss.hard_affinity_ = cpu_num_to_mask(current_cpu);

  // Finally, make sure that the thread is the active thread for the scheduler,
  // and that the weight_total bookkeeping is accurate.
  {
    Scheduler* sched = Get(current_cpu);
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&sched->queue_lock_, SOURCE_TAG};
    sched->AssertInScheduler(*thread);

    SchedulerQueueState& sqs = thread->scheduler_queue_state();
    sqs.active = true;
    sched->active_thread_ = thread;

    sched->weight_total_ = ss.effective_profile_.fair.weight;
    sched->runnable_fair_task_count_++;
    sched->UpdateTotalExpectedRuntime(ss.expected_runtime_ns_);
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
  Scheduler* sched = Get(current_cpu);
  SchedulerState& ss = thread->scheduler_state();

  // Since this is becoming an idle thread, it must have been one of the CPU's
  // first threads.  It should already be bound to this core with hard affinity.
  // Assert this.
  DEBUG_ASSERT(ss.last_cpu_ == current_cpu);
  DEBUG_ASSERT(ss.curr_cpu_ == current_cpu);
  DEBUG_ASSERT(ss.hard_affinity_ == cpu_num_to_mask(current_cpu));

  {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&sched->queue_lock_, SOURCE_TAG};
    sched->AssertInScheduler(*thread);
    SchedulerQueueState& sqs = thread->scheduler_queue_state();

    // We are becoming the idle thread.  We should currently be running with a
    // fair (not deadline) profile, and we should not be holding any locks
    // (therefore, we should not be inheriting any profile pressure).
    DEBUG_ASSERT(ss.base_profile_.IsFair());
    DEBUG_ASSERT(ss.inherited_profile_values_.total_weight == SchedWeight{0});
    DEBUG_ASSERT(ss.inherited_profile_values_.uncapped_utilization == SchedUtilization{0});

    // We should also be the currently active thread on this core, but no
    // longer.  We are about to either exit, or "UnblockIdle".
    DEBUG_ASSERT(sched->active_thread_ == thread);
    DEBUG_ASSERT(sched->runnable_fair_task_count_ > 0);
    sqs.active = false;
    sched->active_thread_ = nullptr;
    sched->weight_total_ -= ss.effective_profile_.fair.weight;
    sched->runnable_fair_task_count_--;
    sched->UpdateTotalExpectedRuntime(-ss.expected_runtime_ns_);

    ss.base_profile_.fair.weight = SchedulerState::ConvertPriorityToWeight(IDLE_PRIORITY);
    ss.effective_profile_.MarkBaseProfileChanged();
    ss.RecomputeEffectiveProfile();
  }
}

// Removes the thread at the head of the first eligible run queue. If there is
// an eligible deadline thread, it takes precedence over available fair
// threads. If there is no eligible work, attempt to steal work from other busy
// CPUs.
Thread* Scheduler::DequeueThread(SchedTime now, Guard<MonitoredSpinLock, NoIrqSave>& queue_guard) {
  percpu& self = percpu::Get(this_cpu_);
  if (self.idle_power_thread.pending_power_work()) {
    return &self.idle_power_thread.thread();
  }

  if (IsDeadlineThreadEligible(now)) {
    return DequeueDeadlineThread(now);
  }
  if (likely(!fair_run_queue_.is_empty())) {
    return DequeueFairThread();
  }

  // Release the queue lock while attempting to steal work, leaving IRQs
  // disabled.  Latch our scale up factor to use while determining whether or
  // not we can steal a given thread before we drop our lock.
  Thread* thread;
  SchedPerformanceScale scale_up_factor = performance_scale_reciprocal_.load();
  queue_guard.CallUnlocked([&] {
    ChainLockTransaction::AssertActive();
    thread = StealWork(now, scale_up_factor);
  });

  // If we successfully stole a thread, it should now be in the Rescheduling
  // transient state.  It needed to be marked as such to prevent PI and
  // Migration code from messing with its (new) scheduler bookkeeping while we
  // were not holding that scheduler's lock.
  //
  // Now that we have re-obtained the scheduler lock, go ahead an clear the
  // Rescheduling flag so that the stolen thread looks exactly like a thread
  // that had been dequeued.
  //
  // TODO(johngro): reconsider this approach.  We need to hold both the thread's
  // lock and this scheduler's lock in order to add it to this scheduler.  The
  // thread's lock must be acquired before the scheduler's lock, which happens
  // at the end of StealWork.  After that, however, we need to drop both locks
  // before returning from StealWork, only to immediately re-acquire the queue
  // lock and return the dequeued thread (without the dequeued thread's lock
  // held).
  //
  // If we changed the semantics of Dequeue thread to return a _locked_ thread,
  // we could have StealWork return a thread which was in the Stolen transient
  // state instead.  Then we could lock the thread, and re-lock our scheduler's
  // queue, and return a locked thread to RescheduleCommon.  This would mean
  // that we don't have to drop the scheduler's lock later on in order to finish
  // the reschedule (or migration) operation.
  if (thread != nullptr) {
    AssertInScheduler(*thread);
    SchedulerQueueState& sqs = thread->scheduler_queue_state();
    DEBUG_ASSERT(sqs.transient_state == SchedulerQueueState::TransientState::Rescheduling);
    sqs.transient_state = SchedulerQueueState::TransientState::None;
    return thread;
  }

  return &self.idle_power_thread.thread();
}

// Attempts to steal work from other busy CPUs and move it to the local run
// queues. Returns a pointer to the stolen thread that is now associated with
// the local Scheduler instance, or nullptr is no work was stolen.
Thread* Scheduler::StealWork(SchedTime now, SchedPerformanceScale scale_up_factor) {
  using TransientState = SchedulerQueueState::TransientState;
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "StealWork");

  const cpu_num_t current_cpu = this_cpu();
  const cpu_mask_t current_cpu_mask = cpu_num_to_mask(current_cpu);
  const cpu_mask_t active_cpu_mask = PeekActiveMask();

  Thread* thread = nullptr;
  const CpuSearchSet& search_set = percpu::Get(current_cpu).search_set;
  for (const auto& entry : search_set.const_iterator()) {
    if (entry.cpu != current_cpu && active_cpu_mask & cpu_num_to_mask(entry.cpu)) {
      Scheduler* const queue = Get(entry.cpu);

      // Only steal across clusters if the target is above the load threshold.
      if (cluster() != entry.cluster &&
          queue->predicted_queue_time_ns() <= kInterClusterThreshold) {
        continue;
      }

      Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&queue->queue_lock_, SOURCE_TAG};

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

      // Common routine for stealing from a RunQueue when we find a thread we
      // are interested in.
      const auto StealFromQueue = [check_affinity, queue](RunQueue& rq, Thread& thread) {
        // check_affinity is only used in a DEBUG_ASSERT.  Ideally we'd annotate
        // it with [[maybe_unused]], but we can't do that in a lambda capture.
        ktl::ignore = check_affinity;

        MarkHasOwnedThreadAccess(thread);
        MarkHasSchedulerAccess(*queue);

        DEBUG_ASSERT(!thread.has_migrate_fn());
        DEBUG_ASSERT(check_affinity(thread));
        DEBUG_ASSERT((&rq == &queue->fair_run_queue_) || (&rq == &queue->deadline_run_queue_));

        rq.erase(thread);
        queue->RemoveForTransition(&thread, TransientState::Stolen);
        queue->TraceThreadQueueEvent("tqe_deque_steal_work"_intern, &thread);
      };

      // Returns true if the given thread in the run queue meets the criteria to
      // run on this CPU.  Don't attempt to steal any threads which are
      // currently in the process of being scheduled.
      const auto deadline_predicate = [check_affinity, scale_up_factor](const auto iter) -> bool {
        const Thread& t = *iter;
        MarkHasOwnedThreadAccess(t);

        const SchedulerState& state = t.scheduler_state();
        const SchedulerQueueState& sqs = t.scheduler_queue_state();
        const EffectiveProfile& ep = state.effective_profile_;
        const SchedUtilization scaled_utilization = ep.deadline.utilization * scale_up_factor;
        const bool is_scheduleable = scaled_utilization <= kThreadUtilizationMax;
        const bool transition_in_progress = sqs.transient_state != TransientState::None;

        return !transition_in_progress && check_affinity(t) && is_scheduleable &&
               !t.has_migrate_fn();
      };

      // Attempt to find a deadline thread that can run on this CPU.
      thread =
          queue->FindEarliestEligibleThread(&queue->deadline_run_queue_, now, deadline_predicate);
      if (thread != nullptr) {
        StealFromQueue(queue->deadline_run_queue_, *thread);
        break;
      }

      // Returns true if the given thread in the run queue meets the criteria to
      // run on this CPU.
      //
      // See the arguments for |deadline_predicate| (above) for why we can
      // disable static analysis here.
      const auto fair_predicate = [check_affinity](const auto iter) -> bool {
        const Thread& t = *iter;
        MarkHasOwnedThreadAccess(t);

        const SchedulerQueueState& sqs = t.scheduler_queue_state();

        const bool transition_in_progress = sqs.transient_state != TransientState::None;
        return !transition_in_progress && check_affinity(t) && !t.has_migrate_fn();
      };

      // TODO(eieio): Revisit the eligibility time parameter if/when moving to
      // WF2Q.
      queue->UpdateTimeline(now);
      SchedTime eligible_time = queue->virtual_time_;
      for (auto iter = queue->fair_run_queue_.cbegin(); iter.IsValid();) {
        const Thread& earliest_thread = *iter;
        MarkHasOwnedThreadAccess(earliest_thread);

        // Skip the first thread is it is in the process of being rescheduled.
        // We cannot steal that one.
        const SchedulerQueueState& sqs = earliest_thread.scheduler_queue_state();
        const bool reschedule_in_progress = sqs.transient_state == TransientState::Rescheduling;
        if (reschedule_in_progress) {
          ++iter;
          continue;
        }

        const SchedTime earliest_start = earliest_thread.scheduler_state().start_time_;
        eligible_time = ktl::max(eligible_time, earliest_start);
        break;
      }

      thread =
          queue->FindEarliestEligibleThread(&queue->fair_run_queue_, eligible_time, fair_predicate);
      if (thread != nullptr) {
        StealFromQueue(queue->fair_run_queue_, *thread);
        break;
      }
    }
  }

  if (thread) {
    // Associate the thread with this Scheduler, but don't enqueue it. It
    // will run immediately on this CPU as if dequeued from a local queue.
    ChainLockTransaction& active_clt = ChainLockTransaction::ActiveRef();
    active_clt.Restart(CLT_TAG("Scheduler::StealWork (restart)"));

    // Acquire the lock without backing off using an explicit retry loop because the current
    // thread's lock is held and ChainLock::Acquire asserts that no chain locks are currently
    // held to avoid misuse in the general case.
    ChainLockGuard thread_guard{thread->get_lock(), ChainLockGuard::Defer};
    while (!thread_guard.TryAcquire(ChainLock::AllowFinalized::No, ChainLock::RecordBackoff::No)) {
      arch::Yield();
    }

    active_clt.Finalize();
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&queue_lock_, SOURCE_TAG};

    // The use of the no-op assert which gives us access to the thread's
    // scheduler-variables requires a bit of explanation.  The thread is in the
    // process of being stolen.  Its scheduler state cpu_id still indicates that
    // it is a member of its previous scheduler, but it is no longer present in
    // that scheduler.  Because of this, reschedule operations on that scheduler
    // will not be able to consider the thread, and will not be able to access
    // the scheduler variables.
    //
    // A PI operation targeting the thread which runs before we acquire the
    // thread's lock exclusively (just above this one) will end up acquiring
    // the old scheduler's lock (because of the stale cpu_id), but it must have
    // happened-before this code (because we hold the thread's lock), and will
    // therefore also be able to see that the thread is in the process of being
    // stolen (because it synchronized with that state when it acquired the old
    // scheduler's queue lock, which was what was being held the last time the
    // steal_in_progress flag was mutated.  The PI operation will _never_ mutate
    // any of these flags.  This is a requirement, although there is no good way
    // to statically enforce it.
    //
    // Finally, we have just acquired our own queue_lock in order to place the
    // thread into our run queue.  We know that no one can have mutated the
    // flag, since no other reschedule operation can find the thread while it is
    // moving, and PI ops will never mutate the flag.  We were the ones to last
    // mutate the flag while holding the thread's old scheduler queue lock, so
    // our view of the flag should be coherent as well.
    //
    MarkHasOwnedThreadAccess(*thread);
    FinishTransition(now, thread);
  }
  return thread;
}

// Dequeues the eligible thread with the earliest virtual finish time. The
// caller must ensure that there is at least one thread in the queue.
Thread* Scheduler::DequeueFairThread() {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "dequeue_fair_thread");

  // Snap the virtual clock to the earliest start time.
  //
  // It should be safe to use the no-op assert here.  We know we are holding
  // this scheduler's queue lock (a requirement to call the function), and we
  // know that the thread must be a member of this scheduler (since we just
  // found it in the fair run queue).
  const auto& earliest_thread = fair_run_queue_.front();
  MarkHasOwnedThreadAccess(earliest_thread);
  const auto earliest_start = earliest_thread.scheduler_state().start_time_;
  const SchedTime eligible_time = ktl::max(virtual_time_, earliest_start);

  // Find the eligible thread with the earliest virtual finish time.
  // Note: Currently, fair tasks are always eligible when added to the run
  // queue, such that this search is equivalent to taking the front element of
  // a tree sorted by finish time, instead of start time. However, when moving
  // to the WF2Q algorithm, eligibility becomes a factor. Using the eligibility
  // query now prepares for migrating the algorithm and also avoids having two
  // different template instantiations of fbl::WAVLTree to support the fair and
  // deadline disciplines.
  Thread* const eligible_thread = FindEarliestEligibleThread(&fair_run_queue_, eligible_time);
  DEBUG_ASSERT_MSG(eligible_thread != nullptr,
                   "virtual_time=%" PRId64 ", eligible_time=%" PRId64 " , start_time=%" PRId64
                   ", finish_time=%" PRId64 ", min_finish_time=%" PRId64 "!",
                   virtual_time_.raw_value(), eligible_time.raw_value(),
                   earliest_thread.scheduler_state().start_time_.raw_value(),
                   earliest_thread.scheduler_state().finish_time_.raw_value(),
                   earliest_thread.scheduler_state().min_finish_time_.raw_value());

  // Same argument as before for the use of the no-op assert.  We are holding
  // the queue lock, and we just found this thread in the scheduler.
  MarkHasOwnedThreadAccess(*eligible_thread);
  virtual_time_ = eligible_time;
  fair_run_queue_.erase(*eligible_thread);
  TraceThreadQueueEvent("tqe_deque_fair"_intern, eligible_thread);
  return eligible_thread;
}

// Dequeues the eligible thread with the earliest deadline. The caller must
// ensure that there is at least one eligible thread in the queue.
Thread* Scheduler::DequeueDeadlineThread(SchedTime eligible_time) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "dequeue_deadline_thread");

  Thread* const eligible_thread = FindEarliestEligibleThread(&deadline_run_queue_, eligible_time);
  DEBUG_ASSERT_MSG(eligible_thread != nullptr, "eligible_time=%" PRId64, eligible_time.raw_value());

  MarkHasOwnedThreadAccess(*eligible_thread);  // See DequeueFairThread
  deadline_run_queue_.erase(*eligible_thread);
  TraceThreadQueueEvent("tqe_deque_deadline"_intern, eligible_thread);

  const SchedulerState& state = const_cast<const Thread*>(eligible_thread)->scheduler_state();
  if (state.finish_time_ <= eligible_time) {
    kcounter_add(counter_deadline_past, 1);
  }
  trace = KTRACE_END_SCOPE(("start time", Round<uint64_t>(state.start_time_)),
                           ("finish time", Round<uint64_t>(state.finish_time_)));
  return eligible_thread;
}

// Returns the eligible thread with the earliest deadline that is also earlier
// than the given deadline. Returns nullptr if no threads meet this criteria or
// the run queue is empty.
Thread* Scheduler::FindEarlierDeadlineThread(SchedTime eligible_time, SchedTime finish_time) {
  Thread* const eligible_thread = FindEarliestEligibleThread(&deadline_run_queue_, eligible_time);

  if (eligible_thread != nullptr) {
    MarkHasOwnedThreadAccess(*eligible_thread);  // See DequeueFairThread
    const bool found_earlier_deadline =
        const_cast<const Thread*>(eligible_thread)->scheduler_state().finish_time_ < finish_time;
    return found_earlier_deadline ? eligible_thread : nullptr;
  } else {
    return nullptr;
  }
}

// Returns the time that the next deadline task will become eligible or infinite
// if there are no ready deadline tasks or there is pending work for the idle
// thread to perform.
SchedTime Scheduler::GetNextEligibleTime() {
  if (deadline_run_queue_.is_empty()) {
    return SchedTime{ZX_TIME_INFINITE};
  }

  const Thread& front = deadline_run_queue_.front();
  MarkHasOwnedThreadAccess(front);
  return front.scheduler_state().start_time_;
}

// Dequeues the eligible thread with the earliest deadline that is also earlier
// than the given deadline. Returns nullptr if no threads meet the criteria or
// the run queue is empty.
Thread* Scheduler::DequeueEarlierDeadlineThread(SchedTime eligible_time, SchedTime finish_time) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "dequeue_earlier_deadline_thread");
  Thread* const eligible_thread = FindEarlierDeadlineThread(eligible_time, finish_time);

  if (eligible_thread != nullptr) {
    MarkHasOwnedThreadAccess(*eligible_thread);
    deadline_run_queue_.erase(*eligible_thread);
    TraceThreadQueueEvent("tqe_deque_earlier_deadline"_intern, eligible_thread);
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

// Selects a thread to run. Performs any necessary maintenance if the current
// thread is changing, depending on the reason for the change.
Thread* Scheduler::EvaluateNextThread(SchedTime now, Thread* current_thread, bool timeslice_expired,
                                      bool current_is_migrating, SchedDuration total_runtime_ns,
                                      Guard<MonitoredSpinLock, NoIrqSave>& queue_guard) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "find_thread");

  const bool is_idle = current_thread->IsIdle();
  const bool is_active = current_thread->state() == THREAD_READY;
  const bool is_deadline = IsDeadlineThread(current_thread);
  const bool is_new_deadline_eligible = IsDeadlineThreadEligible(now);

  Thread* next_thread = nullptr;
  if (!current_is_migrating) {
    if (is_active && likely(!is_idle)) {
      if (timeslice_expired) {
        // If the timeslice expired insert the current thread into the run queue.
        QueueThread(current_thread, Placement::Insertion, now, total_runtime_ns);
      } else if (is_new_deadline_eligible && is_deadline) {
        // The current thread is deadline scheduled and there is at least one
        // eligible deadline thread in the run queue: select the eligible thread
        // with the earliest deadline, which may still be the current thread.
        const SchedTime deadline_ns = current_thread->scheduler_state().finish_time_;
        if (Thread* const earlier_thread = DequeueEarlierDeadlineThread(now, deadline_ns);
            earlier_thread != nullptr) {
          QueueThread(current_thread, Placement::Preemption, now, total_runtime_ns);
          next_thread = earlier_thread;
        } else {
          // The current thread still has the earliest deadline.
          next_thread = current_thread;
        }
      } else if (is_new_deadline_eligible && !is_deadline) {
        // The current thread is fair scheduled and there is at least one eligible
        // deadline thread in the run queue: return this thread to the run queue.
        QueueThread(current_thread, Placement::Preemption, now, total_runtime_ns);
      } else {
        // The current thread has remaining time and no eligible contender.
        next_thread = current_thread;
      }
    } else if (!is_active && likely(!is_idle)) {
      // The current thread is no longer ready, remove its accounting.  Note,
      // active_thread_ can either equal current_thread at this point, or be
      // nullptr if the thread has already been removed from bookkeeping.  Remove
      // handles this edge case and does not remove the thread from bookkeeping if
      // it has already been removed.
      DEBUG_ASSERT((active_thread_ == current_thread) || (active_thread_ == nullptr));
      MarkHasOwnedThreadAccess(*current_thread);
      Remove(current_thread);
    }
  }

  // The current thread is no longer running, has returned to the run queue, or is migrating.
  // Select another thread to run.
  if (next_thread == nullptr) {
    next_thread = DequeueThread(now, queue_guard);
  }

  return next_thread;
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

    static SchedPerformanceScale LatchScaleUpFactor(const Scheduler* s) {
      return [s]()
                 TA_NO_THREAD_SAFETY_ANALYSIS { return s->performance_scale_reciprocal_.load(); }();
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
      const SchedUtilization utilization = ep.deadline.utilization;
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
    const SchedUtilization utilization = ep.deadline.utilization;
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

void Scheduler::UpdateTimeline(SchedTime now) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "update_vtime");

  const auto runtime_ns = now - last_update_time_ns_;
  last_update_time_ns_ = now;

  if (weight_total_ > SchedWeight{0}) {
    virtual_time_ += runtime_ns;
  }

  trace = KTRACE_END_SCOPE(
      ("runtime", Round<uint64_t>(runtime_ns)),
      ("virtual time",
       KTRACE_ANNOTATED_VALUE(AssertHeld(queue_lock_), Round<uint64_t>(virtual_time_))));
}

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
    // Acquire the thread's lock without backing off using an explicit retry loop because the
    // current thread's lock is held and ChainLock::Acquire asserts that no chain locks are
    // currently held to avoid misuse in the general case. We also currently hold the special
    // "sched token", meaning that we will win arbitration against any other chainlock
    // transaction.
    //
    // This is safe to do here because we can guarantee that no other scheduler holds or is
    // attempting to acquire the locks of any of the threads in this save_state_list_. The
    // only context in which schedulers attempt to acquire the locks of threads in other
    // schedulers is during the `StealWork` routine. However, that routine only looks for
    // threads in the RunQueues (not save_state_lists) and also does not steal threads that
    // have migration functions. Therefore, it is impossible for another scheduler to be
    // fighting over this thread's lock.
    while (!to_migrate->get_lock().TryAcquire(ChainLock::AllowFinalized::No,
                                              ChainLock::RecordBackoff::No)) {
      arch::Yield();
    }

    // We now have the thread's lock, so run the migrate function.
    to_migrate->CallMigrateFnLocked(Thread::MigrateStage::Save);

    // TODO(rudymathu): Threads placed in the save_state_list_ via an Unblock operation already
    // performed a `FindTargetCpu` call. In a future optimization, we should be able to reuse
    // that value here and avoid another CPU search, provided that the CPU is still online and
    // load balancing constraints still hold.
    const cpu_num_t target_cpu =
        FindActiveSchedulerForThread(to_migrate, [now](Thread* thread, Scheduler* target) {
          MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
          DEBUG_ASSERT(thread->scheduler_queue_state().transient_state ==
                       SchedulerQueueState::TransientState::Migrating);
          target->FinishTransition(now, thread);
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
  DEBUG_ASSERT_MSG(
      idle_processor_time_ns <= actual_runtime_ns,
      "idle_processor_time_ns=%" PRId64 " actual_runtime_ns=%" PRId64 " current_thread=%s",
      idle_processor_time_ns.raw_value(), actual_runtime_ns.raw_value(), current_thread->name());

  // Subtract any time the processor spent in the low-power idle state from the
  // runtime to ensure that active vs. idle power consumption is attributed
  // correctly. Processors can accumulate both active or idle power consumption,
  // but threads, including the idle power thread, accumulate only active power
  // consumption.
  const SchedDuration active_processor_time_ns = actual_runtime_ns - idle_processor_time_ns;

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
  using TransientState = SchedulerQueueState::TransientState;
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
  UpdateTimeline(now);

  // When calling into reschedule, the current thread is only allowed to be in a
  // limited number of states, depending on where it came from.
  //
  // RUNNING   : Thread is the currently active thread on this CPU.
  // BLOCKED   : Thread has been inserted into its blocked queue and is in the process of
  //             de-activating.
  // SUSPENDED : Thread is in the process of suspending (but is not in any queue).
  // DEAD      : Thread is in the process of dying, this is its final reschedule.
  //
  // In particular, however, the thread may not be in the READY state.  If the
  // current thread were in the ready state on the way into RescheduleCommon
  // (before its scheduler's queue lock was held), it would mean that the thread
  // could have been subject to theft by another scheduler.
  //
  // Now that we hold the scheduler's lock, we can go ahead and transition a
  // RUNNING thread's state to READY.  We also mark the thread as being in the
  // middle of a reschedule, which will prevent it from being stolen out from
  // under us until after the reschedule is complete.
  [&]() TA_REQ(current_thread->get_lock(), queue_lock()) {
    AssertInScheduler(*current_thread);
    DEBUG_ASSERT(current_thread->state() != THREAD_READY);
    DEBUG_ASSERT_MSG(
        current_thread->scheduler_queue_state().transient_state == TransientState::None,
        "Unexpected transient state at the start of RescheduleCommon (tid %lu, "
        "state %u, transient_state %u",
        current_thread->tid(), static_cast<uint32_t>(current_thread->state()),
        static_cast<uint32_t>(current_thread->scheduler_queue_state().transient_state));
    if (current_thread->state() == THREAD_RUNNING) {
      current_thread->set_ready();
      current_thread->scheduler_queue_state().transient_state = TransientState::Rescheduling;
    }
  }();

  // Assert that the current thread is a member of this scheduler, and that we
  // have access to the thread's scheduler owned variables.  Note, that as soon
  // as we call "EvaluateNextThread", the thread's curr_cpu_ state may end up
  // being reset to INVALID_CPU (if the thread is no longer ready), meaning that
  // this is the last time we own and have access to these variables.
  AssertInScheduler(*current_thread);

  const SchedDuration total_runtime_ns = now - start_of_current_time_slice_ns_;
  const SchedDuration actual_runtime_ns = now - current_state->last_started_running_;
  current_state->last_started_running_ = now;
  current_state->runtime_ns_ += actual_runtime_ns;
  current_thread->UpdateRuntimeStats(current_thread->state());

  // Update the energy consumption accumulators for the current task and
  // processor.
  UpdateEstimatedEnergyConsumption(current_thread, actual_runtime_ns);

  // Adjust the rate of the current thread when demand changes. Changes in
  // demand could be due to threads entering or leaving the run queue, or due
  // to weights changing in the current or enqueued threads.
  if (IsThreadAdjustable(current_thread) && weight_total_ != scheduled_weight_total_ &&
      total_runtime_ns < current_state->time_slice_ns_) {
    ktrace::Scope trace_adjust_rate = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "adjust_rate");
    EffectiveProfile& ep = current_state->effective_profile_;
    scheduled_weight_total_ = weight_total_;

    const SchedDuration time_slice_ns = CalculateTimeslice(current_thread);
    const SchedDuration remaining_time_slice_ns =
        time_slice_ns * ep.fair.normalized_timeslice_remainder;

    const bool timeslice_changed = time_slice_ns != ep.fair.initial_time_slice_ns;
    const bool timeslice_remaining = total_runtime_ns < remaining_time_slice_ns;

    // Update the preemption timer if necessary.
    if (timeslice_changed && timeslice_remaining) {
      target_preemption_time_ns_ = start_of_current_time_slice_ns_ + remaining_time_slice_ns;
      const SchedTime preemption_time_ns = ClampToDeadline(target_preemption_time_ns_);
      DEBUG_ASSERT(preemption_time_ns <= target_preemption_time_ns_);
      PreemptReset(current_cpu, now.raw_value(), preemption_time_ns.raw_value());
    }

    ep.fair.initial_time_slice_ns = time_slice_ns;
    current_state->time_slice_ns_ = remaining_time_slice_ns;
    trace_adjust_rate =
        KTRACE_END_SCOPE(("remaining time slice", Round<uint64_t>(remaining_time_slice_ns)),
                         ("total runtime", Round<uint64_t>(total_runtime_ns)));
  }

  // Update the time slice of a deadline task before evaluating the next task.
  if (IsDeadlineThread(current_thread)) {
    // Scale the actual runtime of the deadline task by the relative performance
    // of the CPU, effectively increasing the capacity of the task in proportion
    // to the performance ratio. The remaining time slice may become negative
    // due to scheduler overhead.
    current_state->time_slice_ns_ -= ScaleDown(actual_runtime_ns);
  }

  // Rounding in the scaling above may result in a small non-zero time slice
  // when the time slice should expire from the perspective of the target
  // preemption time. Use a small epsilon to avoid tripping the consistency
  // check below.
  const SchedDuration deadline_time_slice_epsilon{100};

  // Fair and deadline tasks have different time slice accounting strategies:
  // - A fair task expires when the total runtime meets or exceeds the time
  //   slice, which is updated only when the thread is adjusted or returns to
  //   the run queue.
  // - A deadline task expires when the remaining time slice is exhausted,
  //   updated incrementally on every reschedule, or when the absolute deadline
  //   is reached, which may occur with remaining time slice if the task wakes
  //   up late.
  const bool timeslice_expired =
      IsFairThread(current_thread)
          ? total_runtime_ns >= current_state->time_slice_ns_
          : now >= current_state->finish_time_ ||
                current_state->time_slice_ns_ <= deadline_time_slice_epsilon;

  // Check the consistency of the target preemption time and the current time
  // slice.
  [[maybe_unused]] const auto& ep = current_state->effective_profile_;
  DEBUG_ASSERT_MSG(
      now < target_preemption_time_ns_ || timeslice_expired,
      "capacity_ns=%" PRId64 " deadline_ns=%" PRId64 " now=%" PRId64
      " target_preemption_time_ns=%" PRId64 " total_runtime_ns=%" PRId64
      " actual_runtime_ns=%" PRId64 " finish_time=%" PRId64 " time_slice_ns=%" PRId64
      " start_of_current_time_slice_ns=%" PRId64,
      IsDeadlineThread(current_thread) ? ep.deadline.capacity_ns.raw_value() : 0,
      IsDeadlineThread(current_thread) ? ep.deadline.deadline_ns.raw_value() : 0, now.raw_value(),
      target_preemption_time_ns_.raw_value(), total_runtime_ns.raw_value(),
      actual_runtime_ns.raw_value(), current_state->finish_time_.raw_value(),
      current_state->time_slice_ns_.raw_value(), start_of_current_time_slice_ns_.raw_value());

  // If the current thread needs to be migrated, remove it for migration.
  bool current_is_migrating = false;
  if (NeedsMigration(current_thread)) {
    AssertInScheduler(*current_thread);
    // The current_thread's transient_state could either be Rescheduling or None, depending on
    // whether it was running or not. RemoveForTransition, however, expects the transient_state
    // to be None, so set it explicitly here.
    current_thread->scheduler_queue_state().transient_state = TransientState::None;
    RemoveForTransition(current_thread, TransientState::Migrating);
    current_is_migrating = true;
  }

  // Select the next thread to run.
  Thread* const next_thread = EvaluateNextThread(
      now, current_thread, timeslice_expired, current_is_migrating, total_runtime_ns, queue_guard);
  DEBUG_ASSERT(next_thread != nullptr);
  const bool thread_changed{current_thread != next_thread};

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
  if (thread_changed) {
    // Mark the next_thread as "becoming scheduled" while we drop the lock.
    // This will prevent another scheduler from stealing this thread out from
    // under as while we have the queue_lock dropped.  To make static analysis
    // happy, we have to prove that this thread is currently in this scheduler,
    // and that we hold the scheduler's queue_lock_.
    AssertInScheduler(*next_thread);
    SchedulerQueueState& sqs = next_thread->scheduler_queue_state();

    // The new thread we just selected should not currently be in a transient
    // state. It now needs to be in the Rescheduling state while we drop our
    // current scheduler's lock in order to obtain the thread's lock
    // exclusively.
    DEBUG_ASSERT(sqs.transient_state == TransientState::None);
    sqs.transient_state = TransientState::Rescheduling;

    // The ChainLockTransaction we were in when we entered RescheduleCommon has
    // already been finalized.  We need to restart it in order to obtain new
    // locks.
    active_clt.Restart(CLT_TAG("Scheduler::RescheduleCommon (restart)"));

    // Now drop the queue lock, obtain the next thread's lock, and re-acquire the queue lock.
    //
    // TODO(johngro): Need to talk to eieio@ to make absolutely sure that the
    // current scheduler's bookkeeping is coherent at this point in time.  IOW -
    // if (while we had the lock dropped) someone grabbed the lock to record the
    // consequences of a PI propagation, or stole a thread from our ready queue,
    // we need to make sure that _their_ updating of the scheduler's bookkeeping
    // will make sense and leave the bookkeeping in a valid state.
    //
    // Right now, I _think_ that this is the case.  So far, I don't think that
    // we have actually mutated the scheduler specific state in any way which
    // matters.  We have simple decided that we are going to switch to a
    // different thread, and picked that thread out.  We are now committed to
    // running that thread, regardless of any external changes to our
    // bookkeeping.  This should be fine, any externally made changes should
    // result in a reschedule being queued where (if our current choice is no
    // longer the correct thread) we will pick a new best thread.
    queue_guard.CallUnlocked([next_thread]() TA_ACQ(next_thread->get_lock()) {
      ChainLockTransaction::AssertActive();
      // Acquire the lock without backing off using an explicit retry loop because the current
      // thread's lock is held and ChainLock::Acquire asserts that no chain locks are currently
      // held to avoid misuse in the general case.
      while (!next_thread->get_lock().TryAcquire(ChainLock::AllowFinalized::No,
                                                 ChainLock::RecordBackoff::No)) {
        arch::Yield();
      }
    });

    // Now we have all of the locks we need for the reschedule operation.  Go
    // ahead and re-finalize the transaction.
    active_clt.Finalize();
  }

  // Now that we are sure that next_thread is properly locked, if we are
  // actually changing threads, trace the activation of the next thread before
  // context switching.  We can also clear the resched-in-progress flag on the next thread.
  next_thread->get_lock().AssertHeld();
  if (thread_changed) {
    AssertInScheduler(*next_thread);

    SchedulerQueueState& sqs = next_thread->scheduler_queue_state();
    DEBUG_ASSERT(sqs.transient_state == TransientState::Rescheduling);
    sqs.transient_state = TransientState::None;
  }

  // Update stall contributions of the current and next thread.
  if (thread_changed) {
    StallAccumulator::ApplyContextSwitch(current_thread, next_thread);
  }

  // Update the state of the current and next thread.
  const SchedulerQueueState& current_queue_state = current_thread->scheduler_queue_state();
  SchedulerState* const next_state = &next_thread->scheduler_state();
  next_thread->set_running();
  next_state->last_cpu_ = current_cpu;
  DEBUG_ASSERT(next_state->curr_cpu_ == current_cpu);
  active_thread_ = next_thread;

  // Handle any pending migration work.
  next_thread->CallMigrateFnLocked(Thread::MigrateStage::Restore);

  // Update the expected runtime of the current thread and the per-CPU total.
  // Only update the thread and aggregate values if the current thread is still
  // associated with this CPU or is no longer ready.
  const bool current_is_associated =
      !current_queue_state.active || current_state->curr_cpu_ == current_cpu;
  if (!current_thread->IsIdle() && current_is_associated &&
      (timeslice_expired || current_thread != next_thread)) {
    ktrace::Scope trace_update_ema = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "update_expected_runtime");

    // Adjust the runtime for the relative performance of the CPU to account for
    // different performance levels in the estimate. The relative performance
    // scale is in the range (0.0, 1.0], such that the adjusted runtime is
    // always less than or equal to the monotonic runtime.
    const SchedDuration adjusted_total_runtime_ns = ScaleDown(total_runtime_ns);
    current_state->banked_runtime_ns_ += adjusted_total_runtime_ns;

    if (timeslice_expired || !current_queue_state.active) {
      const SchedDuration delta_ns =
          PeakDecayDelta(current_state->expected_runtime_ns_, current_state->banked_runtime_ns_,
                         kExpectedRuntimeAlpha, kExpectedRuntimeBeta);
      current_state->expected_runtime_ns_ += delta_ns;
      current_state->banked_runtime_ns_ = SchedDuration{0};

      // Adjust the aggregate value by the same amount. The adjustment is only
      // necessary when the thread is still active on this CPU.
      if (current_queue_state.active) {
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
  } else if (timeslice_expired || thread_changed) {
    ktrace::Scope trace_start_preemption = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "next_slice");

    // Re-compute the time slice and deadline for the new thread based on the
    // latest state.
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
    trace_continue =
        KTRACE_END_SCOPE(("preemption_time", Round<uint64_t>(preemption_time_ns)),
                         ("target preemption time", Round<uint64_t>(target_preemption_time_ns_)));
  }

  // Assert that there is no path beside running the idle thread can leave the
  // preemption timer unarmed. However, the preemption timer may or may not be
  // armed when running the idle thread.
  // TODO(eieio): In the future, the preemption timer may be canceled when there
  // is only one task available to run. Revisit this assertion at that time.
  DEBUG_ASSERT(next_thread->IsIdle() || percpu::Get(current_cpu).timer_queue.PreemptArmed());

  // Almost done, we need to handle the actual context switch (if any).
  if (thread_changed) {
    LOCAL_KTRACE(
        DETAILED, "switch_threads",
        ("total threads", runnable_fair_task_count_ + runnable_deadline_task_count_),
        ("total weight", weight_total_.raw_value()),
        ("current thread time slice",
         Round<uint64_t>(current_thread->scheduler_state().time_slice_ns_)),
        ("next thread time slice", Round<uint64_t>(next_thread->scheduler_state().time_slice_ns_)));

    // Our current thread should have been flagged as either Rescheduling or
    // Migrating at this point.
    bool finish_migrate_current{false};
    if (current_thread->scheduler_queue_state().transient_state == TransientState::Rescheduling) {
      // If the current thread was simply Rescheduling, we simply need to clear
      // that flag.  Other CPUs are free to attempt to steal this thread now;
      // they will not succeed until after the actual context switch is done and
      // we drop the current thread's lock (even through we are just about to
      // drop the current scheduler's queue lock).
      current_thread->scheduler_queue_state().transient_state = TransientState::None;
    } else if (current_thread->scheduler_queue_state().transient_state ==
               TransientState::Migrating) {
      // Looks like the current thread needs to finish its migration.  Do so
      // after we have dropped this scheduler's queue lock.
      finish_migrate_current = true;
    } else {
      DEBUG_ASSERT(current_thread->scheduler_queue_state().transient_state == TransientState::None);
      DEBUG_ASSERT((current_thread->state() != THREAD_RUNNING) &&
                   (current_thread->state() != THREAD_READY));
    }

    TraceThreadQueueEvent("tqe_astart"_intern, next_thread);

    // Release queue lock before context switching.
    queue_guard.Release();

    // Now finish any pending migration of the current thread.  Again, this will
    // make it available to be selected to run on another CPU, or be stolen by
    // another CPU.  This is OK, as with clearing the rescheduling flag (above),
    // the other CPUs operation cannot complete until after the context switch
    // is finished and we have dropped the current thread's lock.
    if (finish_migrate_current) {
      // Call the Save stage of the thread's migration function as it leaves
      // this CPU.
      current_thread->CallMigrateFnLocked(Thread::MigrateStage::Save);

      const cpu_num_t target_cpu = FindActiveSchedulerForThread(
          current_thread, [now, this](Thread* thread, Scheduler* target) {
            MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
            DEBUG_ASSERT(target != this);
            target->FinishTransition(now, thread);
          });

      // Fire off an IPI to the CPU we just moved the thread to.
      mp_reschedule(cpu_num_to_mask(target_cpu), 0);
    }

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
      ktrace_probe(TraceAlways, TraceContext::Cpu, "tqe_afinish"_intern, arg0, arg1);
    }

    // Remember the pointer to the current thread's active ChainLockTransaction.
    // When the current thread is (eventually) scheduled again (on this CPU, or
    // a different CPU) will will eventually need to restore the thread's CLT
    // pointer (which exists somewhere up the thread's stack) to the per-cpu
    // data structure so it can properly unwind.

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
    // No context switch was needed.  Our current thread should have been
    // flagged as being in the process of rescheduling.  We can clear this flag
    // now.
    DEBUG_ASSERT(current_thread->scheduler_queue_state().transient_state ==
                 TransientState::Rescheduling);
    current_thread->scheduler_queue_state().transient_state = TransientState::None;

    // Restore our reschedule context and drop our queue guard before unwinding.
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

void Scheduler::UpdatePeriod() {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "update_period");

  DEBUG_ASSERT(runnable_fair_task_count_ >= 0);
  DEBUG_ASSERT(minimum_granularity_ns_ > 0);
  DEBUG_ASSERT(target_latency_grans_ > 0);

  const int64_t num_tasks = runnable_fair_task_count_;
  const int64_t normal_tasks = Round<int64_t>(target_latency_grans_);

  // The scheduling period stretches when there are too many tasks to fit
  // within the target latency.
  scheduling_period_grans_ = SchedDuration{num_tasks > normal_tasks ? num_tasks : normal_tasks};

  trace = KTRACE_END_SCOPE(("task count", num_tasks));
}

SchedDuration Scheduler::CalculateTimeslice(const Thread* thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "calculate_timeslice");
  const SchedulerState& state = thread->scheduler_state();
  const EffectiveProfile& ep = state.effective_profile_;

  // Calculate the relative portion of the scheduling period.
  const SchedWeight proportional_time_slice_grans =
      scheduling_period_grans_ * ep.fair.weight / weight_total_;

  // Ensure that the time slice is at least the minimum granularity.
  const int64_t time_slice_grans = Round<int64_t>(proportional_time_slice_grans);
  const int64_t minimum_time_slice_grans = time_slice_grans > 0 ? time_slice_grans : 1;

  // Calculate the time slice in nanoseconds.
  const SchedDuration time_slice_ns = minimum_time_slice_grans * minimum_granularity_ns_;

  trace = KTRACE_END_SCOPE(
      ("weight", ep.fair.weight.raw_value()),
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
  EffectiveProfile& ep = state->effective_profile_;
  SchedTime target_preemption_time_ns;

  if (IsFairThread(thread)) {
    // Calculate the next time slice and the deadline when the time slice is
    // completed.
    const SchedDuration time_slice_ns = CalculateTimeslice(thread);
    const SchedDuration remaining_time_slice_ns =
        time_slice_ns * ep.fair.normalized_timeslice_remainder;

    DEBUG_ASSERT(time_slice_ns > 0);
    DEBUG_ASSERT(remaining_time_slice_ns > 0);

    ep.fair.initial_time_slice_ns = time_slice_ns;
    state->time_slice_ns_ = remaining_time_slice_ns;
    target_preemption_time_ns = now + remaining_time_slice_ns;

    DEBUG_ASSERT_MSG(state->time_slice_ns_ > 0 && target_preemption_time_ns > now,
                     "time_slice_ns=%" PRId64 " now=%" PRId64 " target_preemption_time_ns=%" PRId64,
                     state->time_slice_ns_.raw_value(), now.raw_value(),
                     target_preemption_time_ns.raw_value());

    trace =
        KTRACE_END_SCOPE(("time slice", Round<uint64_t>(state->time_slice_ns_)),
                         ("target preemption time", Round<uint64_t>(target_preemption_time_ns)));
  } else {
    // Calculate the deadline when the remaining time slice is completed. The
    // time slice is maintained by the deadline queuing logic, no need to update
    // it here. The target preemption time is based on the time slice scaled by
    // the performance of the CPU and clamped to the deadline. This increases
    // capacity on slower processors, however, bandwidth isolation is preserved
    // because CPU selection attempts to keep scaled total capacity below one.
    const SchedDuration scaled_time_slice_ns = ScaleUp(state->time_slice_ns_);
    target_preemption_time_ns =
        ktl::min<SchedTime>(now + scaled_time_slice_ns, state->finish_time_);

    trace =
        KTRACE_END_SCOPE(("scaled time slice", Round<uint64_t>(scaled_time_slice_ns)),
                         ("target preemption time", Round<uint64_t>(target_preemption_time_ns)));
  }

  return target_preemption_time_ns;
}

void Scheduler::QueueThread(Thread* thread, Placement placement, SchedTime now,
                            SchedDuration total_runtime_ns) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "queue_thread");

  DEBUG_ASSERT(thread->state() == THREAD_READY);
  DEBUG_ASSERT(!thread->IsIdle());
  DEBUG_ASSERT(placement != Placement::Association);

  SchedulerState* const state = &thread->scheduler_state();
  EffectiveProfile& ep = state->effective_profile_;

  if (IsFairThread(thread)) {
    // Account for the consumed fair time slice. The consumed time is zero when
    // the thread is unblocking, migrating, or adjusting queue position. The
    // remaining time slice may become negative due to scheduler overhead.
    state->time_slice_ns_ -= total_runtime_ns;

    // Compute the ratio of remaining time slice to ideal time slice. This may
    // be less than 1.0 due to time slice consumed or due to previous preemption
    // by a deadline task or both.
    //
    // Note: it is important to ignore the initial_time_slice_ns and
    // normalized_timeslice_remainder members of the effective profile during an
    // insertion operation.  These member variables lost their meaning when the
    // thread blocked, if they were even defined at the time of blocking at all.
    const SchedRemainder normalized_timeslice_remainder =
        (placement == Placement::Insertion)
            ? SchedRemainder{0}
            : state->time_slice_ns_ / ktl::max(ep.fair.initial_time_slice_ns, SchedDuration{1});

    DEBUG_ASSERT_MSG(normalized_timeslice_remainder <= SchedRemainder{1},
                     "time_slice_ns=%" PRId64 " initial_time_slice_ns=%" PRId64
                     " remainder=%" PRId64 "\n",
                     state->time_slice_ns_.raw_value(), ep.fair.initial_time_slice_ns.raw_value(),
                     normalized_timeslice_remainder.raw_value());

    // If we are unblocking (placement is Insertion), or we have exhausted our
    // timeslice, then grant the thread a new timeslice by establishing a new
    // start time, and setting the normalized remainder to 1.
    //
    // Note that we use our recomputed normalized timeslice remainder to
    // determine whether or not our timeslice has expired (in the non
    // Placement::Insertion case).  This is important.  If we chose to use
    // time_slice_ns_ to make this decision, and ended up recomputing NTSR only
    // after we had determined that time_slice_ns_ was positive, it is possible
    // to end up with a positive time_slice_ns_, but a NTSR which is zero (which
    // will trigger asserts when the thread is next scheduled).
    if (normalized_timeslice_remainder <= 0) {
      state->start_time_ = ktl::max(state->finish_time_, virtual_time_);
      ep.fair.normalized_timeslice_remainder = SchedRemainder{1};
    } else if (placement == Placement::Preemption) {
      DEBUG_ASSERT(state->time_slice_ns_ > 0);
      ep.fair.normalized_timeslice_remainder = normalized_timeslice_remainder;
    }

    const SchedDuration scheduling_period_ns = scheduling_period_grans_ * minimum_granularity_ns_;
    const SchedWeight rate = kReciprocalMinWeight * ep.fair.weight;
    const SchedDuration delta_norm = scheduling_period_ns / rate;
    state->finish_time_ = state->start_time_ + delta_norm;

    DEBUG_ASSERT_MSG(state->start_time_ < state->finish_time_,
                     "start=%" PRId64 " finish=%" PRId64 " delta_norm=%" PRId64 "\n",
                     state->start_time_.raw_value(), state->finish_time_.raw_value(),
                     delta_norm.raw_value());
  } else {
    // Both a new insertion into the run queue or a re-insertion due to
    // preemption can happen after the time slice and/or deadline expires.
    if (placement == Placement::Insertion || placement == Placement::Preemption) {
      ktrace::Scope deadline_trace = LOCAL_KTRACE_BEGIN_SCOPE(
          DETAILED, "deadline_op",
          ("placement", placement == Placement::Insertion ? "insertion" : "preemption"));

      // Determine how much time is left before the deadline. This might be less
      // than the remaining time slice or negative if the thread blocked.
      const SchedDuration time_until_deadline_ns = state->finish_time_ - now;
      if (time_until_deadline_ns <= 0 || state->time_slice_ns_ <= 0) {
        const SchedTime period_finish_ns = state->start_time_ + ep.deadline.deadline_ns;

        state->start_time_ = now >= period_finish_ns ? now : period_finish_ns;
        state->finish_time_ = state->start_time_ + ep.deadline.deadline_ns;
        state->time_slice_ns_ = ep.deadline.capacity_ns;
      }
      deadline_trace =
          KTRACE_END_SCOPE(("time until deadline", Round<uint64_t>(time_until_deadline_ns)),
                           ("time slice", Round<uint64_t>(state->time_slice_ns_)));
    }

    DEBUG_ASSERT_MSG(state->start_time_ < state->finish_time_,
                     "start=%" PRId64 " finish=%" PRId64 " capacity=%" PRId64 "\n",
                     state->start_time_.raw_value(), state->finish_time_.raw_value(),
                     state->time_slice_ns_.raw_value());
  }

  // Only update the generation, enqueue time, and emit a flow event if this
  // is an insertion, preemption, or migration. In contrast, an adjustment only
  // changes the queue position in the same queue due to a parameter change and
  // should not perform these actions.
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

    // The generation count must always be updated when changing between CPUs,
    // as each CPU has its own generation count.
    state->generation_ = ++generation_count_;
  }

  // Insert the thread into the appropriate run queue after the generation count
  // is potentially updated above.
  if (IsFairThread(thread)) {
    fair_run_queue_.insert(thread);
  } else {
    deadline_run_queue_.insert(thread);
  }

  if (placement != Placement::Adjustment) {
    TraceThreadQueueEvent("tqe_enque"_intern, thread);
  }

  trace = KTRACE_END_SCOPE(("start time", Round<uint64_t>(state->start_time_)),
                           ("finish time", Round<uint64_t>(state->finish_time_)));
}

void Scheduler::Insert(SchedTime now, Thread* thread, Placement placement) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "insert");

  DEBUG_ASSERT(thread->state() == THREAD_READY);
  DEBUG_ASSERT(!thread->IsIdle());
  // We should never be inserting a thread into an inactive CPU's queues.
  DEBUG_ASSERT(IsLockedSchedulerActive());

  SchedulerQueueState& queue_state = thread->scheduler_queue_state();
  SchedulerState& state = thread->scheduler_state();
  const EffectiveProfile& ep = state.effective_profile_;

  // Ensure insertion happens only once, even if Unblock is called multiple times.
  if (queue_state.OnInsert()) {
    // Insertion can happen from a different CPU. Set the thread's current
    // CPU to the one this scheduler instance services.
    state.curr_cpu_ = this_cpu();

    UpdateTotalExpectedRuntime(state.expected_runtime_ns_);

    if (IsFairThread(thread)) {
      runnable_fair_task_count_++;
      DEBUG_ASSERT(runnable_fair_task_count_ > 0);

      UpdateTimeline(now);
      UpdatePeriod();

      weight_total_ += ep.fair.weight;
      DEBUG_ASSERT(weight_total_ > 0);
    } else {
      UpdateTotalDeadlineUtilization(ep.deadline.utilization);
      runnable_deadline_task_count_++;
      DEBUG_ASSERT(runnable_deadline_task_count_ != 0);
    }
    TraceTotalRunnableThreads();

    if (placement != Placement::Association) {
      QueueThread(thread, placement, now);
    } else {
      // Connect the flow into the previous queue to the new queue.
      LOCAL_KTRACE_FLOW_STEP(FLOW, "sched_latency", state.flow_id(), ("tid", thread->tid()));
    }
  }
}

void Scheduler::RemoveForTransition(Thread* thread,
                                    SchedulerQueueState::TransientState target_state) {
  using TransientState = SchedulerQueueState::TransientState;
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(DETAILED, "remove");

  // RemoveForTransition is used when channels are being actively migrated, and
  // when they are being stolen, but also as just common shared code for when
  // Scheduler::Remove is called.  In this case, the |target_state| will just be
  // None.
  DEBUG_ASSERT_MSG((target_state == TransientState::None) ||
                       (target_state == TransientState::Migrating) ||
                       (target_state == TransientState::Stolen),
                   "Bad target transient state! (%u)\n", static_cast<uint32_t>(target_state));
  DEBUG_ASSERT(!thread->IsIdle());

  SchedulerQueueState& queue_state = thread->scheduler_queue_state();
  const SchedulerState& state = const_cast<const Thread*>(thread)->scheduler_state();
  const EffectiveProfile& ep = state.effective_profile_;

  DEBUG_ASSERT_MSG(queue_state.transient_state == TransientState::None,
                   "Attempting to transition a thread which is already transitioning! (%u)\n",
                   static_cast<uint32_t>(queue_state.transient_state));
  DEBUG_ASSERT(!queue_state.InQueue());
  queue_state.transient_state = target_state;

  // Ensure that removal happens only once, even if Block() is called multiple times.
  if (queue_state.OnRemove()) {
    UpdateTotalExpectedRuntime(-state.expected_runtime_ns_);

    if (IsFairThread(thread)) {
      DEBUG_ASSERT(runnable_fair_task_count_ > 0);
      runnable_fair_task_count_--;

      UpdatePeriod();

      weight_total_ -= ep.fair.weight;
      DEBUG_ASSERT(weight_total_ >= 0);
    } else {
      UpdateTotalDeadlineUtilization(-ep.deadline.utilization);
      DEBUG_ASSERT(runnable_deadline_task_count_ > 0);
      runnable_deadline_task_count_--;
    }
    TraceTotalRunnableThreads();
  }
}

void Scheduler::FinishTransition(SchedTime now, Thread* thread) {
  // We had better either be in the process of being stolen or migrated.
  using TransientState = SchedulerQueueState::TransientState;
  SchedulerQueueState& sqs = thread->scheduler_queue_state();

  // Note, we don't have to reset curr_cpu_ in the thread's scheduler state.
  // Insert (called below) will do that for us.
  if (IsFairThread(thread)) {
    thread->scheduler_state().start_time_ = SchedNs(0);
    thread->scheduler_state().finish_time_ = SchedNs(0);
  }

  if (sqs.transient_state == TransientState::Migrating) {
    // Active migration uses a Placement of Insertion.
    Insert(now, thread, Placement::Insertion);

    // Our transition is complete, update our transient state.
    sqs.transient_state = TransientState::None;
  } else {
    // If the transition wasn't an active migration, it must be that the thread
    // was stolen.
    DEBUG_ASSERT_MSG(sqs.transient_state == TransientState::Stolen,
                     "Bad transient state when finishing transition! (%u)\n",
                     static_cast<uint32_t>(sqs.transient_state));

    // Add ourselves to our new scheduler's run queue.  Stolen threads use a
    // placement of Association.
    Insert(now, thread, Placement::Association);

    // Our transition is not quite done yet.  We were just stolen, which means
    // that we are in the process of becoming the active thread on this
    // scheduler (but have not done so yet).  Change our transient state to
    // "Rescheduling"
    sqs.transient_state = TransientState::Rescheduling;
  }
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
    // TODO(rudymathu): This target_cpu should be stashed in the thread prior to adding it to the
    // save_state_list_, as that would allow us to bypass CPU selection when processing the list so
    // long as the CPU is still online.
    target_cpu = FindTargetCpu(thread);
    const cpu_num_t last_cpu = thread->scheduler_state().last_cpu();
    const bool needs_migration = (last_cpu != INVALID_CPU && target_cpu != last_cpu &&
                                  thread->has_migrate_fn() && !thread->migrate_pending());
    if (needs_migration) {
      target_cpu = last_cpu;
    }
    Scheduler* const target = Get(target_cpu);
    Guard<MonitoredSpinLock, NoIrqSave> target_queue_guard{&target->queue_lock_, SOURCE_TAG};

    if (target->IsLockedSchedulerActive()) {
      TraceWakeup(thread, target_cpu);
      thread->set_ready();
      thread->UpdateRuntimeStats(thread->state());
      if (needs_migration) {
        MarkHasOwnedThreadAccess(*thread);
        thread->scheduler_queue_state().transient_state =
            SchedulerQueueState::TransientState::Migrating;
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
      // TODO(rudymathu): This target_cpu should be stashed in the thread prior to adding it to the
      // save_state_list_, as that would allow us to bypass CPU selection when processing the list
      // so long as the CPU is still online.
      target_cpu = FindTargetCpu(thread);
      const cpu_num_t last_cpu = thread->scheduler_state().last_cpu();
      const bool needs_migration = (last_cpu != INVALID_CPU && target_cpu != last_cpu &&
                                    thread->has_migrate_fn() && !thread->migrate_pending());
      if (needs_migration) {
        target_cpu = last_cpu;
      }
      Scheduler* const target = Get(target_cpu);
      Guard<MonitoredSpinLock, NoIrqSave> target_queue_guard{&target->queue_lock_, SOURCE_TAG};

      if (target->IsLockedSchedulerActive()) {
        TraceWakeup(thread, target_cpu);
        thread->set_ready();
        thread->UpdateRuntimeStats(thread->state());
        if (needs_migration) {
          MarkHasOwnedThreadAccess(*thread);
          thread->scheduler_queue_state().transient_state =
              SchedulerQueueState::TransientState::Migrating;
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

  {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&Get()->queue_lock_, SOURCE_TAG};
    thread->set_ready();
    state->curr_cpu_ = lowest_cpu_set(state->hard_affinity_);
  }
}

void Scheduler::Yield(Thread* const current_thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_yield");
  DEBUG_ASSERT(current_thread == Thread::Current::Get());
  DEBUG_ASSERT(!current_thread->IsIdle());

  if (IsFairThread(current_thread)) {
    Scheduler& current_scheduler = *Get();
    const SchedTime now = CurrentTime();

    {
      // TODO(eieio,johngro): What is this protecting?
      Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&current_scheduler.queue_lock_, SOURCE_TAG};
      SchedulerState& current_state = current_thread->scheduler_state();
      EffectiveProfile& ep = current_thread->scheduler_state().effective_profile_;

      // Update the virtual timeline in preparation for snapping the thread's
      // virtual finish time to the current virtual time.
      current_scheduler.UpdateTimeline(now);

      // Set the time slice to expire now.
      current_state.time_slice_ns_ = SchedDuration{0};

      // The thread is re-evaluated with zero lag against other competing threads
      // and may skip lower priority threads with similar arrival times.
      current_state.finish_time_ = current_scheduler.virtual_time_;
      ep.fair.initial_time_slice_ns = current_state.time_slice_ns_;
      ep.fair.normalized_timeslice_remainder = SchedRemainder{1};
    }

    current_scheduler.RescheduleCommon(current_thread, now, trace.Completer());
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

void Scheduler::Migrate(Thread* thread) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_migrate");

  SchedulerState* const state = &thread->scheduler_state();
  const cpu_mask_t effective_cpu_mask = state->GetEffectiveCpuMask(PeekActiveMask());
  const cpu_mask_t thread_cpu_mask = cpu_num_to_mask(state->curr_cpu_);
  const bool stale_curr_cpu = (thread_cpu_mask & effective_cpu_mask) == 0;
  cpu_mask_t cpus_to_reschedule_mask = 0;

  if (stale_curr_cpu) {
    if (thread->state() == THREAD_RUNNING) {
      // The CPU the thread is running on will take care of the actual migration.
      cpus_to_reschedule_mask |= thread_cpu_mask;
    } else if (thread->state() == THREAD_READY) {
      bool save_state_on_current_cpu{false};

      do {  // Explicit scope top hold the lock for this thread's current scheduler.
        Scheduler& thread_sched = *Get(state->curr_cpu_);
        Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&thread_sched.queue_lock_, SOURCE_TAG};
        thread_sched.AssertInScheduler(*thread);
        SchedulerQueueState& sqs = thread->scheduler_queue_state();
        const cpu_num_t current_cpu = arch_curr_cpu_num();

        // If the thread is currently in a transient state (rescheduling, being
        // stolen, or migrating), don't do anything else.  When the thread gets
        // to wherever it is finally going, if that location turns out to be
        // incompatible with its affinity masks, a new CPU should end up being
        // selected instead of wherever it was going.
        if (sqs.transient_state != SchedulerQueueState::TransientState::None) {
          break;
        }

        // Initiate the migration for this thread. If this thread is currently running on
        // this CPU, we can run its migrate function inline. Otherwise, we need to enqueue
        // it into the target scheduler's save_state_list_ and IPI that core.
        thread_sched.EraseFromQueue(thread);
        thread_sched.RemoveForTransition(thread, SchedulerQueueState::TransientState::Migrating);
        if (thread->has_migrate_fn() && !thread->migrate_pending() &&
            (thread->scheduler_state().last_cpu_ != INVALID_CPU) &&
            (thread->scheduler_state().last_cpu_ != current_cpu)) {
          thread_sched.save_state_list_.push_front(thread);
          cpus_to_reschedule_mask |= thread_cpu_mask;
        } else {
          save_state_on_current_cpu = true;
        }
      } while (false);

      if (save_state_on_current_cpu) {
        // Before obtaining any new locks, try to call the thread's migration
        // function.  CallMigrateFnLocked will do nothing if we have no
        // migration function, or if we have already started the migration (eg;
        // called the Save stage).
        thread->CallMigrateFnLocked(Thread::MigrateStage::Save);

        // Find a new CPU for this thread and add it to the queue.
        const cpu_num_t target_cpu =
            FindActiveSchedulerForThread(thread, [](Thread* thread, Scheduler* target) {
              MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
              target->FinishTransition(CurrentTime(), thread);
            });

        // Reschedule both CPUs to handle the run queue changes.
        cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu) | thread_cpu_mask;
      }
    }
  }

  // The operation is finished.  Drop the thread's lock before we call into
  // reschedule so that we are certain to not be holding any threads' locks
  // during the RescheduleMask operation (which might result in an immediate
  // preempt).
  trace.End();
  thread->get_lock().Release();
  RescheduleMask(cpus_to_reschedule_mask);
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
  Thread::UnblockList migrating_threads;
  Thread::SaveStateList local_save_state_list;
  {
    Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&current->queue_lock_, SOURCE_TAG};

    if (!current->save_state_list_.is_empty()) {
      local_save_state_list = ktl::move(current->save_state_list_);
    }

    auto MoveThreads = [&migrating_threads, current, current_cpu_mask](
                           RunQueue& run_queue,
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
          current->RemoveForTransition(&consider, SchedulerQueueState::TransientState::Migrating);
          migrating_threads.push_back(&consider);
        }
      }
    };

    MoveThreads(current->fair_run_queue_, "tqe_deque_migrate_unpinned_fair"_intern);
    MoveThreads(current->deadline_run_queue_, "tqe_deque_migrate_unpinned_deadline"_intern);
  }

  // OK, now that we have dropped our queue lock, go over our list of migrating
  // threads and finish the migration process.  Don't make any assumptions about
  // the fair/deadline nature of the threads as we migrate.  Since we dropped
  // the scheduler's queue lock, it is possible that these threads have changed
  // effective priority since being remove from their old scheduler, but before
  // arriving at a new one.
  auto MigrateThread = [current, now](Thread* thread) -> cpu_num_t {
    SingleChainLockGuard guard{IrqSaveOption, thread->get_lock(),
                               CLT_TAG("Scheduler::MigrateUnpinnedThreads")};

    // Call the Save stage of the thread's migration function as it leaves
    // this CPU.
    thread->CallMigrateFnLocked(Thread::MigrateStage::Save);

    const cpu_num_t target_cpu =
        FindActiveSchedulerForThread(thread, [current, now](Thread* thread, Scheduler* target) {
          // Finish the transition and add the thread to the new target queue.  The
          // Restore stage of the migration function will be called on the new CPU the
          // next time the thread becomes scheduled.
          MarkInFindActiveSchedulerForThreadCbk(*thread, *target);
          (void)current;  // We cannot annotate 'current' with [[maybe_unused]] in the capture list,
                          // so suppress the warning using the old school void-cast trick.
          DEBUG_ASSERT(target != current);
          target->FinishTransition(now, thread);
        });

    return target_cpu;
  };

  while (!migrating_threads.is_empty()) {
    Thread* thread = migrating_threads.pop_front();
    const cpu_num_t target_cpu = MigrateThread(thread);
    cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu);
  }

  while (!local_save_state_list.is_empty()) {
    Thread* thread = local_save_state_list.pop_front();
    const cpu_num_t target_cpu = MigrateThread(thread);
    cpus_to_reschedule_mask |= cpu_num_to_mask(target_cpu);
  }

  trace.End();
  RescheduleMask(cpus_to_reschedule_mask);
}

void Scheduler::TimerTick(SchedTime now) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_timer_tick");
  Thread::Current::preemption_state().PreemptSetPending();
}

void Scheduler::InitializePerformanceScale(SchedPerformanceScale scale) {
  // This happens early in boot, before the scheduler is actually running.  We
  // should be able to treat these assignments as if they were done in a
  // constructor and skip obtaining the queue lock.
  DEBUG_ASSERT(scale > 0);
  [&]() TA_NO_THREAD_SAFETY_ANALYSIS {
    performance_scale_ = scale;
    default_performance_scale_ = scale;
    pending_user_performance_scale_ = scale;
    performance_scale_reciprocal_ = 1 / scale;
  }();
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

template <Affinity AffinityType>
cpu_mask_t Scheduler::SetCpuAffinity(Thread& thread, cpu_mask_t affinity) {
  if constexpr (AffinityType == Affinity::Hard) {
    DEBUG_ASSERT_MSG(
        const cpu_mask_t active_mask = PeekActiveMask();
        (affinity & active_mask) != 0,
        "Attempted to set affinity mask to %#x, which has no overlap of active CPUs %#x.", affinity,
        active_mask);
  }

  auto AssignAffinity = [](Thread& thread, cpu_mask_t affinity)
                            TA_NO_THREAD_SAFETY_ANALYSIS -> cpu_mask_t {
    cpu_mask_t previous_affinity;

    if constexpr (AffinityType == Affinity::Hard) {
      previous_affinity = thread.scheduler_state().hard_affinity_;
      thread.scheduler_state().hard_affinity_ = affinity;
    } else {
      static_assert(AffinityType == Affinity::Soft);
      previous_affinity = thread.scheduler_state().soft_affinity_;
      thread.scheduler_state().soft_affinity_ = affinity;
    }

    return previous_affinity;
  };

  // Acquire the thread's lock unconditionally, and without using a guard.  The
  // call to Scheduler::Migrate will drop the lock for us before potentially
  // rescheduling.
  cpu_mask_t previous_affinity;
  const auto do_transaction =
      [&]() TA_REQ(chainlock_transaction_token) -> ChainLockTransaction::Result<> {
    thread.get_lock().AcquireFirstInChain();

    // set the affinity mask.  Note: to mutate the scheduler state (instead of
    // just reading it), we need to hold both the thread's lock, as well as the
    // lock of any container the thread happens to be in at the time (if any).
    //
    // TODO(johngro): It is really easy to mess this requirement up.  Is there a
    // better way to statically assert these requirements?
    if ((thread.state() == THREAD_RUNNING) || (thread.state() == THREAD_READY)) {
      // We are assigned to a scheduler, we need to hold its lock while mutate
      // our affinity.  That said, we have all of the chain locks we need, so go
      // ahead and finalize our transaction now.
      ChainLockTransaction::Finalize();
      SchedulerState& ss = thread.scheduler_state();
      DEBUG_ASSERT(ss.curr_cpu() != INVALID_CPU);
      Scheduler* scheduler = Scheduler::Get(ss.curr_cpu());
      Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler->queue_lock(), SOURCE_TAG};
      previous_affinity = AssignAffinity(thread, affinity);
    } else if (thread.state() == THREAD_BLOCKED) {
      // We are in a wait queue, we need to obtain the wait queue's lock, and be
      // prepared to back off if needed.
      WaitQueue* wq = thread.wait_queue_state().blocking_wait_queue_;
      DEBUG_ASSERT(wq != nullptr);

      if (!wq->get_lock().AcquireOrBackoff()) {
        thread.get_lock().Release();
        return ChainLockTransaction::Action::Backoff;
      }

      ChainLockTransaction::Finalize();
      previous_affinity = AssignAffinity(thread, affinity);
      wq->get_lock().Release();
    } else {
      // No container, we have all the locks we need.
      ChainLockTransaction::Finalize();
      previous_affinity = AssignAffinity(thread, affinity);
    }

    // Is our new affinity mask incompatible with our current CPU?  If so, let
    // let Scheduler::Migrate deal with the rest of the migration, it will drop
    // our lock for us.  Otherwise, just drop the thread's lock and get out.
    if ((affinity & cpu_num_to_mask(arch_curr_cpu_num())) == 0) {
      Scheduler::Migrate(&thread);
    } else {
      thread.get_lock().Release();
    }

    return ChainLockTransaction::Done;
  };

  ChainLockTransaction::UntilDone(IrqSaveOption, CLT_TAG("Scheduler::SetCpuAffinity"),
                                  do_transaction);
  return previous_affinity;
}

template cpu_mask_t Scheduler::SetCpuAffinity<Affinity::Hard>(Thread& thread, cpu_mask_t affinity);
template cpu_mask_t Scheduler::SetCpuAffinity<Affinity::Soft>(Thread& thread, cpu_mask_t affinity);

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

void Scheduler::PowerLevelControl::TimerHandler(Timer* timer, zx_time_t now, void* arg) {
  // Only queue the DPC if the timer handler is running on the expected CPU. If the timer handler
  // is running on a different CPU, the CPU it services went offline while the timer was pending
  // and its power level requests are no longer relevant.
  PowerLevelControl* power_level_control = static_cast<PowerLevelControl*>(arg);
  if (power_level_control->cpu() == arch_curr_cpu_num()) {
    power_level_control->request_dpc_.Queue();
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
