// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl restricted_enter
restricted_enter:
  // TODO(fxbug.dev/128554): implement riscv support.
  ret

.globl restricted_return
restricted_return:
  // TODO(fxbug.dev/128554): implement riscv support.
  ret

.globl breakpoint_for_module_changes
breakpoint_for_module_changes:
  ebreak
  ret
