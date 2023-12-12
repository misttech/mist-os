// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// On aarch64 ldar provides load with acquire semantics

.globl atomic_load_u32_acquire
.globl atomic_load_u32_acquire_end

atomic_load_u32_acquire:
    ldar w0, [x0]
    ret
atomic_load_u32_acquire_end:
    brk  #0x1
