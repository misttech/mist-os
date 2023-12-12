// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// On aarch64, stlr provides store with release semantics.

.globl atomic_store_u32_release
.globl atomic_store_u32_release_end

atomic_store_u32_release:
    mov x8, x0
    mov w0, wzr
    stlr w1, [x8]
    ret
atomic_store_u32_release_end:
    brk  #0x1
