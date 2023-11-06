// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <lib/ktrace.h>
#include <zircon/errors.h>

#include <arch/interrupt.h>
#include <arch/ops.h>
#include <kernel/mp.h>
#include <kernel/percpu.h>
#include <kernel/thread.h>
#include <ktl/atomic.h>

namespace {

constexpr bool kEnableRunloopTracing = false;

constexpr const fxt::InternedString& ToString(IdlePowerThread::State state) {
  using fxt::operator""_intern;
  switch (state) {
    case IdlePowerThread::State::Offline:
      return "offline"_intern;
    case IdlePowerThread::State::Suspend:
      return "suspend"_intern;
    case IdlePowerThread::State::Online:
      return "online"_intern;
    default:
      return "unknown"_intern;
  }
}

}  // anonymous namespace

void IdlePowerThread::FlushAndHalt() {
  DEBUG_ASSERT(arch_ints_disabled());

  const StateMachine state = state_.load(ktl::memory_order_acquire);
  DEBUG_ASSERT(state.target == State::Offline);
  state_.store({State::Offline, State::Offline}, ktl::memory_order_release);

  arch_flush_state_and_halt(&complete_);
}

int IdlePowerThread::Run(void* arg) {
  const cpu_num_t cpu_num = arch_curr_cpu_num();
  percpu& this_cpu = percpu::GetCurrent();
  for (;;) {
    const StateMachine state = this_cpu.idle_power_thread.state_.load(ktl::memory_order_acquire);
    if (state.target != state.current) {
      ktrace::Scope trace = KTRACE_CPU_BEGIN_SCOPE_ENABLE(
          kEnableRunloopTracing, "kernel:sched", "transition", ("from", ToString(state.current)),
          ("to", ToString(state.target)));
      dprintf(INFO, "CPU %u: %s -> %s\n", cpu_num, ToString(state.current).string,
              ToString(state.target).string);

      switch (state.target) {
        case State::Offline: {
          InterruptDisableGuard interrupt_disable;
          Guard<MonitoredSpinLock, NoIrqSave> guard{ThreadLock::Get(), SOURCE_TAG};

          // Emit the complete event early, since mp_unplug_current_cpu() will not return.
          trace.End();

          // Updating the state and signaling the complete event is handled by
          // mp_unplug_current_cpu() when it calls FlushAndHalt().
          mp_unplug_current_cpu(ktl::move(guard));
          break;
        }

        default:
          this_cpu.idle_power_thread.state_.store({.current = state.target, .target = state.target},
                                                  ktl::memory_order_release);
          this_cpu.idle_power_thread.complete_.Signal();
          break;
      }

      // If the power thread just transitioned to online, reschedule to ensure that the run queue is
      // evaluated. Otherwise, this thread may continue to run the idle loop until the next IPI.
      if (state.target == State::Online) {
        Thread::Current::Reschedule();
      }
    } else {
      ktrace::Scope trace =
          KTRACE_CPU_BEGIN_SCOPE_ENABLE(kEnableRunloopTracing, "kernel:sched", "idle");
      //  TODO(eieio): Use scheduler and timer states to determine latency requirements.
      const zx_duration_t max_latency = 0;
      arch_idle_enter(max_latency);
    }
  }
}

IdlePowerThread::TransitionResult IdlePowerThread::TransitionFromTo(State expected_state,
                                                                    State target_state,
                                                                    zx_time_t timeout_at) {
  // Attempt to move from the expected state to the transitional state.
  StateMachine expected{.current = expected_state, .target = expected_state};
  const StateMachine transitional{.current = expected_state, .target = target_state};
  if (!state_.compare_exchange_strong(expected, transitional, ktl::memory_order_acq_rel,
                                      ktl::memory_order_relaxed)) {
    // The move to the transitional state failed because the current state is already the target
    // state. Indicate that the transition did not occur but the target state is achieved anyway by
    // returning the original state.
    if (expected.current == target_state) {
      return {ZX_OK, target_state};
    }

    // The move to the transitional state failed because the current state is neither the expected
    // state nor the target state.
    return {ZX_ERR_BAD_STATE, expected.current};
  }
  DEBUG_ASSERT(expected.current == expected_state);

  {
    // Reschedule the CPU for the idle/power thread to expedite handling the transition. Disable
    // interrupts to ensure this thread doesn't migrate to another CPU after sampling the current
    // CPU.
    InterruptDisableGuard interrupt_disable;
    if (this_cpu_ == arch_curr_cpu_num()) {
      Thread::Current::Reschedule();
    } else {
      mp_reschedule(cpu_num_to_mask(this_cpu_), 0);
    }
  }

  // Wait for the transition to the target state to complete or timeout, looping to deal with
  // spurious wakeups.
  do {
    const zx_status_t status = complete_.WaitDeadline(timeout_at, Interruptible::No);
    if (status != ZX_OK) {
      return {status, expected_state};
    }
    expected = state_.load(ktl::memory_order_acquire);
    DEBUG_ASSERT(expected.target == target_state);
  } while (expected.current != target_state);

  // The transition from the expected state to the target state was successful.
  return {ZX_OK, expected_state};
}
