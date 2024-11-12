// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_SIGNAL_HANDLING_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_SIGNAL_HANDLING_H_

#include <lib/mistos/starnix/kernel/arch/x64/lib.h>
#include <lib/mistos/starnix/kernel/arch/x64/registers.h>
#include <lib/mistos/starnix/kernel/task/exit_status.h>
#include <lib/mistos/starnix/kernel/task/task.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

#include <linux/signal.h>

namespace starnix {

/// Indicates where in the signal queue a signal should go.  Signals
/// can jump the queue when being injected by tools like ptrace.
enum SignalPriority {
  First,
  Last,
};

// `send_signal*()` calls below may fail only for real-time signals (with EAGAIN). They are
// expected to succeed for all other signals.
void send_signal_first(const fbl::RefPtr<Task>& task,
                       starnix_sync::RwLock<TaskMutableState>::RwLockWriteGuard task_state,
                       SignalInfo siginfo);

// Sends `signal` to `task`. The signal must be a standard (i.e. not real-time) signal.
void send_standard_signal(const fbl::RefPtr<Task>& task, SignalInfo siginfo);

fit::result<Errno> send_signal(const fbl::RefPtr<Task>& task, SignalInfo siginfo);
fit::result<Errno> send_signal_prio(
    const fbl::RefPtr<Task>& task,
    starnix_sync::RwLock<TaskMutableState>::RwLockWriteGuard task_state, SignalInfo siginfo,
    SignalPriority prio, bool force_wake);

/// Represents the action to take when signal is delivered.
///
/// See https://man7.org/linux/man-pages/man7/signal.7.html.
class DeliveryAction {
 public:
  enum Type {
    Ignore,
    CallHandler,
    Terminate,
    CoreDump,
    Stop,
    Continue,
  };

  explicit DeliveryAction(Type type) : type_(type) {}

  /// Returns whether the target task must be interrupted to execute the action.
  ///
  /// The task will not be interrupted if the signal is the action is the Continue action, or if
  /// the action is Ignore and the user specifically requested to ignore the signal.
  bool must_interrupt(struct sigaction sigaction) const {
    switch (type_) {
      case Continue:
        return false;
      case Ignore:
        return sigaction.sa_handler == SIG_IGN;
      default:
        return true;
    }
  }

  bool operator==(const Type& type) const { return this->type_ == type; }
  bool operator!=(const Type& type) const { return !(*this == type); }

  Type type() { return type_; }

 private:
  Type type_;
};

DeliveryAction action_for_signal(SignalInfo siginfo, struct sigaction sigaction);

/// Dequeues and handles a pending signal for `current_task`.
void dequeue_signal(CurrentTask& current_task);

ktl::optional<ExitStatus> deliver_signal(
    const fbl::RefPtr<Task>& task,
    starnix_sync::RwLock<TaskMutableState>::RwLockWriteGuard task_state, SignalInfo siginfo,
    RegisterState* registers, ExtendedPstateState* extended_pstate);

#if 0
/// Prepares `current` state to execute the signal handler stored in `action`.
///
/// This function stores the state required to restore after the signal handler on the stack.
void dispatch_signal_handler(fbl::RefPtr<Task>& task, RegisterState* registers,
                             ExtendedPstateState* extended_pstate, SignalState* signal_state,
                             SignalInfo siginfo, struct ::sigaction action);

void restore_from_signal_handler(CurrentTask& current_task);

/// Maybe adjust a task's registers to restart a syscall once the task switches back to userspace,
/// based on whether the return value is one of the restartable error codes such as ERESTARTSYS.
void prepare_to_restart_syscall(RegisterState& registers,
                                std::optional<struct ::sigaction> sigaction);
#endif

#if MISTOS_STARNIX_TEST
class AutoReleasableTask;
namespace testing {
void dequeue_signal_for_test(starnix::AutoReleasableTask& current_task);
}
#endif

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_SIGNAL_HANDLING_H_
