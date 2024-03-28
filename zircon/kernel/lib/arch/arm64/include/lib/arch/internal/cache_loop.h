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
// Updated to handle FEAT_CCIDX.
.macro data_cache_way_set_op_impl, op name
    mrs     x0, clidr_el1           // x0 = clidr_el1

    ubfm    w3, w0, #24, #26        // w3 = level of coherence from clidr[24:26]
    lsl     w3, w3, #1              // w3 = level of coherence * 2

    cbz     w3, .Lfinished_\name    // LoC == 0, exit now

    // test for the presence of the FEAT_CCIDX: id_aa64mmfr2_el1[23:20]
    mrs     x11, id_aa64mmfr2_el1
    ubfm    w11, w11, #20, #23      // w11 = >0 FEAT_CCIDX, 0 legacy ccsidr_el1 format

    mov     w10, #0                 // w10 = 2x cache level, start with 0
    mov     w8, #1                  // w8 = constant 1

    // loop per level of cache
.Lloop1_\name:
    add     w2, w10, w10, lsr #1    // w2 = 3x cache level (w3 + w3 / 2)
    lsr     w1, w0, w2              // w1 = 3 bit cache type for this level, extracted from clidr
    and     w1, w1, #0b111

    cmp     w1, #2
    b.lt    .Lskip_\name            // no data or unified cache at this level (< type 2)

    msr     csselr_el1, x10         // select this cache level (w10 already holds cache level << 1)
    isb                             // synchronize change to csselr

    // parse ccsidr
    mrs     x1, ccsidr_el1          // x1 = ccsidr

    // line len [3..0] - 4 in both 32 and 64bit ccsidr
    and     w2, w1, #0b111          // w2 = log2(line len) - 4
    add     w2, w2, #4              // w2 = log2(line len)

    // based on FEAT_CCIDX parse the next two fields differently
    cbnz    w11, 0f

    // 32bit ccsidr: associativity [12:3] sets [27:13]
    ubfm    w4, w1, #3, #12         // w4 = max way number, right aligned
    ubfm    w6, w1, #13, #27        // w6 = max set number, right aligned
    b       1f

0:
    // 64bit ccsidr: associativity [23:3] sets [55:32]
    ubfm    w4, w1, #3, #23         // w4 = max way number, right aligned
    ubfm    x6, x1, #32, #55        // w6 = max set number, right aligned

1:
    clz     w5, w4                  // w5 = 32 - log2(ways), bit position of way in DC operand
    lsl     w9, w4, w5              // w9 = max way number, aligned to position in DC operand
    lsl     w12, w8, w5             // w12 = amount to decrement way number per iteration

    lsl     w6, w6, w2              // w6 = max set number, aligned to position in DC operand
    lsl     w13, w8, w2             // w13 = amount to decrement set number per iteration

    // loop by way
.Lloop2_\name:
    mov     w7, w6                  // w7 = reload max set number from w6

    //  loop by set
.Lloop3_\name:
    orr     w1, w10, w9             // w1 = combine way number and cache number
    orr     w1, w1, w7              //      and set number for DC operand
    dc      \op, x1                 // data cache op
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
