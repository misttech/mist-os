// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/riscv64/mmu.h>
#include <arch/riscv64.h>
#include <arch/defines.h>
#include <lib/arch/asm.h>

// We avoid the `la` pseudoinstruction while doing something equivalent
// so that we can avoid generating a .got relocation and thus can assert
// that there should never be any use of .got and ensure compiler-generated
// and other unintended uses don't creep in.  (`lla` computes the address
// of the symbol using PC-relative relocations, while `la` under __PIC__
// does a PC-relative load from the symbol's GOT slot.)

// Entry point for secondary cpus when started from SBI
//
// This code is entered at its physical address.  However, relocatable data
// (including RELRO "constants") have already been relocated by physboot to
// their final virtual addresses.  This code takes care to manage the
// transition of physical to virtual precisely.
.function riscv64_secondary_start, global
  // Enable the MMU with the ASID 0, prefilled by _start
  ld      t0, riscv64_kernel_bootstrap_translation_table_phys
  srli    t1, t0, PAGE_SIZE_SHIFT
  li      t2, (RISCV64_SATP_MODE_SV39 << RISCV64_SATP_MODE_SHIFT)
  or      t1, t1, t2
  csrw    satp, t1

  // global tlb fence
  sfence.vma  zero, zero

  // Compute the relocation offset
  lla     t1, virtual_executable_start
  lla     t0, __executable_start
  ld      t1, (t1)
  sub     t0, t1, t0

  // Jump to high memory
  lla     t1, .Lmmu_on_vaddr_secondary
  add     t1, t1, t0
  jr      t1

.Lmmu_on_vaddr_secondary:
  // SBI is kind enough to give us a private parameter in a1, we fill it with
  // the stack pointer for this hart
  mv      sp, a1

  // Read  the cpu number off of the stack
  ld      a1, -8(sp)

  // Set up the shadow call stack
#if __has_feature(shadow_call_stack)
  ld      shadow_call_sp, -16(sp)
#endif

  // The identity mapping is still there, we can jump to C.
  // The hart id is already in a0.
  call    riscv64_secondary_entry

  // should never return here
0:
  wfi
  j       0b
.end_function

.object virtual_executable_start, relro, local
  .quad __executable_start
.end_object
