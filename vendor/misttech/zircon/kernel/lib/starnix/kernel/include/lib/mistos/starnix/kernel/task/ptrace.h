// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PTRACE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PTRACE_H_

#include <zircon/types.h>

// #include <linux/ptrace.h>

namespace starnix {

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

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PTRACE_H_
