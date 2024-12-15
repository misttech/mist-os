// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYSCALLS_INCLUDE_LIB_MISTOS_STARNIX_SYSCALLS_SYSCALL_ARG_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYSCALLS_INCLUDE_LIB_MISTOS_STARNIX_SYSCALLS_SYSCALL_ARG_H_

#include <zircon/types.h>

namespace starnix_syscalls {

class SyscallArg {
 public:
  explicit SyscallArg(uint64_t raw = 0) : value_(raw) {}

  static SyscallArg from_raw(uint64_t raw) { return SyscallArg(raw); }

  uint64_t raw() const { return value_; }

 private:
  uint64_t value_;
};

template <typename T>
T from(SyscallArg arg);

}  // namespace starnix_syscalls

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYSCALLS_INCLUDE_LIB_MISTOS_STARNIX_SYSCALLS_SYSCALL_ARG_H_
