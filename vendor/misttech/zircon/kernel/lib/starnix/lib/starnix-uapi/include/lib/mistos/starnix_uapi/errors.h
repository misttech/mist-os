// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_

#include <lib/fit/result.h>
#include <zircon/types.h>

#include <linux/errno.h>

namespace starnix_uapi {

class ErrnoCode {
 public:
  // impl ErrnoCode
  static ErrnoCode from_return_value(uint64_t retval) {
    int64_t rv = static_cast<int64_t>(retval);
    if (rv >= 0) {
      // Collapse all success codes to 0. This is the only value in the u32 range which
      // is guaranteed to not be an error code.
      return ErrnoCode(0);
    }
    return ErrnoCode(static_cast<uint32_t>(-rv));
  }

  static ErrnoCode from_error_code(int16_t code) { return ErrnoCode(static_cast<uint32_t>(code)); }

  uint64_t return_value() const { return -(static_cast<int64_t>(code_)); }

  uint32_t error_code() const { return code_; }

  // C++
  explicit ErrnoCode(uint32_t code) : code_(code) {}

  bool operator==(ErrnoCode other) const { return code_ == other.code_; }
  bool operator==(uint32_t other) const { return code_ == other; }

 private:
  uint32_t code_;
};

class Errno {
 public:
  ErrnoCode code_{0};

  // impl Errno
  static Errno New(const ErrnoCode& code) { return Errno(code); }

  uint64_t return_value() const { return code_.return_value(); }

  // C++
  uint32_t error_code() const { return code_.error_code(); }

  bool operator==(const Errno& other) const { return code_ == other.code_; }
  bool operator==(const ErrnoCode& other) const { return code_ == other; }

 private:
  explicit Errno(const ErrnoCode& code) : code_(code) {}
};

// Special errors indicating a blocking syscall was interrupted, but it can be restarted.
//
// They are not defined in uapi, but can be observed by ptrace on Linux.
//
// If the syscall is restartable, it might not be restarted, depending on the value of SA_RESTART
// for the signal handler and the specific restartable error code.
// But it will always be restarted if the signal did not call a userspace signal handler.
// If not restarted, this error code is converted into EINTR.
//
// More information can be found at
// https://cs.opensource.google/gvisor/gvisor/+/master:pkg/errors/linuxerr/internal.go;l=71;drc=2bb73c7bd7dcf0b36e774d8e82e464d04bc81f4b.

/// Convert to EINTR if interrupted by a signal handler without SA_RESTART enabled, otherwise
/// restart.
const ErrnoCode ERESTARTSYS(512);

/// Always restart, regardless of the signal handler.
const ErrnoCode ERESTARTNOINTR(513);

/// Convert to EINTR if interrupted by a signal handler. SA_RESTART is ignored. Otherwise restart.
const ErrnoCode ERESTARTNOHAND(514);

/// Like `ERESTARTNOHAND`, but restart by invoking a closure instead of calling the syscall
/// implementation again.
const ErrnoCode ERESTART_RESTARTBLOCK(516);

fit::result<Errno> map_eintr(fit::result<Errno> result, Errno err);

template <typename T>
fit::result<Errno, T> map_eintr(fit::result<Errno, T> result, Errno err) {
  if (result.is_error()) {
    if (result.error_value().error_code() == EINTR) {
      return fit::error(err);
    }
    return result.take_error();
  }
  return result.take_value();
}

// There isn't really a mapping from zx_status::Status to Errno. The correct mapping is
// context-specific but this converter is a reasonable first-approximation. The translation matches
// fdio_status_to_errno. See https://fxbug.dev/42105838 for more context.
// TODO: Replace clients with more context-specific mappings.
uint32_t from_status_like_fdio(zx_status_t);

}  // namespace starnix_uapi

/// `errno` returns an `Errno` struct tagged with the current file name and line number.
///
/// Use `error!` instead if you want the `Errno` to be wrapped in an `Err`.
#define errno(err) starnix_uapi::Errno::New(starnix_uapi::ErrnoCode(err))

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_
