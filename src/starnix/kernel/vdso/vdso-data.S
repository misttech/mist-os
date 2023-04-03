// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vdso-constants.h"

// Reserve space for the data constants used by vDSO. The actual values
// will be filled in at boot time by Starnix.

// This is in assembly so that the LTO compiler cannot see the
// initializer values and decide it's OK to optimize away references.

.section .rodata.vdso_constants,"a",%progbits
    .balign VDSO_CONSTANTS_ALIGN
    .global DATA_CONSTANTS
    .hidden DATA_CONSTANTS
    .type DATA_CONSTANTS, %object
    .size DATA_CONSTANTS, VDSO_CONSTANTS_SIZE
DATA_CONSTANTS:
    .fill VDSO_CONSTANTS_SIZE / 4, 4, 0xdeadbeef