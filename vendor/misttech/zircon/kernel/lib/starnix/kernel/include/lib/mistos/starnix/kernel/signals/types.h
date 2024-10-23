// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/waiter.h>
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

class SignalState {
 private:
  // See https://man7.org/linux/man-pages/man2/sigaltstack.2.html
  ktl::optional<sigaltstack> alt_stack_;

  /// Wait queue for signalfd and sigtimedwait. Signaled whenever a signal is added to the queue.
  WaitQueue signal_wait_;

  /// A handle for interrupting this task, if any.
  // RunState run_state_;

  /// The signal mask of the task.
  ///
  /// It is the set of signals whose delivery is currently blocked for the caller.
  /// See https://man7.org/linux/man-pages/man7/signal.7.html
  starnix_uapi::SigSet mask_;

  /// The signal mask that should be restored by the signal handling machinery, after dequeuing
  /// a signal.
  ///
  /// Some syscalls apply a temporary signal mask by setting `SignalState.mask` during the wait.
  /// This means that the mask must be set to the temporary mask when the signal is dequeued,
  /// which is done by the syscall dispatch loop before returning to userspace. After the signal
  /// is dequeued `mask` can be reset to `saved_mask`.
  ktl::optional<starnix_uapi::SigSet> saved_mask_;

  /// The queue of signals for the task.
  // QueuedSignals queue_;

 public:
  static SignalState with_mask(starnix_uapi::SigSet mask) { return SignalState(mask); }

  starnix_uapi::SigSet set_mask(starnix_uapi::SigSet signal_mask) {
    starnix_uapi::SigSet old_mask = mask_;
    mask_ = signal_mask & ~starnix_uapi::UNBLOCKABLE_SIGNALS;
    return old_mask;
  }

  void set_temporary_mask(starnix_uapi::SigSet signal_mask) {
    ASSERT(!saved_mask_.has_value());
    saved_mask_ = mask_;
    mask_ = signal_mask & ~starnix_uapi::UNBLOCKABLE_SIGNALS;
  }

  void restore_mask() {
    if (saved_mask_.has_value()) {
      mask_ = *saved_mask_;
      saved_mask_.reset();
    }
  }

  starnix_uapi::SigSet mask() const { return mask_; }

  ktl::optional<starnix_uapi::SigSet> saved_mask() const { return saved_mask_; }

 private:
  explicit SignalState(starnix_uapi::SigSet mask) : mask_(mask) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_
