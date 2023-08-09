// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl write_to_stack
write_to_stack:
  addi a0, zero, 0xaaaabbbbccccdddd
  addi sp, sp, -16
  sd a0, 8(sp)
  ecall
