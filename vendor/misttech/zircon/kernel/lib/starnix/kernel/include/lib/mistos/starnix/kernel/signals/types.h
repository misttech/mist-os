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
#include <lib/starnix_sync/interruptible_event.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/ref_ptr.h>
#include <ktl/array.h>
#include <ktl/variant.h>

#include <linux/signal.h>

namespace starnix {

using starnix_sync::InterruptibleEvent;
using starnix_uapi::SigSet;

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

#if 0
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
#endif
};

// Whether, and how, this task is blocked. This enum can be extended with new
// variants to optimize different kinds of waiting.
class RunState {
 public:
  using Variant = ktl::variant<ktl::monostate, WaiterRef, fbl::RefPtr<InterruptibleEvent>>;

  /// This task is not blocked.
  ///
  /// The task might be running in userspace or kernel.
  static RunState Running() { return RunState({}); }

  /// This thread is blocked in a `Waiter`.
  static RunState Waiter(WaiterRef ref) { return RunState(ktl::move(ref)); }

  /// This thread is blocked in an `InterruptibleEvent`.
  static RunState Event(fbl::RefPtr<InterruptibleEvent> event) { return RunState(event); }

  bool is_blocked() const {
    return ktl::visit(overloaded{[](const ktl::monostate&) { return false; },
                                 [](const WaiterRef& ref) { return ref.IsValid(); },
                                 [](const fbl::RefPtr<InterruptibleEvent>&) { return true; }},
                      variant_);
  }

  void wake() {
    ktl::visit(overloaded{[](ktl::monostate&) {
                            // Do nothing for Running state
                          },
                          [](WaiterRef& waiter) { waiter.Interrupt(); },
                          [](fbl::RefPtr<InterruptibleEvent>& event) {
                            // event->Interrupt();
                          }},
               variant_);
  }

  bool operator==(const RunState& other) const {
    return std::visit(
        overloaded{[](const ktl::monostate&, const ktl::monostate&) { return true; },
                   [](const WaiterRef& lhs, const WaiterRef& rhs) { return lhs == rhs; },
                   [](const fbl::RefPtr<InterruptibleEvent>& lhs,
                      const fbl::RefPtr<InterruptibleEvent>& rhs) { return lhs == rhs; },
                   [](const auto&, const auto&) { return false; }},
        variant_, other.variant_);
  }

  bool operator!=(const RunState& other) const { return !(*this == other); }

 private:
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  explicit RunState(Variant variant) : variant_(ktl::move(variant)) {}

  Variant variant_;
};

class QueuedSignals {
 public:
  /// Returns whether any signals are queued and not blocked by the given mask.
  bool is_any_allowed_by_mask(SigSet mask) { return false; }
};

class SignalState {
 public:
  // See https://man7.org/linux/man-pages/man2/sigaltstack.2.html
  ktl::optional<sigaltstack> alt_stack_;

  /// Wait queue for signalfd and sigtimedwait. Signaled whenever a signal is added to the queue.
  WaitQueue signal_wait_;

  /// A handle for interrupting this task, if any.
  RunState run_state_;

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

  /// impl SignalState

  static SignalState with_mask(starnix_uapi::SigSet mask) { return SignalState(mask); }

  /// Sets the signal mask of the state, and returns the old signal mask.
  starnix_uapi::SigSet set_mask(starnix_uapi::SigSet signal_mask) {
    starnix_uapi::SigSet old_mask = mask_;
    mask_ = signal_mask & ~starnix_uapi::UNBLOCKABLE_SIGNALS;
    return old_mask;
  }

  /// Sets the signal mask of the state temporarily, until the signal machinery has completed its
  /// next dequeue operation. This can be used by syscalls that want to change the signal mask
  /// during a wait, but want the signal mask to be reset before returning back to userspace after
  /// the wait.
  void set_temporary_mask(starnix_uapi::SigSet signal_mask) {
    ASSERT(!saved_mask_.has_value());
    saved_mask_ = mask_;
    mask_ = signal_mask & ~starnix_uapi::UNBLOCKABLE_SIGNALS;
  }
  // Restores the signal mask to what it was before the previous call to `set_temporary_mask`.
  /// If there is no saved mask, the mask is left alone.
  void restore_mask() {
    if (saved_mask_.has_value()) {
      mask_ = *saved_mask_;
      saved_mask_.reset();
    }
  }

  starnix_uapi::SigSet mask() const { return mask_; }

  ktl::optional<starnix_uapi::SigSet> saved_mask() const { return saved_mask_; }

  // void enqueue(SignalInfo siginfo) { queue_.enqueue(siginfo); signal_wait_.notify_all(); }

  // void jump_queue(SignalInfo siginfo) { queue_.jump_queue(siginfo); signal_wait_.notify_all(); }

  // bool is_empty() const { return queue_.is_empty(); }

  // ktl::optional<SignalInfo> take_next_where(ktl::function<bool(const SignalInfo&)> predicate) {
  //   return queue_.take_next_where(predicate);
  // }

  bool is_any_pending() const {
    // return queue_.is_any_allowed_by_mask(mask_);
    return false;  // Placeholder until queue_ is implemented
  }

  // bool is_any_allowed_by_mask(starnix_uapi::SigSet mask) const {
  //   return queue_.is_any_allowed_by_mask(mask);
  // }

  // starnix_uapi::SigSet pending() const { return queue_.pending(); }

  // bool has_queued(starnix_uapi::Signal signal) const { return queue_.has_queued(signal); }

  // size_t num_queued() const { return queue_.num_queued(); }

  // #ifdef __Fuchsia_CONFIG_STARNIX_TEST
  // size_t queued_count(starnix_uapi::Signal signal) const { return queue_.queued_count(signal); }
  // #endif

 private:
  explicit SignalState(starnix_uapi::SigSet mask) : run_state_(RunState::Running()), mask_(mask) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_
