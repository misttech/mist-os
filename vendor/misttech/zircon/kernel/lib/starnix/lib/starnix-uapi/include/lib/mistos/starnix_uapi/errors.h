// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_

#include <lib/fit/result.h>
#include <zircon/types.h>

#include <linux/errno.h>

class ErrnoCode {
 public:
  explicit ErrnoCode(uint32_t code) : code_(code) {}

  static ErrnoCode from_return_value(uint64_t retval) {
    int64_t rv = static_cast<int64_t>(retval);
    if (rv >= 0) {
      return ErrnoCode(0);
    }
    return ErrnoCode(static_cast<uint32_t>(-rv));
  }

  static ErrnoCode from_error_code(int16_t code) { return ErrnoCode(static_cast<uint32_t>(code)); }
  uint64_t return_value() const { return -(static_cast<int64_t>(code_)); }
  uint32_t error_code() const { return code_; }

  bool operator==(ErrnoCode other) const { return code_ == other.code_; }

 private:
  uint32_t code_;
};

class Errno {
 public:
  static Errno New(const ErrnoCode& code) { return Errno(code); }
  // static Errno Fail(const ErrnoCode& code) { return Errno(code); }
  uint64_t return_value() const { return code_.return_value(); }
  uint32_t error_code() const { return code_.error_code(); }

  bool operator==(const Errno& other) const { return code_ == other.code_; }

 private:
  friend zx_status_t From(const Errno& code);

  explicit Errno(const ErrnoCode& code) : code_(code) {}
  ErrnoCode code_{0};
};

zx_status_t From(const Errno& code);
uint32_t from_status_like_fdio(zx_status_t);

// Define macro errno!
#define errno(err) Errno::New(ErrnoCode(err))

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

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_
