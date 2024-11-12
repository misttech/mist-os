// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/signals/signal_handling.h"

#include <lib/mistos/starnix/kernel/arch/x64/lib.h>
#include <lib/mistos/starnix/kernel/arch/x64/registers.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>

#include <fbl/ref_ptr.h>

#include <linux/signal.h>

namespace starnix {

// `send_signal*()` calls below may fail only for real-time signals (with EAGAIN). They are
// expected to succeed for all other signals.
void send_signal_first(const fbl::RefPtr<Task>& task,
                       starnix_sync::RwLock<TaskMutableState>::RwLockWriteGuard task_state,
                       SignalInfo siginfo) {
  ZX_ASSERT_MSG(
      send_signal_prio(task, ktl::move(task_state), siginfo, SignalPriority::First, true).is_ok(),
      "send_signal(SignalPriority::First) is not expected to fail");
}

// Sends `signal` to `task`. The signal must be a standard (i.e. not real-time) signal.
void send_standard_signal(const fbl::RefPtr<Task>& task, SignalInfo siginfo) {
  ZX_ASSERT(!siginfo.signal.is_real_time());
  auto state = task->Write();
  ZX_ASSERT_MSG(
      send_signal_prio(task, ktl::move(state), siginfo, SignalPriority::Last, false).is_ok(),
      "send_signal(SignalPriority::First) is not expected to fail for standard signals.");
}

fit::result<Errno> send_signal(const fbl::RefPtr<Task>& task, SignalInfo siginfo) {
  auto state = task->Write();
  return send_signal_prio(task, ktl::move(state), siginfo, SignalPriority::Last, false);
}

fit::result<Errno> send_signal_prio(
    const fbl::RefPtr<Task>& task,
    starnix_sync::RwLock<TaskMutableState>::RwLockWriteGuard task_state, SignalInfo siginfo,
    SignalPriority prio, bool force_wake) {
  bool is_masked = task_state->is_signal_masked(siginfo.signal);
  bool was_masked = task_state->is_signal_masked_by_saved_mask(siginfo.signal);
  auto sigaction = task->get_signal_action(siginfo.signal);
  auto action = action_for_signal(siginfo, sigaction);

  if (siginfo.signal.is_real_time() && prio != SignalPriority::First) {
    if (task_state->pending_signal_count() >=
        task->thread_group_->get_rlimit(
            starnix_uapi::Resource{.value = starnix_uapi::ResourceEnum::SIGPENDING})) {
      return fit::error(errno(EAGAIN));
    }
  }

  // If the signal is ignored then it doesn't need to be queued, except the following 2 cases:
  //  1. The signal is blocked by the current or the original mask. The signal may be unmasked
  //     later, see `SigtimedwaitTest.IgnoredUnmaskedSignal` gvisor test.
  //  2. The task is ptraced. In this case we want to queue the signal for signal-delivery-stop.
  bool is_queued =
      (action != DeliveryAction::Ignore) || is_masked || was_masked || task_state->is_ptraced();
  if (is_queued) {
    if (prio == SignalPriority::First) {
      task_state->enqueue_signal_front(siginfo);
    } else {
      task_state->enqueue_signal(siginfo);
    }
    task_state->set_flags(TaskFlags(TaskFlagsEnum::SIGNALS_AVAILABLE), true);
  }

  std::destroy_at(std::addressof(task_state));

  if (is_queued && !is_masked && action.must_interrupt(sigaction)) {
    // Wake the task. Note that any potential signal handler will be executed before
    // the task returns from the suspend (from the perspective of user space).
    task->interrupt();
  }

  // Unstop the process for SIGCONT. Also unstop for SIGKILL, the only signal that can interrupt
  // a stopped process.
  if (siginfo.signal == kSIGKILL) {
    task->thread_group_->set_stopped(StopState::ForceWaking, siginfo, false);
    task->Write()->set_stopped(StopState::ForceWaking, {}, {}, {});
  } else if (siginfo.signal == kSIGCONT || force_wake) {
    task->thread_group_->set_stopped(StopState::Waking, siginfo, false);
    task->Write()->set_stopped(StopState::Waking, {}, {}, {});
  }

  return fit::ok();
}

DeliveryAction action_for_signal(SignalInfo siginfo, struct sigaction sigaction) {
  __sighandler_t handler =
      (siginfo.force && sigaction.sa_handler == SIG_IGN) ? SIG_DFL : sigaction.sa_handler;

  if (handler == SIG_DFL) {
    switch (siginfo.signal.number()) {
      case SIGCHLD:
      case SIGURG:
      case SIGWINCH:
        return DeliveryAction(DeliveryAction::Ignore);
      case SIGHUP:
      case SIGINT:
      case SIGKILL:
      case SIGPIPE:
      case SIGALRM:
      case SIGTERM:
      case SIGUSR1:
      case SIGUSR2:
      case SIGPROF:
      case SIGVTALRM:
      case SIGSTKFLT:
      case SIGIO:
      case SIGPWR:
        return DeliveryAction(DeliveryAction::Terminate);
      case SIGQUIT:
      case SIGILL:
      case SIGABRT:
      case SIGFPE:
      case SIGSEGV:
      case SIGBUS:
      case SIGSYS:
      case SIGTRAP:
      case SIGXCPU:
      case SIGXFSZ:
        return DeliveryAction(DeliveryAction::CoreDump);
      case SIGSTOP:
      case SIGTSTP:
      case SIGTTIN:
      case SIGTTOU:
        return DeliveryAction(DeliveryAction::Stop);
      case SIGCONT:
        return DeliveryAction(DeliveryAction::Continue);
      default:
        if (siginfo.signal.is_real_time()) {
          return DeliveryAction(DeliveryAction::Ignore);
        }
        // TODO: Handle unknown signals more gracefully
        ZX_PANIC("Unknown signal");
    }
  } else if (handler == SIG_IGN) {
    return DeliveryAction(DeliveryAction::Ignore);
  } else {
    return DeliveryAction(DeliveryAction::CallHandler);
  }
}

void dequeue_signal(CurrentTask& current_task) {
  auto& task = current_task.task();
  auto& thread_state = current_task.thread_state();
  auto task_state = task->Write();

  // This code is occasionally executed as the task is stopping. Stopping /
  // stopped threads should not get signals.
  if (StopStateHelper::is_stopping_or_stopped(task->load_stopped())) {
    return;
  }

  auto siginfo = task_state->take_any_signal();
  /*prepare_to_restart_syscall(
      &thread_state.registers,
      siginfo ? task->thread_group()->signal_actions()->Get(siginfo->signal) : std::nullopt);*/

  if (siginfo) {
    /*if (task_state->ptrace_on_signal_consume() && siginfo->signal != kSIGKILL) {
      // Indicate we will be stopping for ptrace at the next opportunity.
      // Whether you actually deliver the signal is now up to ptrace, so
      // we can return.
      task_state->set_stopped(StopState::SignalDeliveryStopping, siginfo, nullptr, nullptr);
      return;
    }*/
  }

  // A syscall may have been waiting with a temporary mask which should be used to dequeue the
  // signal, but after the signal has been dequeued the old mask should be restored.
  task_state->restore_signal_mask();
  {
    auto [clear, set] =
        task_state->pending_signal_count() == 0
            ? ktl::pair(TaskFlags(TaskFlagsEnum::SIGNALS_AVAILABLE), TaskFlags::empty())
            : ktl::pair(TaskFlags::empty(), TaskFlags(TaskFlagsEnum::SIGNALS_AVAILABLE));
    task_state->update_flags(clear | TaskFlags(TaskFlagsEnum::TEMPORARY_SIGNAL_MASK), set);
  }

  if (siginfo) {
    /*if (auto timer = std::get_if<SignalDetail::Timer>(&siginfo->detail)) {
      timer->on_signal_delivered();
    }*/
    if (auto status = deliver_signal(task, ktl::move(task_state), *siginfo, &thread_state.registers,
                                     &thread_state.extended_pstate)) {
      current_task.thread_group_exit(*status);
    }
  }
}

ktl::optional<ExitStatus> deliver_signal(
    const fbl::RefPtr<Task>& task,
    starnix_sync::RwLock<TaskMutableState>::RwLockWriteGuard task_state, SignalInfo siginfo,
    RegisterState* registers, ExtendedPstateState* extended_pstate) {
  while (true) {
    auto sigaction = task->thread_group_->signal_actions_->Get(siginfo.signal);
    auto action = action_for_signal(siginfo, sigaction);
    // log_trace("handling signal %d with action %d", siginfo.signal, action);
    ktl::optional<ExitStatus> result;
    switch (action.type()) {
      case DeliveryAction::Ignore:
        break;
      case DeliveryAction::CallHandler: {
#if 0
        auto signal = siginfo.signal;
        if (dispatch_signal_handler(task, registers, extended_pstate, task_state->signals_mut(),
                                    siginfo, sigaction)
                .is_ok()) {
          if (sigaction.sa_flags & SA_RESETHAND) {
            struct sigaction new_sigaction = {.sa_handler = SIG_DFL,
                                              .sa_flags = sigaction.sa_flags & ~SA_RESETHAND,
                                              .sa_mask = sigaction.sa_mask};
            task->thread_group()->signal_actions()->Set(signal, new_sigaction);
          }
        } else {
          // log_warn("failed to deliver signal %d", signal);
          //siginfo = SignalInfo(SIGSEGV);
          auto new_sigaction = task->thread_group()->signal_actions()->Get(siginfo.signal);
          auto new_action = action_for_signal(siginfo, new_sigaction);
          auto masked_signals = task_state->signal_mask();
          if (signal == SIGSEGV || masked_signals.has_signal(SIGSEGV) ||
              new_action.type_ == DeliveryAction::Ignore) {
            task_state->set_signal_mask(masked_signals & ~SigSet(SIGSEGV));
            task->thread_group()->signal_actions()->Set(SIGSEGV, struct sigaction{});
          }
          continue;
        }
#endif
        break;
      }
      case DeliveryAction::Terminate:
        // Release the signals lock. [`ThreadGroup::exit`] sends signals to threads which
        // will include this one and cause a deadlock re-acquiring the signals lock.
        // task_state.reset();
        result = ExitStatus::Kill(siginfo);
        break;
      case DeliveryAction::CoreDump:
        task_state->set_flags(TaskFlags(TaskFlagsEnum::DUMP_ON_EXIT), true);
        // task_state.reset();
        result = ExitStatus::CoreDump(siginfo);
        break;
      case DeliveryAction::Stop:
        // task_state.reset();
        // task->thread_group()->set_stopped(StopState::GroupStopping, siginfo, false);
        break;
      case DeliveryAction::Continue:
        break;
    }

    if (result.has_value()) {
      return result;
    }
    break;
  }
  return ktl::nullopt;
}

}  // namespace starnix
