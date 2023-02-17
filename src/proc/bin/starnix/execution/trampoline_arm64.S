// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

.globl restricted_enter
restricted_enter:
  // TODO implement and test this.
  //
  // This function takes two arguments, the options value (in r0) and the vector_table (in r1) to
  // pass to the zx_restricted_enter() syscall. These are untouched as they are also the first two
  // parameters to zx_restricted_enter().
  //
  // This function should (see the x86 version):
  //   - Save the callee saved registers (including the link register) on the stack.
  //   - Pass the stack pointer as the third ("context") parameter (in r2) to zx_restricted_enter().
  //   - Call zx_restricted_enter().
  //   - Return any failures (this will not return at all on success).

  // So it's obvious this is not implemented, issue a debug break.
  brk 0

.globl restricted_return
restricted_return:
  // TODO implement and test this.
  //
  // This function should do the reverse of restricted_enter:
  //   - Restore the stack pointer which will be passed in as the first parameter (r0).
  //   - Pop the callee saved registers we pushed in restricted_enter() above.
  //   - Return to the link register. This will be the one passed in to restricted_enter so will
  //     have the effect of returning from that call.

  // So it's obvious this is not implemented, issue a debug break.
  brk 0
