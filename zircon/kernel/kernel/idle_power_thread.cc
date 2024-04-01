// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "kernel/idle_power_thread.h"

#include <assert.h>
#include <lib/fit/defer.h>
#include <lib/ktrace.h>
#include <platform.h>
#include <zircon/errors.h>
#include <zircon/time.h>

#include <arch/interrupt.h>
#include <arch/mp.h>
#include <arch/ops.h>
#include <kernel/cpu.h>
#include <kernel/mp.h>
#include <kernel/percpu.h>
#include <kernel/thread.h>
#include <ktl/atomic.h>

namespace {

constexpr bool kEnableRunloopTracing = false;

constexpr const fxt::InternedString& ToInternedString(IdlePowerThread::State state) {
  using fxt::operator""_intern;
  switch (state) {
    case IdlePowerThread::State::Offline:
      return "offline"_intern;
    case IdlePowerThread::State::Suspend:
      return "suspend"_intern;
    case IdlePowerThread::State::Active:
      return "active"_intern;
    case IdlePowerThread::State::Wakeup:
      return "wakeup"_intern;
    default:
      return "unknown"_intern;
  }
}

[[maybe_unused]] constexpr const char* ToString(IdlePowerThread::State state) {
  return ToInternedString(state).string;
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
  IdlePowerThread& this_idle_power_thread = percpu::GetCurrent().idle_power_thread;
  for (;;) {
    const StateMachine state = this_idle_power_thread.state_.load(ktl::memory_order_acquire);
    if (state.target != state.current) {
      ktrace::Scope trace = KTRACE_CPU_BEGIN_SCOPE_ENABLE(
          kEnableRunloopTracing, "kernel:sched", "transition",
          ("from", ToInternedString(state.current)), ("to", ToInternedString(state.target)));
      dprintf(INFO, "CPU %u: %s -> %s\n", cpu_num, ToInternedString(state.current).string,
              ToInternedString(state.target).string);

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

        default: {
          // Complete the requested transition, which could be interrupted by a wake trigger.
          StateMachine expected = state;
          const bool success = this_idle_power_thread.CompareExchangeState(
              expected, {.current = state.target, .target = state.target});
          DEBUG_ASSERT_MSG(success || expected == kSuspendToWakeup || expected == kWakeup,
                           "current=%s target=%s", ToString(expected.current),
                           ToString(expected.target));
          this_idle_power_thread.complete_.Signal();
          break;
        }
      }

      // If the power thread just transitioned to Active or Wakeup, reschedule to ensure that the
      // run queue is evaluated. Otherwise, this thread may continue to run the idle loop until the
      // next IPI.
      if (state.target == State::Active || state.target == State::Wakeup) {
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
  if (!CompareExchangeState(expected, transitional)) {
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
  StateMachine state;
  do {
    const zx_status_t status = complete_.WaitDeadline(timeout_at, Interruptible::No);
    if (status != ZX_OK) {
      return {status, expected_state};
    }
    state = state_.load(ktl::memory_order_acquire);

    // If the current or target state changed while waiting, this transition may have been reversed
    // before the current thread could observe the successful transition. This can happen if an
    // interrupt transitions the current CPU from Suspend back to Active after a successful
    // transition to Suspend, but before this thread starts waiting for completion.
    if (state.target != target_state) {
      return {ZX_ERR_CANCELED, state.current};
    }
  } while (state.current != target_state);

  // The transition from the expected state to the target state was successful.
  return {ZX_OK, expected_state};
}

zx_status_t IdlePowerThread::TransitionAllActiveToSuspend(zx_time_t resume_at) {
  // Prevent re-entrant calls to suspend.
  Guard<Mutex> guard{TransitionLock::Get()};

  if (resume_at < current_time()) {
    return ZX_ERR_TIMED_OUT;
  }

  // Conditionally set the global suspended flag so that other subsystems can act appropriately
  // during suspend. If there are pending wake events, abort the suspend.
  {
    uint64_t expected = SystemSuspendStateActive;
    if (!system_suspend_state_.compare_exchange_strong(expected, SystemSuspendStateSuspendedBit,
                                                       ktl::memory_order_release,
                                                       ktl::memory_order_relaxed)) {
      DEBUG_ASSERT((expected & SystemSuspendStateSuspendedBit) != SystemSuspendStateSuspendedBit);
      const uint64_t pending_wake_events = expected & SystemSuspendStateWakeEventPendingMask;
      dprintf(INFO, "Aborting suspend due to %" PRIu64 " pending wake events\n",
              pending_wake_events);
      return ZX_ERR_BAD_STATE;
    }
  }

  auto restore_suspend_flag = fit::defer([] {
    system_suspend_state_.fetch_and(~SystemSuspendStateSuspendedBit, ktl::memory_order_release);
  });

  // Move to the boot CPU which will be suspended last.
  auto restore_affinity = fit::defer(
      [previous_affinity = Thread::Current::Get()->SetCpuAffinity(cpu_num_to_mask(BOOT_CPU_ID))] {
        Thread::Current::Get()->SetCpuAffinity(previous_affinity);
      });
  DEBUG_ASSERT(arch_curr_cpu_num() == BOOT_CPU_ID);

  // TODO(eieio): Consider temporarily pinning the debuglog notifier thread to the boot CPU to give
  // it a chance to notify readers during the non-boot CPU suspend sequence. This is not needed for
  // correctness, since the debuglog critical sections are protected by a spinlock, which prevents
  // the local CPU from being suspended while held. However, given that readers might also get
  // suspended on non-boot CPUs, this may not have much effect on timeliness of log output.

  // Keep track of which non-boot CPUs to resume.
  const cpu_mask_t cpus_active_before_suspend =
      mp_get_active_mask() & ~cpu_num_to_mask(BOOT_CPU_ID);
  dprintf(INFO, "Active non-boot CPUs before suspend: %#x\n", cpus_active_before_suspend);

  // Suspend all of the active CPUs besides the boot CPU.
  dprintf(INFO, "Suspending non-boot CPUs...\n");

  const Deadline suspend_timeout_at = Deadline::after(ZX_MIN(1));
  cpu_mask_t cpus_to_suspend = cpus_active_before_suspend;
  while (cpus_to_suspend != 0) {
    const cpu_num_t cpu_id = highest_cpu_set(cpus_to_suspend);
    const TransitionResult active_to_suspend_result =
        percpu::Get(cpu_id).idle_power_thread.TransitionFromTo(State::Active, State::Suspend,
                                                               suspend_timeout_at.when());
    if (active_to_suspend_result.status != ZX_OK) {
      KERNEL_OOPS("Failed to transition CPU %u to suspend: %d\n", cpu_id,
                  active_to_suspend_result.status);
      break;
    }

    cpus_to_suspend &= ~cpu_num_to_mask(cpu_id);
  }

  if (cpus_to_suspend != 0) {
    dprintf(INFO, "Aborting suspend and resuming non-boot CPUs...\n");
  } else {
    dprintf(INFO, "Done suspending non-boot CPUs.\n");

    // Set the resume timer before suspending the boot CPU.
    if (resume_at != ZX_TIME_INFINITE) {
      dprintf(INFO, "Setting boot CPU to resume at time %" PRId64 "\n", resume_at);
      resume_timer_.SetOneshot(
          resume_at,
          +[](Timer* timer, zx_time_t now, void* resume_at_ptr) {
            // Verify this handler is running in the correct context.
            DEBUG_ASSERT(arch_curr_cpu_num() == BOOT_CPU_ID);

            WakeEvent wake_event;
            const WakeResult wake_result = wake_event.Trigger();
            const char* message_prefix = wake_result == WakeResult::SuspendAborted
                                             ? "Wakeup before suspend completed. Aborting suspend"
                                             : "Resuming boot CPU";

            const zx_duration_t resume_delta =
                zx_time_sub_time(now, *static_cast<zx_time_t*>(resume_at_ptr));
            dprintf(INFO, "%s at time %" PRId64 ", %" PRId64 "ns after target resume time.\n",
                    message_prefix, now, resume_delta);
          },
          &resume_at);
    } else {
      // TODO(eieio): Check that at least one wake source is configured if no resume time is set.
      // Maybe this check needs require at least one wake source if the resume time is too far in
      // the future?
      dprintf(INFO, "No resume timer set. System will not resume.\n");
    }

    dprintf(INFO, "Attempting to suspend boot CPU...\n");

    // Suspend the boot CPU to finalize the active CPU suspend operation.
    IdlePowerThread& boot_idle_power_thread = percpu::Get(BOOT_CPU_ID).idle_power_thread;
    const TransitionResult active_to_suspend_result = boot_idle_power_thread.TransitionFromTo(
        State::Active, State::Suspend, suspend_timeout_at.when());

    // Cancel the resume timer in case it wasn't the reason for the wakeup.
    resume_timer_.Cancel();

    switch (active_to_suspend_result.status) {
      case ZX_ERR_TIMED_OUT:
        dprintf(INFO, "Timed out waiting for boot CPU to suspend!\n");
        break;

      case ZX_ERR_CANCELED:
        dprintf(INFO, "Boot CPU resumed.\n");
        DEBUG_ASSERT_MSG(active_to_suspend_result.starting_state == State::Wakeup,
                         "startring_state=%s", ToString(active_to_suspend_result.starting_state));
        break;

      case ZX_ERR_BAD_STATE:
        dprintf(INFO, "Boot CPU suspend aborted.\n");
        DEBUG_ASSERT_MSG(active_to_suspend_result.starting_state == State::Wakeup,
                         "startring_state=%s", ToString(active_to_suspend_result.starting_state));
        break;

      default:
        DEBUG_ASSERT_MSG(
            false, "Unexpected result from boot CPU suspend: status=%d starting_state=%s",
            active_to_suspend_result.status, ToString(active_to_suspend_result.starting_state));
    }

    // If the boot CPU is in the Wakeup state, set it to Active.
    StateMachine expected = kWakeup;
    const bool succeeded = boot_idle_power_thread.CompareExchangeState(expected, kActive);
    DEBUG_ASSERT_MSG(succeeded || expected == kActive, "current=%s target=%s",
                     ToString(expected.current), ToString(expected.target));
  }

  // Resume all non-boot CPUs that were successfully suspended.
  dprintf(INFO, "Resuming non-boot CPUs...\n");

  const Deadline resume_timeout_at = Deadline::after(ZX_MIN(1));
  cpu_mask_t cpus_to_resume = cpus_active_before_suspend ^ cpus_to_suspend;
  while (cpus_to_resume != 0) {
    const cpu_num_t cpu_id = highest_cpu_set(cpus_to_resume);
    const TransitionResult suspend_to_active_result =
        percpu::Get(cpu_id).idle_power_thread.TransitionFromTo(State::Suspend, State::Active,
                                                               resume_timeout_at.when());
    if (suspend_to_active_result.status != ZX_OK) {
      KERNEL_OOPS("Failed to transition CPU %u to active: %d\n", cpu_id,
                  suspend_to_active_result.status);
      break;
    }

    cpus_to_resume &= ~cpu_num_to_mask(cpu_id);
  }
  DEBUG_ASSERT(cpus_to_resume == 0);

  dprintf(INFO, "Done resuming non-boot CPUs.\n");
  return ZX_OK;
}

IdlePowerThread::WakeResult IdlePowerThread::WakeBootCpu() {
  DEBUG_ASSERT(system_suspended());
  DEBUG_ASSERT(arch_ints_disabled());
  DEBUG_ASSERT(Thread::Current::Get()->preemption_state().PreemptIsEnabled() == false);

  // Poke the boot CPU when preemption is reenabled to ensure it responds to the potential state
  // change.
  Thread::Current::Get()->preemption_state().PreemptSetPending(cpu_num_to_mask(BOOT_CPU_ID));

  IdlePowerThread& boot_idle_power_thread = percpu::Get(BOOT_CPU_ID).idle_power_thread;

  // Attempt to transition the boot CPU from Suspend to Wakeup. This method may be running on the
  // boot CPU and interrupting the context of the idle/power thread.
  StateMachine expected = kSuspend;
  do {
    bool success = boot_idle_power_thread.CompareExchangeState(expected, kSuspendToWakeup);
    if (success) {
      return WakeResult::Resumed;
    }

    // If the current state is Active or Active-to-Suspend, this wake up occurred before or during
    // the Active-to-Suspend transition of the boot CPU. Set the state to Wakeup to prevent the
    // Active-to-Suspend transition, which would cause this wake up to be missed.
    if (expected == kActive || expected == kActiveToSuspend) {
      success = boot_idle_power_thread.CompareExchangeState(expected, kWakeup);
      if (success) {
        return WakeResult::SuspendAborted;
      }
    }

    // Try again if the previous attempt collided with completing the Active-to-Suspend transition.
  } while (expected == kSuspend);

  // Racing with another wake trigger can cause the attempts above to gracefully fail. However,
  // colliding with other states is an error.
  DEBUG_ASSERT_MSG(expected == kSuspendToWakeup || expected == kWakeup, "current=%s target=%s",
                   ToString(expected.current), ToString(expected.target));
  return WakeResult::Resumed;
}

IdlePowerThread::WakeResult IdlePowerThread::TriggerSystemWakeEvent() {
  DEBUG_ASSERT(arch_ints_disabled());
  DEBUG_ASSERT(Thread::Current::Get()->preemption_state().PreemptIsEnabled() == false);

  const uint64_t previous = system_suspend_state_.fetch_add(1, ktl::memory_order_relaxed);
  const uint64_t previous_pending_wake_events = previous & SystemSuspendStateWakeEventPendingMask;
  const bool suspended = previous & SystemSuspendStateSuspendedBit;

  DEBUG_ASSERT_MSG(previous_pending_wake_events != SystemSuspendStateWakeEventPendingMask,
                   "Pending wake event count overflow!");

  // Wake the boot CPU if the system is suspended and this is the first pending wake event.
  if (suspended && previous_pending_wake_events == 0) {
    return WakeBootCpu();
  }
  return suspended ? WakeResult::Resumed : WakeResult::Active;
}

void IdlePowerThread::AcknowledgeSystemWakeEvent() {
  const uint64_t previous = system_suspend_state_.fetch_add(-1, ktl::memory_order_relaxed);
  const uint64_t previous_pending_wake_events = previous & SystemSuspendStateWakeEventPendingMask;
  DEBUG_ASSERT_MSG(previous_pending_wake_events != 0, "Pending wake event count underflow!");
}

#include <lib/console.h>

static int cmd_suspend(int argc, const cmd_args* argv, uint32_t flags) {
  if (argc < 2) {
  notenoughargs:
    printf("not enough arguments\n");
  badunits:
    printf("%s enter <resume delay> <m|s|ms|us|ns>\n", argv[0].str);
    return -1;
  }

  if (!strcmp(argv[1].str, "enter")) {
    if (argc < 4) {
      goto notenoughargs;
    }

    zx_duration_t delay = argv[2].i;
    const char* units = argv[3].str;
    if (!strcmp(units, "m")) {
      delay = ZX_MIN(delay);
    } else if (!strcmp(units, "s")) {
      delay = ZX_SEC(delay);
    } else if (!strcmp(units, "ms")) {
      delay = ZX_MSEC(delay);
    } else if (!strcmp(units, "us")) {
      delay = ZX_USEC(delay);
    } else if (!strcmp(units, "ns")) {
      delay = ZX_NSEC(delay);
    } else {
      printf("Invalid units: %s\n", units);
      goto badunits;
    }

    const zx_time_t resume_at = zx_time_add_duration(current_time(), delay);
    return IdlePowerThread::TransitionAllActiveToSuspend(resume_at);
  }

  return 0;
}

STATIC_COMMAND_START
STATIC_COMMAND_MASKED("suspend", "processor suspend commands", &cmd_suspend, CMD_AVAIL_ALWAYS)
STATIC_COMMAND_END(suspend)
