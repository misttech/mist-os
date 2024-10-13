// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <zircon/types.h>

#include <ktl/array.h>
#include <ktl/variant.h>

#include <linux/signal.h>

namespace starnix {

struct SignalInfoHeader {
  uint32_t signo;
  int32_t errno;
  int32_t code;
  int32_t _pad;
};

struct KillDetail {
  pid_t pid;
  uid_t uid;
};

struct SIGCHLDDetail {
  pid_t pid;
  uid_t uid;
  int32_t status;
};

struct SigFaultDetail {
  uint64_t addr;
};

struct SIGSYSDetail {
  starnix_uapi::UserAddress call_addr;
  int32_t syscall;
  uint32_t arch;
};

struct TimerDetail {
  // Assuming IntervalTimerHandle is a type defined elsewhere in your code.
  // IntervalTimerHandle timer;
};

constexpr size_t SI_HEADER_SIZE = sizeof(SignalInfoHeader);

struct RawDetail {
  ktl::array<uint8_t, SI_MAX_SIZE - SI_HEADER_SIZE> data;
};

using SignalDetail = ktl::variant<std::monostate, KillDetail, SIGCHLDDetail, SigFaultDetail,
                                  SIGSYSDetail, TimerDetail, RawDetail>;

struct SignalInfo {
  starnix_uapi::Signal signal;
  int32_t errno;
  int32_t code;
  int32_t _pad;
  SignalDetail detail;
  bool force;

  static SignalInfo Default(starnix_uapi::Signal signal) {
    return New(signal, SI_KERNEL, std::monostate{});
  }

  static SignalInfo New(starnix_uapi::Signal signal, int32_t code, SignalDetail detail) {
    return SignalInfo{
        .signal = signal, .errno = 0, .code = code, ._pad = 0, .detail = detail, .force = false};
  }

  static SignalInfo new_kill(starnix_uapi::Signal signal, pid_t pid, uid_t uid) {
    return SignalInfo{.signal = signal,
                      .errno = 0,
                      .code = SI_USER,
                      ._pad = 0,
                      .detail = KillDetail{.pid = pid, .uid = uid},
                      .force = false};
  }

  static SignalInfo new_sigchld(pid_t pid, uid_t uid, int32_t status, int32_t code) {
    return SignalInfo{.signal = starnix_uapi::kSIGCHLD,
                      .errno = 0,
                      .code = code,
                      ._pad = 0,
                      .detail = SIGCHLDDetail{.pid = pid, .uid = uid, .status = status},
                      .force = false};
  }

  static SignalInfo new_sigfault(starnix_uapi::Signal signal, uint64_t addr, int32_t errno) {
    return SignalInfo{.signal = signal,
                      .errno = errno,
                      .code = SI_KERNEL,
                      ._pad = 0,
                      .detail = SigFaultDetail{.addr = addr},
                      .force = false};
  }

  static SignalInfo new_sigsys(starnix_uapi::UserAddress call_addr, int32_t syscall,
                               uint32_t arch) {
    return SignalInfo{
        .signal = starnix_uapi::kSIGSYS,
        .errno = 0,
        .code = SYS_SECCOMP,
        ._pad = 0,
        .detail = SIGSYSDetail{.call_addr = call_addr, .syscall = syscall, .arch = arch},
        .force = false};
  }

  static SignalInfo new_timer(starnix_uapi::Signal signal) {
    return SignalInfo{.signal = signal,
                      .errno = 0,
                      .code = SI_TIMER,
                      ._pad = 0,
                      .detail = TimerDetail{},
                      .force = false};
  }
};

class SignalState {};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_
