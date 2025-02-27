// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/arm64/asm.h>
#include <arch/arm64/mmu.h>
#include <arch/arm64.h>
#include <lib/arch/asm.h>
#include <arch/code-patches/case-id-asm.h>
#include <arch/defines.h>
#include <arch/kernel_aspace.h>
#include <lib/code-patching/asm.h>
#include <zircon/tls.h>

#ifndef __has_feature
#define __has_feature(x) 0
#endif

//
// Register use:
//   x0-x3   Arguments
//   x9-x14  Scratch
//   x21-x28 Globals
//
tmp                     .req x9
tmp2                    .req x10
tmp3                    .req x11
tmp4                    .req x12
tmp5                    .req x13

kernel_vaddr            .req x21
handoff_paddr           .req x22

// Collect timestamp in tmp, tmp2.  Also clobbers tmp3-5.
.macro sample_ticks
  mrs tmp, cntpct_el0
  mrs tmp2, cntvct_el0

  // Workaround for Cortex-A73 erratum 858921.
  // See kernel/dev/timer/arm_generic/arm_generic_timer.cc::read_cntpct_a73.
  mrs tmp3, cntpct_el0
  mrs tmp4, cntvct_el0
  eor tmp5, tmp, tmp3
  tst tmp5, #(1 << 32)
  csel tmp, tmp, tmp3, eq
  eor tmp5, tmp2, tmp4
  tst tmp5, #(1 << 32)
  csel tmp2, tmp2, tmp4, eq
.endm

// Store sample_ticks results in a uint64_t[2] location.
// Clobbers tmp3.
.macro store_ticks symbol
  // There is no reloc like :lo12: that works for stp's scaled immediate,
  // so the add after the adrp can't be folded into the store like with str.
  adr_global tmp3, \symbol
  stp tmp, tmp2, [tmp3]
.endm

// This code is purely position-independent and generates no relocations
// that need boot-time fixup.
.function _start, global
.label PhysbootHandoff, global
  // As early as possible collect the time stamp.
  sample_ticks

  // Record the entry time stamp.
  store_ticks kernel_entry_ticks

  // Clear any phys exception handlers.
  msr vbar_el1, xzr

  // Save the x0 argument in a register that won't be clobbered.
  mov handoff_paddr, x0

  // This can be any arbitrary (page-aligned) address >= KERNEL_ASPACE_BASE.
  // TODO(https://fxbug.dev/42098994): Choose it randomly.
  ldr_global kernel_vaddr, kernel_relocated_base

  // Set up a functional stack pointer
  adr_global tmp, .Lboot_cpu_kstack_end
  mov sp, tmp

  // Call into the arm64 boot mapping code to give it a chance to initialize its page tables
  adr_global x0, __executable_start
  sub x0, kernel_vaddr, x0 // compute the delta between virtual and physical addresses
  bl arm64_boot_map_init

.Lmmu_on_pc:
  isb

  // Map our current physical PC to the virtual PC and jump there.
  // PC = next_PC - __executable_start + kernel_vaddr
  adr tmp, .Lmmu_on_vaddr
  adr_global tmp2, __executable_start
  sub tmp, tmp, tmp2
  add tmp, tmp, kernel_vaddr
  br tmp

.Lmmu_on_vaddr:
  // Disable trampoline page-table in ttbr0
  movlit tmp, MMU_TCR_FLAGS_KERNEL
  msr tcr_el1, tmp
  isb

  // Invalidate the entire TLB
  tlbi vmalle1
  dsb sy
  isb

  // set up the boot stack for real
  adr_global tmp, .Lboot_cpu_kstack_end
  mov sp, tmp

  // Set the thread pointer early so compiler-generated references
  // to the stack-guard and unsafe-sp slots work.  This is not a
  // real 'struct thread' yet, just a pointer to (past, actually)
  // the two slots used by the ABI known to the compiler.  This avoids
  // having to compile-time disable safe-stack and stack-protector
  // code generation features for all the C code in the bootstrap
  // path, which (unlike on x86, e.g.) is enough to get annoying.
  adr_global tmp, .Lboot_cpu_fake_thread_pointer_location
  msr tpidr_el1, tmp
#if __has_feature(shadow_call_stack)
  // The shadow call stack grows up.
  adr_global shadow_call_sp, boot_cpu_shadow_call_kstack
#endif

  // set the per cpu pointer for cpu 0
  adr_global percpu_ptr, arm64_percpu_array

  // Choose a good (ideally random) stack-guard value as early as possible.
  bl choose_stack_guard
  mrs tmp, tpidr_el1
  str x0, [tmp, #ZX_TLS_STACK_GUARD_OFFSET]
  // Don't leak the value to other code.
  mov x0, xzr

  // Collect the time stamp of entering "normal" C++ code in virtual space.
  sample_ticks
  store_ticks kernel_virtual_entry_ticks

  mov x0, handoff_paddr
  bl lk_main
  b .
.end_function

.object boot_cpu_fake_arch_thread, bss, local, align=8
  .quad 0 // Location of stack guard
#if __has_feature(safe_stack)
  .quad .Lboot_cpu_unsafe_kstack_end
#else
  .quad 0
#endif
.Lboot_cpu_fake_thread_pointer_location:
.end_object

.object boot_cpu_kstack, bss, local
  .skip ARCH_DEFAULT_STACK_SIZE
  .balign 16
.Lboot_cpu_kstack_end:
.end_object

#if __has_feature(safe_stack)
.object boot_cpu_unsafe_kstack, bss, local
  .skip ARCH_DEFAULT_STACK_SIZE
  .balign 16
.Lboot_cpu_unsafe_kstack_end:
.end_object
#endif

#if __has_feature(shadow_call_stack)
.object boot_cpu_shadow_call_kstack, bss, local, align=8
  .skip PAGE_SIZE
.end_object
#endif

// This symbol is used by gdb python to know the base of the kernel module
.label KERNEL_BASE_ADDRESS, global, value=KERNEL_BASE
