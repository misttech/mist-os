// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_EXIT_STATUS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_EXIT_STATUS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/signals/types.h>
#include <lib/mistos/starnix/kernel/task/ptrace.h>
#include <sys/types.h>

#include <ktl/atomic.h>
#include <ktl/variant.h>

namespace starnix {

struct ExitStatusExit {
  uint8_t code;
};

struct ExitStatusKill {
  SignalInfo signal_info;
};

struct ExitStatusCoreDump {
  SignalInfo signal_info;
};

struct ExitStatusStop {
  SignalInfo signal_info;
  PtraceEvent ptrace_event;
};

struct ExitStatusContinue {
  SignalInfo signal_info;
  PtraceEvent ptrace_event;
};

class ExitStatus {
 public:
  using Variant = ktl::variant<ExitStatusExit, ExitStatusKill, ExitStatusCoreDump, ExitStatusStop,
                               ExitStatusContinue>;

  static ExitStatus Exit(uint8_t code) { return ExitStatus(ExitStatusExit{code}); }

  static ExitStatus Kill(SignalInfo signal_info) {
    return ExitStatus(ExitStatusKill{ktl::move(signal_info)});
  }

  static ExitStatus CoreDump(SignalInfo signal_info) {
    return ExitStatus(ExitStatusCoreDump{ktl::move(signal_info)});
  }

  static ExitStatus Stop(SignalInfo signal_info, PtraceEvent ptrace_event) {
    return ExitStatus(
        ExitStatusStop{.signal_info = ktl::move(signal_info), .ptrace_event = ptrace_event});
  }

  static ExitStatus Continue(SignalInfo signal_info, PtraceEvent ptrace_event) {
    return ExitStatus(
        ExitStatusContinue{.signal_info = ktl::move(signal_info), .ptrace_event = ptrace_event});
  }

  // const Variant& variant() const { return variant_; }

  static int wait_status(const ExitStatus& exit_status) {
    return std::visit(
        overloaded{
            [](const ExitStatusExit& status) -> int { return static_cast<int>(status.code) << 8; },
            [](const ExitStatusKill& kill) -> int { return kill.signal_info.signal.number(); },
            [](const ExitStatusCoreDump& core_dump) -> int {
              return core_dump.signal_info.signal.number() | 0x80;
            },
            [](const ExitStatusContinue& cont) -> int {
              uint32_t trace_event_val = static_cast<uint32_t>(cont.ptrace_event);
              return (trace_event_val != 0)
                         ? cont.signal_info.signal.number() | (trace_event_val << 16)
                         : 0xffff;
            },
            [](const ExitStatusStop& stop) -> int {
              uint32_t trace_event_val = static_cast<uint32_t>(stop.ptrace_event);
              return (0x7f + (stop.signal_info.signal.number() << 8)) | (trace_event_val << 16);
            }},
        exit_status.variant_);
  }

  static int signal_info_code(const ExitStatus& exit_status) {
    return std::visit(overloaded{[](const ExitStatusExit&) -> int { return CLD_EXITED; },
                                 [](const ExitStatusKill&) -> int { return CLD_KILLED; },
                                 [](const ExitStatusCoreDump&) -> int { return CLD_DUMPED; },
                                 [](const ExitStatusStop&) -> int { return CLD_STOPPED; },
                                 [](const ExitStatusContinue&) -> int { return CLD_CONTINUED; }},
                      exit_status.variant_);
  }

  static int signal_info_status(const ExitStatus& exit_status) {
    return std::visit(
        overloaded{
            [](const ExitStatusExit& status) -> int { return static_cast<int>(status.code); },
            [](const ExitStatusKill& kill) -> int { return kill.signal_info.signal.number(); },
            [](const ExitStatusCoreDump& core_dump) -> int {
              return core_dump.signal_info.signal.number();
            },
            [](const ExitStatusStop& stop) -> int { return stop.signal_info.signal.number(); },
            [](const ExitStatusContinue& cont) -> int { return cont.signal_info.signal.number(); }},
        exit_status.variant_);
  }

 private:
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  explicit ExitStatus(Variant variant) : variant_(ktl::move(variant)) {}

  Variant variant_;
};

// This enum describes the state that a task or thread group can be in when being stopped.
// The names are taken from ptrace(2).
enum class StopState : uint8_t {
  // In this state, the process has been told to wake up, but has not yet been woken.
  // Individual threads may still be stopped.
  Waking,
  // In this state, at least one thread is awake.
  Awake,
  // Same as the above, but you are not allowed to make further transitions. Used
  // to kill the task / group. These names are not in ptrace(2).
  ForceWaking,
  ForceAwake,

  // In this state, the process has been told to stop via a signal, but has not yet stopped.
  GroupStopping,
  // In this state, at least one thread of the process has stopped
  GroupStopped,
  // In this state, the task has received a signal, and it is being traced, so it will
  // stop at the next opportunity.
  SignalDeliveryStopping,
  // Same as the last one, but has stopped.
  SignalDeliveryStopped,
  // Stop for a ptrace event: a variety of events defined by ptrace and
  // enabled with the use of various ptrace features, such as the
  // PTRACE_O_TRACE_* options. The parameter indicates the type of
  // event. Examples include PTRACE_EVENT_FORK (the event is a fork),
  // PTRACE_EVENT_EXEC (the event is exec), and other similar events.
  PtraceEventStopping,
  // Same as the last one, but has stopped
  PtraceEventStopped,
  // In this state, we have stopped before executing a syscall
  SyscallEnterStopping,
  SyscallEnterStopped,
  // In this state, we have stopped after executing a syscall
  SyscallExitStopping,
  SyscallExitStopped,
};

class StopStateHelper {
 public:
  // This means a stop is either in progress or we've stopped.
  static bool is_stopping_or_stopped(StopState state) {
    return is_stopped(state) || is_stopping(state);
  }

  // This means a stop is in progress. Refers to any stop state ending in "ing".
  static bool is_stopping(StopState state) {
    switch (state) {
      case StopState::GroupStopping:
      case StopState::SignalDeliveryStopping:
      case StopState::PtraceEventStopping:
      case StopState::SyscallEnterStopping:
      case StopState::SyscallExitStopping:
        return true;
      default:
        return false;
    }
  }

  // This means we've stopped. Refers to any stop state ending in "ed".
  static bool is_stopped(StopState state) {
    switch (state) {
      case StopState::GroupStopped:
      case StopState::SignalDeliveryStopped:
      case StopState::PtraceEventStopped:
      case StopState::SyscallEnterStopped:
      case StopState::SyscallExitStopped:
        return true;
      default:
        return false;
    }
  }
};

struct AtomicStopState {
 public:
  AtomicStopState(StopState state) : inner_(static_cast<uint8_t>(state)) {}

  StopState load(ktl::memory_order order) const {
    uint8_t value = inner_.load(order);
    // SAFETY: we only ever store to the atomic a value originating
    // from a valid `StopState`.
    return static_cast<StopState>(value);
  }

  void store(StopState state, ktl::memory_order order) {
    inner_.store(static_cast<uint8_t>(state), order);
  }

 private:
  ktl::atomic<uint8_t> inner_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_EXIT_STATUS_H_
