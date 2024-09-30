// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_NUMBERS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_NUMBERS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fs_args.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/strings/utf_codecs.h>
#include <zircon/types.h>

namespace starnix {

class FdNumber {
 public:
  // impl FdNumber
  static const FdNumber AT_FDCWD_;

  static FdNumber from_raw(int32_t n) { return FdNumber(n); }

  int32_t raw() const { return number_; }

  /// Parses a file descriptor number from a byte string.
  static fit::result<Errno, FdNumber> from_fs_str(const FsStr& s) {
    if (!util::IsStringUTF8(s)) {
      return fit::error(errno(EINVAL));
    }
    auto result = parse<int32_t>(s);
    if (result.is_error())
      return result.take_error();
    return fit::ok(FdNumber(result.value()));
  }

  bool operator==(const FdNumber& other) const { return number_ == other.number_; }

  operator starnix_syscalls::SyscallResult() const {
    return starnix_syscalls::SyscallResult(raw());
  }

 private:
  explicit FdNumber(int32_t n) : number_(n) {}

  int32_t number_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_NUMBERS_H_
