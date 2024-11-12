// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_STATE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_STATE_H_

#include <lib/mistos/starnix/kernel/arch/x64/lib.h>
#include <lib/mistos/starnix/kernel/arch/x64/registers.h>

namespace starnix {

// The thread related information of a `CurrentTask`. The information should never be used outside
// of the thread owning the `CurrentTask`.
struct ThreadState {
  // A copy of the registers associated with the Zircon thread. Up-to-date values can be read
  // from `self.handle.read_state_general_regs()`. To write these values back to the thread, call
  // `self.handle.write_state_general_regs(self.thread_state.registers.into())`.
  RegisterState registers;

  /// Copy of the current extended processor state including floating point and vector registers.
  ExtendedPstateState extended_pstate;

  /// A custom function to resume a syscall that has been interrupted by SIGSTOP.
  /// To use, call set_syscall_restart_func and return ERESTART_RESTARTBLOCK. sys_restart_syscall
  /// will eventually call it.
  // pub syscall_restart_func: Option<Box<SyscallRestartFunc>>,

  /// impl ThreadState
 public:
  /// Returns a new `ThreadState` with the same `registers` as this one.
  ThreadState snapshot() const {
    return ThreadState{.registers = RegisterState::From(*this->registers), .extended_pstate = {}};
  }

  ThreadState extended_snapshot() const {
    return ThreadState{.registers = this->registers, .extended_pstate = this->extended_pstate};
  }

  void replace_registers(const ThreadState& other) {
    this->registers = other.registers;
    this->extended_pstate = other.extended_pstate;
  }
};

/// This contains thread state that tracers can inspect and modify.  It is
/// captured when a thread stops, and optionally copied back (if dirty) when a
/// thread starts again.  An alternative implementation would involve the
/// tracers acting on thread state directly; however, this would involve sharing
/// CurrentTask structures across multiple threads, which goes against the
/// intent of the design of CurrentTask.
struct CapturedThreadState {
  /// The thread state of the traced task. This is copied out when the thread stops.
  ThreadState thread_state;

  /// Indicates that the last ptrace operation changed the thread state, so it should be written
  /// back to the original thread.
  bool dirty;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_STATE_H_
