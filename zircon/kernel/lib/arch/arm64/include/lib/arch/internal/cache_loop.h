// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_INTERNAL_CACHE_LOOP_H_
#define ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_INTERNAL_CACHE_LOOP_H_

// clang-format off

#ifdef __ASSEMBLER__

// Export a routine to iterate over all ways/sets across all levels of data
// caches from level 0 to the point of coherence.
//
// Adapted from example code in the ARM Architecture Reference Manual ARMv8.
.macro data_cache_way_set_op_impl, op name
    mrs     x0, clidr_el1           // x0 = clidr_el1

    and     w3, w0, #0x07000000     // w3 = level of coherence from clidr
    lsr     w3, w3, #23             // multiply by 2

    cbz     w3, .Lfinished_\name    // LoC == 0, exit now

    mov     w10, #0                 // w10 = 2x cache level, start with 0
    mov     w8, #1                  // w8 = constant 1

    // loop per level of cache
.Lloop1_\name:
    add     w2, w10, w10, lsr #1    // w2 = 3x cache level
    lsr     w1, w0, w2              // w1 = 3 bit cache type for this level, extracted from clidr
    and     w1, w1, #0x7

    cmp     w1, #2
    b.lt    .Lskip_\name            // no data or unified cache at this level

    msr     csselr_el1, x10         // select this cache level (w10 already holds cache level << 1)
    isb                             // synchronize change to csselr

    // TODO(https://fxbug.dev/330385318): update this logic to work with extended CCSIDR
    mrs     x1, ccsidr_el1          // w1 = ccsidr
    and     w2, w1, #7              // w2 = log2(line len) - 4
    add     w2, w2, #4              // w2 = log2(line len)

    ubfx    w4, w1, #3, #10         // w4 = max way number, right aligned
    clz     w5, w4                  // w5 = 32 - log2(ways), bit position of way in DC operand
    lsl     w9, w4, w5              // w9 = max way number, aligned to position in DC operand
    lsl     w12, w8, w5             // w12 = amount to decrement way number per iteration

    ubfx    w6, w1, #13, #15        // w6 = max set number, right aligned
    lsl     w6, w6, w2              // w6 = max set number, aligned to position in DC operand
    lsl     w13, w8, w2             // w13 = amount to decrement set number per iteration

    // loop by way
.Lloop2_\name:
    mov     w7, w6                  // w7 = reload max set number from w6

    //  loop by set
.Lloop3_\name:
    orr     w11, w10, w9            // w11 = combine way number and cache number
    orr     w11, w11, w7            //       and set number for DC operand
    dc      \op, x11                // data cache op
    subs    w7, w7, w13             // decrement set number
    b.ge    .Lloop3_\name           // repeat until set < 0

    subs    x9, x9, x12             // decrement way number
    b.ge    .Lloop2_\name           // repeat until way < 0

    dsb     sy                      // ensure completion of previous cache maintenance instructions

.Lskip_\name:
    add     w10, w10, #2            // increment 2x cache level
    cmp     w3, w10                 // loop while we haven't reached level of coherence
    b.gt    .Lloop1_\name

.Lfinished_\name:
.endm

#endif


#endif  // ZIRCON_KERNEL_LIB_ARCH_ARM64_INCLUDE_LIB_ARCH_INTERNAL_CACHE_LOOP_H_
