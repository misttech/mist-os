// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_REBOOT_H_
#define LIB_ZBI_FORMAT_REBOOT_H_

#include <stdint.h>

#define ZBI_HW_REBOOT_UNDEFINED ((uint32_t)0)
#define ZBI_HW_REBOOT_COLD ((uint32_t)1)
#define ZBI_HW_REBOOT_WARM ((uint32_t)2)
#define ZBI_HW_REBOOT_BROWNOUT ((uint32_t)3)
#define ZBI_HW_REBOOT_WATCHDOG ((uint32_t)4)

// ZBI_TYPE_HW_REBOOT_REASON payload.
#ifndef __cplusplus
typedef uint32_t zbi_hw_reboot_reason_t;
#else
enum class ZbiHwRebootReason : uint32_t {
  Undefined = ZBI_HW_REBOOT_UNDEFINED,
  Cold = ZBI_HW_REBOOT_COLD,
  Warm = ZBI_HW_REBOOT_WARM,
  Brownout = ZBI_HW_REBOOT_BROWNOUT,
  Watchdog = ZBI_HW_REBOOT_WATCHDOG,
};
using zbi_hw_reboot_reason_t = ZbiHwRebootReason;
#endif  // __cplusplus

#endif  // LIB_ZBI_FORMAT_REBOOT_H_
