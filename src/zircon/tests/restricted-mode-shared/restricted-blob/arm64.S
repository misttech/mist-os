// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl write_to_stack
write_to_stack:
  movz x0, #0xdddd
  movk x0, #0xcccc, lsl 16
  movk x0, #0xbbbb, lsl 32
  movk x0, #0xaaaa, lsl 48
  add sp, sp, #-16
  str x0, [sp, #8]
  svc     #0
