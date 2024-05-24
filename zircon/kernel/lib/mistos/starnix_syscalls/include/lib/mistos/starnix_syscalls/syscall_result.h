// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_SYSCALLS_INCLUDE_LIB_MISTOS_STARNIX_SYSCALLS_SYSCALL_RESULT_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_SYSCALLS_INCLUDE_LIB_MISTOS_STARNIX_SYSCALLS_SYSCALL_RESULT_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix_uapi/user_address.h>

namespace starnix_syscalls {

class SyscallResult {
 public:
  SyscallResult(uint64_t value) : value_(value) {}

  inline uint64_t value() const { return value_; }

  static SyscallResult From(const starnix_uapi::UserAddress& value) {
    return SyscallResult(static_cast<uint64_t>(value.ptr()));
  }

  static SyscallResult From(pid_t value) { return SyscallResult(static_cast<uint64_t>(value)); }
  static SyscallResult From(uid_t value) { return SyscallResult(static_cast<uint64_t>(value)); }
  static SyscallResult From(size_t value) { return SyscallResult(static_cast<uint64_t>(value)); }

  bool operator==(const SyscallResult& other) const { return value_ == other.value_; }

 private:
  uint64_t value_;
};

const SyscallResult SUCCESS = SyscallResult(0);

}  // namespace starnix_syscalls

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_SYSCALLS_INCLUDE_LIB_MISTOS_STARNIX_SYSCALLS_SYSCALL_RESULT_H_
