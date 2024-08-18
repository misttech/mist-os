// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <arch/defines.h>
#include <lib/arch/asm.h>
#include <lib/userabi/vdso-constants.h>

// This is in assembly so that the LTO compiler cannot see the
// initializer values and decide it's OK to optimize away references.

.object DATA_TIME_VALUES, rodata, global, align=PAGE_SIZE
  .fill PAGE_SIZE / 4, 4, 0xdeadbeef
.end_object

.object DATA_CONSTANTS, rodata, global, align=VDSO_CONSTANTS_ALIGN
  .fill VDSO_CONSTANTS_SIZE / 4, 4, 0xdeadbeef
.end_object
