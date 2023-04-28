// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_REBOOT_H_
#define LIB_ZBI_FORMAT_REBOOT_H_

#include <stdint.h>

// ZBI_TYPE_HW_REBOOT_REASON payload.
typedef uint32_t zbi_hw_reboot_reason_t;

#define ZBI_HW_REBOOT_REASON_UNDEFINED ((zbi_hw_reboot_reason_t)0)
#define ZBI_HW_REBOOT_REASON_COLD ((zbi_hw_reboot_reason_t)1)
#define ZBI_HW_REBOOT_REASON_WARM ((zbi_hw_reboot_reason_t)2)
#define ZBI_HW_REBOOT_REASON_BROWNOUT ((zbi_hw_reboot_reason_t)3)
#define ZBI_HW_REBOOT_REASON_WATCHDOG ((zbi_hw_reboot_reason_t)4)

#endif  // LIB_ZBI_FORMAT_REBOOT_H_
