// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>

static __attribute__((used)) uint64_t udiv64(uint64_t dividend, uint64_t divisor,
                                             uint64_t* remainder) asm("udiv64");

static uint64_t udiv64(uint64_t dividend, uint64_t divisor, uint64_t* remainder) {
  uint64_t quotient = 0;
  uint32_t count = 1;

  // Shortcut special cases
  if (divisor == 0) {
    // div-by-0.
    return UINT64_MAX;
  }
  if (divisor > dividend) {
    *remainder = dividend;
    return 0;
  }
  if (divisor == dividend) {
    *remainder = 0;
    return 1;
  }

  // If not, we want to move the divisor as far left as we can,
  // and then compare against an accumulator of the dividend's left
  // bits. If the accumulator is larger, then we subtract it out, set
  // the quotient bit and keep going.  The quotient bit can be set
  // and shifted because it can't be larger than the divisor was.
  *remainder = 0;

  // Find the first bit in the divisor and shift it left,
  // so we can test at each ste.
  while ((divisor >> 63) == 0) {
    count++;
    divisor <<= 1;
  }
  *remainder = dividend;
  while (count) {
    quotient <<= 1;  // shift here so our last bit is available.
    if (*remainder >= divisor) {
      quotient |= 1;
      *remainder -= divisor;
    }
    count -= 1;
    divisor >>= 1;
  }
  return quotient;
}

extern "C" __attribute__((naked)) void __aeabi_uldivmod() {
  asm("push { r11, lr}");
  asm("sub sp, sp, #16");
  asm("add r12, sp, #8");
  asm("str r12, [sp]");
  asm("bl udiv64");
  asm("ldr r2, [sp, #8]");
  asm("ldr r3, [sp, #12]");
  asm("add sp, sp, #16");
  asm("pop {r11, lr}");
  asm("bx lr");
}
