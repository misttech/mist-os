// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PTRACE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PTRACE_H_

#include <zircon/types.h>

namespace starnix {

/// For most of the time, for the purposes of ptrace, a tracee is either "going"
/// or "stopped".  However, after certain ptrace calls, there are special rules
/// to be followed.
enum class PtraceStatus : uint8_t {
  /// Proceed as otherwise indicated by the task's stop status.
  Default,
  /// Resuming after a ptrace_cont with a signal, so do not stop for signal-delivery-stop
  Continuing,
  /// "The state of the tracee after PTRACE_LISTEN is somewhat of a
  /// gray area: it is not in any ptrace-stop (ptrace commands won't work on it,
  /// and it will deliver waitpid(2) notifications), but it also may be considered
  /// "stopped" because it is not executing instructions (is not scheduled), and
  /// if it was in group-stop before PTRACE_LISTEN, it will not respond to signals
  /// until SIGCONT is received."
  Listening,
};

/// Indicates the way that ptrace attached to the task.
enum class PtraceAttachType : uint8_t {
  /// Attached with PTRACE_ATTACH
  Attach,
  /// Attached with PTRACE_SEIZE
  Seize,
};

enum class PtraceEvent : uint32_t {
  None = 0,
  /*Stop = PTRACE_EVENT_STOP,
  Clone = PTRACE_EVENT_CLONE,
  Fork = PTRACE_EVENT_FORK,
  Vfork = PTRACE_EVENT_VFORK,
  VforkDone = PTRACE_EVENT_VFORK_DONE,
  Exec = PTRACE_EVENT_EXEC,
  Exit = PTRACE_EVENT_EXIT,
  Seccomp = PTRACE_EVENT_SECCOMP,*/
};

/// Information about what caused a ptrace-event-stop.
struct PtraceEventData {};

/// The ptrace state that a new task needs to connect to the same tracer as the
/// task that clones it.
struct PtraceCoreState {};

/// Per-task ptrace-related state
struct PtraceState {};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PTRACE_H_
