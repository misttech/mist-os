// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/asm.h>
#include <lib/thread-stack/abi.h>

// We avoid the `la` pseudoinstruction while doing something equivalent
// so that we can avoid generating a .got relocation and thus can assert
// that there should never be any use of .got and ensure compiler-generated
// and other unintended uses don't creep in.  (`lla` computes the address
// of the symbol using PC-relative relocations, while `la` under __PIC__
// does a PC-relative load from the symbol's GOT slot.)

.function _start, global
.label PhysbootHandoff, global

  // collect the starting time stamp
  rdtime a2

  // Save a0 (physboot handoff paddr) in a called-saved register for later
  // passing to lk_main after other calls below.  s0 is fp, so start with s1.
  mv      s1, a0

  // save the time stamp we recorded earlier
  lla     t0, kernel_entry_ticks
  sd      a2, (t0)

  // Run the boot cpu init routine with the boot hart id.
  // This will do basic initialization of the cpu such as initializing
  // the main control registers and loading the exception vector table.
  // Also loads the per cpu register.
  mv      a0, s1
  call    riscv64_boot_cpu_init

  // save the time stamp just before entering C
  rdtime  a0
  lla     a1, kernel_virtual_entry_ticks
  sd      a0, (a1)

  // Recover the PhysHandoff pointer.
  mv      a0, s1

  // Call main
  call    lk_main

  // should never return here
0:
  wfi
  j       0b
.end_function

// This symbol is used by gdb python to know the base of the kernel module
.label KERNEL_BASE_ADDRESS, global, value=KERNEL_BASE
