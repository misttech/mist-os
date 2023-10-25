// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vdso-platform.h"

// Cortex-A73 has an issue where there is a 1 cycle window where an incorrect
// value may be read from the CNTVCT register (Erratum #858921). To workaround
// the issue, ARM suggests that we read the register twice and use the first
// read value if the 32nd bits are different or use the second read value if the
// 32nd bits are the same.
//
// For more details, see Cortex-A73 Erratum #858921 documentation[1].
//
// To keep things simple, we use the same logic for all ARM platforms even if no
// Cortex-A73 cores are used. This is not an issue because we would read the
// same register anyways - we would just be performing an extra (unnecessary)
// read and comparison in the non-Cortex-A73.
//
// [1]: https://developer.arm.com/documentation/epm086451/1200/
//
// This is the same implementation Zircon uses in its VDSO; see the method
// `CODE_ticks_get_arm_a73` in Zircon's VDSO source available at
// `//zircon/kernel/lib/userabi/vdso/zx_ticks_get.cc` (as of writing).
uint64_t get_raw_ticks() {
  uint64_t ticks1, ticks2;
  __asm__ volatile("mrs %0, cntvct_el0" : "=r"(ticks1));
  __asm__ volatile("mrs %0, cntvct_el0" : "=r"(ticks2));
  return (((ticks1 ^ ticks2) >> 32) & 1) ? ticks1 : ticks2;
}
