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
};

class SignalState {};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_TYPES_H_
