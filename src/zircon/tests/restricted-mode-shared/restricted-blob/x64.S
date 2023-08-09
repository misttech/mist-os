// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl write_to_stack
write_to_stack:
  movabsq $0xaaaabbbbccccdddd, %r8
  push %r8
  syscall

