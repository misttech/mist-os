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
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <algorithm>
#include <deque>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/array.h>
#include <ktl/variant.h>

#include <linux/signal.h>

namespace starnix {

using starnix_sync::InterruptibleEvent;
using starnix_uapi::Signal;
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

  // Convert SignalInfo to siginfo_t bytes
  ktl::array<uint8_t, sizeof(siginfo_t)> as_siginfo_bytes() const {
    ktl::array<uint8_t, sizeof(siginfo_t)> array = {};
    siginfo_t info = {};

    memcpy(array.data(), &info, sizeof(siginfo_t));
    return array;
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

// SignalActions contains a sigaction for each valid signal.
class SignalActions : public fbl::RefCounted<SignalActions> {
 public:
  static fbl::RefPtr<SignalActions> Default() {
    fbl::AllocChecker ac;
    auto tmp = fbl::AdoptRef(new (&ac) SignalActions());
    ZX_ASSERT(ac.check());
    return tmp;
  }

  // Returns the sigaction that is currently set for signal.
  sigaction Get(Signal signal) const { return actions_.Lock()->at(signal.number()); }

  // Update the action for signal. Returns the previously configured action.
  sigaction Set(Signal signal, sigaction new_action) {
    auto actions = actions_.Lock();
    sigaction old_action = actions->at(signal.number());
    actions->at(signal.number()) = new_action;
    return old_action;
  }

  fbl::RefPtr<SignalActions> Fork() const {
    fbl::AllocChecker ac;
    auto tmp = fbl::AdoptRef(new (&ac) SignalActions());
    ZX_ASSERT(ac.check());
    *tmp->actions_.Lock() = *this->actions_.Lock();
    return tmp;
  }

  void ResetForExec() {
    auto actions = actions_.Lock();
    for (auto& action : *actions) {
      if (action.sa_handler != SIG_DFL && action.sa_handler != SIG_IGN) {
        action.sa_handler = SIG_DFL;
      }
    }
  }

 private:
  SignalActions() {
    auto actions = actions_.Lock();
    actions->fill(sigaction{});
  }

  DISALLOW_COPY_ASSIGN_AND_MOVE(SignalActions);

  mutable starnix_sync::Mutex<ktl::array<sigaction, Signal::NUM_SIGNALS + 1>> actions_;
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
  static RunState Waiter(WaiterRef ref) { return RunState(ref); }

  /// This thread is blocked in an `InterruptibleEvent`.
  static RunState Event(fbl::RefPtr<InterruptibleEvent> event) { return RunState(event); }

  bool is_blocked() const {
    return ktl::visit(overloaded{[](const ktl::monostate&) { return false; },
                                 [](const WaiterRef& ref) { return ref.is_valid(); },
                                 [](const fbl::RefPtr<InterruptibleEvent>&) { return true; }},
                      variant_);
  }

  void wake() const {
    ktl::visit(overloaded{[](const ktl::monostate&) {
                            // Do nothing for Running state
                          },
                          [](const WaiterRef& waiter) { waiter.interrupt(); },
                          [](const fbl::RefPtr<InterruptibleEvent>& event) {
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

  RunState(RunState&& other) { variant_ = ktl::move(other.variant_); }
  RunState(const RunState& other) { variant_ = other.variant_; }

  RunState& operator=(RunState&& other) {
    variant_ = ktl::move(other.variant_);
    return *this;
  }

  RunState& operator=(const RunState& other) = default;

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
 private:
  using SignalQueue = std::deque<SignalInfo, util::Allocator<SignalInfo>>;

  /// The queue of standard signals for the task.
  SignalQueue queue_;

  // Real-time signals queued for the task. Unlike standard signals there may be more than one
  /// instance of the same real-time signal in the queue. POSIX requires real-time signals
  /// with lower values to be delivered first. `enqueue()` ensures proper ordering when adding
  /// new elements. There are no ordering requirements for standard signals. We always dequeue
  /// standard signals first. This matches Linux behavior.
  SignalQueue rt_queue_;

  /// impl QueuedSignals
 public:
  void enqueue(const SignalInfo& siginfo) {
    if (siginfo.signal.is_real_time()) {
      // Real-time signals are stored in `rt_queue` in the order they will be delivered,
      // i.e. they sorted by the signal number. Signals with the same number must be
      // delivered in the order they were queued. Use binary search to find the right
      // position to insert the signal. Note that the comparator return `Less` when the
      // signal is the same.
      auto pos = std::ranges::lower_bound(rt_queue_, siginfo,
                                          [](const SignalInfo& a, const SignalInfo& b) {
                                            return a.signal.number() < b.signal.number();
                                          });
      rt_queue_.insert(pos, siginfo);
    } else {
      // Don't queue duplicate standard signals.
      if (std::ranges::find_if(queue_, [&](const SignalInfo& info) {
            return info.signal == siginfo.signal;
          }) == queue_.end()) {
        queue_.push_back(siginfo);
      }
    }
  }

  /// Used by ptrace to provide a replacement for the signal that might have been
  /// delivered when the task entered signal-delivery-stop.
  void jump_queue(const SignalInfo& siginfo) { queue_.push_front(siginfo); }

  /// Finds the next queued signal where the given function returns true, removes it from the
  /// queue, and returns it.
  template <typename F>
  ktl::optional<SignalInfo> take_next_where(F&& predicate) {
    // Find the first signal passing `predicate`, prioritizing standard signals.
    if (auto it = std::ranges::find_if(queue_, predicate); it != queue_.end()) {
      SignalInfo info = *it;
      queue_.erase(it);
      return info;
    }
    if (auto it = std::ranges::find_if(rt_queue_, predicate); it != rt_queue_.end()) {
      SignalInfo info = *it;
      rt_queue_.erase(it);
      return info;
    }
    return ktl::nullopt;
  }

  bool is_empty() const { return queue_.empty() && rt_queue_.empty(); }

  /// Returns whether any signals are queued and not blocked by the given mask.
  bool is_any_allowed_by_mask(SigSet mask) const { return false; }

#if 0
  /// Returns an iterator over all the pending signals.
  auto iter() const {
    //return util::chain(queue, rt_queue);
  }

  /// Iterates over queued signals with the given number.
  auto iter_queued_by_number(starnix_uapi::Signal signal) const {
    return util::filter(iter(), [signal](const SignalInfo& info) {
      return info.signal == signal;
    })
  }
#endif

  /// Returns the set of currently pending signals, both standard and real time.
  starnix_uapi::SigSet pending() const {
    starnix_uapi::SigSet pending;
    // for (const auto& signal : iter()) {
    //   pending |= signal.signal;
    // }
    return pending;
  }

  /// Tests whether a signal with the given number is in the queue.
  bool has_queued(starnix_uapi::Signal signal) const {
    // return iter_queued_by_number(signal).begin() != iter_queued_by_number(signal).end();
    return false;
  }

  size_t num_queued() const { return queue_.size() + rt_queue_.size(); }

#ifdef MISTOS_STARNIX_TEST
  size_t queued_count(starnix_uapi::Signal signal) const { return 0u; }
#endif
};

class SignalState {
 public:
  // See https://man7.org/linux/man-pages/man2/sigaltstack.2.html
  ktl::optional<sigaltstack> alt_stack_;

  /// Wait queue for signalfd and sigtimedwait. Signaled whenever a signal is added to the queue.
  WaitQueue signal_wait_;

  /// A handle for interrupting this task, if any.
  RunState run_state_;

 private:
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
  QueuedSignals queue_;

  /// impl SignalState
 public:
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

  void enqueue(SignalInfo siginfo) {
    queue_.enqueue(siginfo);
    signal_wait_.notify_all();
  }

  // Used by ptrace to provide a replacement for the signal that might have been
  /// delivered when the task entered signal-delivery-stop.
  void jump_queue(SignalInfo siginfo) {
    queue_.jump_queue(siginfo);
    signal_wait_.notify_all();
  }

  bool is_empty() const { return queue_.is_empty(); }

  /// Finds the next queued signal where the given function returns true, removes it from the
  /// queue, and returns it.
  template <typename F>
  ktl::optional<SignalInfo> take_next_where(F&& predicate) {
    // static asseet // using SignalInfoPredicate = bool (*)(const SignalInfo&);
    return queue_.take_next_where(predicate);
  }

  /// Returns whether any signals are pending (queued and not blocked).
  bool is_any_pending() const { return queue_.is_any_allowed_by_mask(mask_); }

  /// Returns whether any signals are queued and not blocked by the given mask.
  bool is_any_allowed_by_mask(starnix_uapi::SigSet mask) const {
    return queue_.is_any_allowed_by_mask(mask);
  }

  SigSet pending() const { return queue_.pending(); }

  /// Tests whether a signal with the given number is in the queue.
  bool has_queued(starnix_uapi::Signal signal) const { return queue_.has_queued(signal); }

  size_t num_queued() const { return queue_.num_queued(); }

#ifdef MISTOS_STARNIX_TEST
  size_t queued_count(starnix_uapi::Signal signal) const { return queue_.queued_count(signal); }
#endif

 private:
  explicit SignalState(starnix_uapi::SigSet mask) : run_state_(RunState::Running()), mask_(mask) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_
