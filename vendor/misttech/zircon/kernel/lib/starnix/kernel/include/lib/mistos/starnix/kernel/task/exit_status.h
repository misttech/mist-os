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

// Variant representing ExitStatus
using ExitStatus = ktl::variant<ExitStatusExit, ExitStatusKill, ExitStatusCoreDump, ExitStatusStop,
                                ExitStatusContinue>;

class ExitStatusHelper {
 public:
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
        exit_status);
  }

  static int signal_info_code(const ExitStatus& exit_status) {
    return std::visit(overloaded{[](const ExitStatusExit&) -> int { return CLD_EXITED; },
                                 [](const ExitStatusKill&) -> int { return CLD_KILLED; },
                                 [](const ExitStatusCoreDump&) -> int { return CLD_DUMPED; },
                                 [](const ExitStatusStop&) -> int { return CLD_STOPPED; },
                                 [](const ExitStatusContinue&) -> int { return CLD_CONTINUED; }},
                      exit_status);
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
        exit_status);
  }

 private:
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_EXIT_STATUS_H_
