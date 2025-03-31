// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_INTERNAL_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_INTERNAL_H_

#include <lib/kconcurrent/chainlock_transaction.h>
#include <lib/ktrace.h>

#include <arch/mp.h>
#include <ffl/fixed_format.h>
#include <kernel/scheduler.h>
#include <ktl/algorithm.h>

// Determines which subset of tracers are enabled when detailed tracing is
// enabled. When queue tracing is enabled the minimum trace level is COMMON.
#define LOCAL_KTRACE_LEVEL                                         \
  (SCHEDULER_TRACING_LEVEL == 0 && SCHEDULER_QUEUE_TRACING_ENABLED \
       ? KERNEL_SCHEDULER_TRACING_LEVEL_COMMON                     \
       : SCHEDULER_TRACING_LEVEL)

// The tracing levels used in this compilation unit.
#define KERNEL_SCHEDULER_TRACING_LEVEL_COMMON 1
#define KERNEL_SCHEDULER_TRACING_LEVEL_FLOW 2
#define KERNEL_SCHEDULER_TRACING_LEVEL_COUNTER 3
#define KERNEL_SCHEDULER_TRACING_LEVEL_DETAILED 4

// Evaluates to true if tracing is enabled for the given level.
#define LOCAL_KTRACE_LEVEL_ENABLED(level) \
  ((LOCAL_KTRACE_LEVEL) >= FXT_CONCATENATE(KERNEL_SCHEDULER_TRACING_LEVEL_, level))

