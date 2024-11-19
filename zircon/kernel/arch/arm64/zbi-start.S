// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/arm64/asm.h>
#include <lib/arch/asm.h>

// These must match up with start.S register assignments.
tmp .req x9
tmp2 .req x10

.text
FUNCTION(ImageFixup)
    // The fixup code appears right after the kernel image (at __data_end in
    // our view).  Note this code overlaps with the kernel's bss!  It
    // expects x0 to contain the actual runtime address of __executable_start.
    bl      __data_end

    /* clear out the kernel's bss using current physical location */
    /* NOTE: Relies on __bss_start and _end being 16 byte aligned */
.Ldo_bss:
    adr_global tmp, __bss_start
    adr_global tmp2, _end
    sub     tmp2, tmp2, tmp
    cbz     tmp2, .Lbss_loop_done
.Lbss_loop:
    sub     tmp2, tmp2, #16
    stp     xzr, xzr, [tmp], #16
    cbnz    tmp2, .Lbss_loop
.Lbss_loop_done:

    b AfterImageFixup
END_FUNCTION(ImageFixup)