#define LOCAL_KTRACE(level, string, args...) \
  KTRACE_CPU_INSTANT_ENABLE(LOCAL_KTRACE_LEVEL_ENABLED(level), "kernel:probe", string, ##args)

#define LOCAL_KTRACE_FLOW_BEGIN(level, string, flow_id, args...)                                   \
  KTRACE_CPU_FLOW_BEGIN_ENABLE(LOCAL_KTRACE_LEVEL_ENABLED(level), "kernel:sched", string, flow_id, \
                               ##args)

#define LOCAL_KTRACE_FLOW_END(level, string, flow_id, args...)                                   \
  KTRACE_CPU_FLOW_END_ENABLE(LOCAL_KTRACE_LEVEL_ENABLED(level), "kernel:sched", string, flow_id, \
                             ##args)

#define LOCAL_KTRACE_FLOW_STEP(level, string, flow_id, args...)                                   \
  KTRACE_CPU_FLOW_STEP_ENABLE(LOCAL_KTRACE_LEVEL_ENABLED(level), "kernel:sched", string, flow_id, \
                              ##args)

#define LOCAL_KTRACE_COUNTER(level, string, counter_id, args...)                                   \
  KTRACE_CPU_COUNTER_ENABLE(LOCAL_KTRACE_LEVEL_ENABLED(level), "kernel:sched", string, counter_id, \
                            ##args)

#define LOCAL_KTRACE_BEGIN_SCOPE(level, string, args...) \
  KTRACE_CPU_BEGIN_SCOPE_ENABLE(LOCAL_KTRACE_LEVEL_ENABLED(level), "kernel:sched", string, ##args)

// Scales the given value up by the reciprocal of the CPU performance scale.
template <typename T>
inline T Scheduler::ScaleUp(T value) const {
  return value * performance_scale_reciprocal();
}

// Scales the given value down by the CPU performance scale.
template <typename T>
inline T Scheduler::ScaleDown(T value) const {
  return value * performance_scale();
}

// Returns a new flow id when flow tracing is enabled, zero otherwise.
inline uint64_t Scheduler::NextFlowId() {
  if constexpr (LOCAL_KTRACE_LEVEL_ENABLED(FLOW)) {
    return next_flow_id_.fetch_add(1);
  }
  return 0;
}

// Updates the total expected runtime estimator with the given delta. The
// exported value is scaled by the relative performance factor of the CPU to
// account for performance differences in the estimate.
inline void Scheduler::UpdateTotalExpectedRuntime(SchedDuration delta_ns) {
  total_expected_runtime_ns_ += delta_ns;
  DEBUG_ASSERT(total_expected_runtime_ns_ >= 0);
  const SchedDuration scaled_ns = ScaleUp(total_expected_runtime_ns_);
  exported_total_expected_runtime_ns_ = scaled_ns;
  LOCAL_KTRACE_COUNTER(COUNTER, "Stats", this_cpu(), ("Demand", scaled_ns.raw_value()));
}

// Updates the total deadline utilization estimator with the given delta. The
// exported value is scaled by the relative performance factor of the CPU to
// account for performance differences in the estimate.
inline void Scheduler::UpdateTotalDeadlineUtilization(SchedUtilization delta) {
  total_deadline_utilization_ += delta;
  DEBUG_ASSERT(total_deadline_utilization_ >= 0);
  const SchedUtilization scaled = ScaleUp(total_deadline_utilization_);
  exported_total_deadline_utilization_ = scaled;
  LOCAL_KTRACE_COUNTER(COUNTER, "Stats", this_cpu(),
                       ("Utilization", ffl::Round<uint64_t>(scaled * 1000)));
}

inline void Scheduler::TraceTotalRunnableThreads() const {
  LOCAL_KTRACE_COUNTER(COUNTER, "Stats", this_cpu(),
                       ("Queue Length", runnable_fair_task_count_ + runnable_deadline_task_count_));
}

inline void Scheduler::RescheduleMask(cpu_mask_t cpus_to_reschedule_mask) {
  // Does the local CPU need to be preempted?
  const cpu_mask_t local_mask = cpu_num_to_mask(arch_curr_cpu_num());
  const cpu_mask_t local_cpu = cpus_to_reschedule_mask & local_mask;

  PreemptionState& preemption_state = Thread::Current::Get()->preemption_state();

  // First deal with the remote CPUs.
  if (preemption_state.EagerReschedDisableCount() == 0) {
    // |mp_reschedule| will remove the local cpu from the mask for us.
    mp_reschedule(cpus_to_reschedule_mask, 0);
  } else {
    // EagerReschedDisabled implies that local preemption is also disabled.
    DEBUG_ASSERT(!preemption_state.PreemptIsEnabled());
    preemption_state.preempts_pending_add(cpus_to_reschedule_mask);
    if (local_cpu != 0) {
      preemption_state.EvaluateTimesliceExtension();
    }
    return;
  }

  if (local_cpu != 0) {
    const bool preempt_enabled = preemption_state.EvaluateTimesliceExtension();

    // Can we do it here and now?
    if (preempt_enabled) {
      // TODO(https://fxbug.dev/42143537): Once spinlocks/chainlocks imply preempt disable,
      // this if-else can be replaced with a call to Preempt().

      if ((arch_num_spinlocks_held() == 0) && !arch_blocking_disallowed()) {
        // From a chain-lock requirement perspective, there are a few different
        // cases to consider.
        //
        // 1) We currently have no transaction in progress and hold no locks.
        //    In this case, we can simply call Preempt, which will start a
        //    transaction, lock our current thread, and take care of preemption.
        // 2) We do currently have a transaction in progress, but we hold no
        //    locks.  This can happen in the cases such as Unblock(thread) or
        //    Unblock(list), where all of the unblocking thread's locks were
        //    dropped after adding them to the proper scheduler queue.  We need
        //    to re-use our existing transaction by first restarting it, then
        //    obtaining our current thread's lock, and finally calling
        //    PreemptLocked.
        // 3) We do currently have a transaction in progress, and we are holding
        //    exactly one lock which is the current thread's lock.  In this case,
        //    we call simply PreemptLocked directly.
        //
        // TODO(johngro): Determine if #1 and #3 are actual possibilities by the
        // time that we hit this stage.  #2 may be the only legit case, but I'm
        // not quite sure yet.
        if (ChainLockTransaction* active_clt = ChainLockTransaction::Active();
            active_clt != nullptr) {
          ChainLockTransaction::MarkActive();
          Thread* const current_thread = Thread::Current::Get();

          if (current_thread->get_lock().is_held() == false) {
            active_clt->Restart(CLT_TAG("Scheduler::RescheduleMask"));
            active_clt->AssertNumLocksHeld(0);
            ChainLockGuard guard{current_thread->get_lock()};
            active_clt->Finalize();
            PreemptLocked(current_thread);
          } else {
            active_clt->AssertNumLocksHeld(1);
            current_thread->get_lock().AssertHeld();
            PreemptLocked(current_thread);
          }
        } else {
          Preempt();
        }
        return;
      }
    }

    // Nope, can't do it now.  Make a note for later.
    preemption_state.preempts_pending_add(local_cpu);
  }
}

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_INTERNAL_H_
