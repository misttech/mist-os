// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2009 Corey Tabaka
// Copyright (c) 2015 Intel Corporation
// Copyright (c) 2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/code-patches/case-id-asm.h>
#include <arch/defines.h>
#include <arch/kernel_aspace.h>
#include <arch/x86/asm.h>
#include <arch/x86/descriptor.h>
#include <arch/x86/mmu.h>
#include <arch/x86/registers.h>
#include <lib/arch/asm.h>
#include <lib/code-patching/asm.h>
#include <lib/instrumentation/asan.h>
#include <zircon/tls.h>
#include <phys/handoff.h>

// We assume `KERNEL_ASPACE_SIZE` <= 512GB.
.if KERNEL_ASPACE_SIZE > 0x0000008000000000
.err "KERNEL_ASPACE_SIZE must be less than or equal to 512GB"
.endif

// Clobbers %rax, %rdx.
.macro sample_ticks out
  rdtsc
  shl $32, %rdx
  or %rdx, %rax
  mov %rax, \out
.endm

.function _start, global
.label PhysbootHandoff, global
  // As early as possible collect the time stamp.
  sample_ticks %r15

  // Set up the stack.
  lea _kstack_end(%rip), %rsp

  // Save off the handoff pointer in a register that won't get clobbered.
  mov %rdi, %r14

  mov %r15, kernel_entry_ticks(%rip)

  // Load our new GDT before touching the segment registers, as the latter
  // will cause the CPU to verify the GDTR points to valid memory for the
  // GDT.
  lgdt _temp_gdtr(%rip)

  // Load our new CS from the newly-loaded GDT with a long return.
  pushq $CODE_64_SELECTOR
  lea .Lreload_cs(%rip), %rax
  pushq %rax
  lretq
.Lreload_cs:

  // We modify the currently live address space below.
  mov $X86_CR3_BASE_MASK, %rax
  mov %cr3, %rcx
  andq %rax, %rcx  // %rcx now holds the root page table address.

  // Pick out the table pointed to by the last entry, covering [-512GiB, 0);
  // save it for reference later both outside of this and below to link in
  // the following table.
  add $(NO_OF_PT_ENTRIES - 1) * 8, %rcx
  mov (%rcx), %rcx
  mov $X86_PT_BASE_ADDRESS_MASK, %rax
  and %rax, %rcx
  mov %rcx, upper_512gib_page_table_phys(%rip)

#if __has_feature(address_sanitizer)
  // After these instructions, virtual addresses may be translated to physical
  // ones by subtracting %rbx.
  lea __executable_start(%rip), %rbx
  sub PHYS_HANDOFF_KERNEL_PHYSICAL_LOAD_ADDRESS(%r14), %rbx

  // kASAN tracks memory validity with a 'shadow map' starting at a fixed offset. The shadow map
  // tracks the validity of accesses within an eight-byte region with one byte - zero means that
  // all bytes are valid, non-zero tracks either fine-grained validity or various invalid states.
  //
  // At boot, start with a shadow map of all zeros, allowing every access. Efficiently encode the
  // zeroed shadow map by using a single page of zeros and pointing all kASAN page tables at it.
  //
  // The shadow map covers 512 GB of kernel address space which is the current virtual address
  // space of the kernel. This requires 64 GB of kernel virtual address space, which requires
  // 64 PDP entries.
  // TODO(https://fxbug.dev/42104852): Unmap the shadow's shadow, the region of shadow memory covering the
  // shadow map. This should never be accessed.
  // Make the kasan Page Tables point to the zero page
  movl $NO_OF_PT_ENTRIES, %ecx
  lea kasan_shadow_pt(%rip), %rdi
  lea kasan_zero_page(%rip), %rax
  sub %rbx, %rax  // Translate to physical address
  or $X86_KERNEL_KASAN_INITIAL_PT_FLAGS, %rax
  rep stosq

  // Make the Page Directory point to the Page Table
  movl $NO_OF_PT_ENTRIES, %ecx
  lea kasan_shadow_pd(%rip), %rdi
  lea kasan_shadow_pt(%rip), %rax
  sub %rbx, %rax  // Translate to physical address
  or $X86_KERNEL_KASAN_INITIAL_PD_FLAGS, %rax
  rep stosq

  // Put the page directory entry into the upper 512 GiB table. It's 64 entries starting from
  // the index corresponding to the KASAN_SHADOW_OFFSET virtual address.
  // 64 pdp entries span 64GB of shadow map, covering 512 GB of kernel address space
#define PDP_HIGH_SHADOW_OFFSET (((KASAN_SHADOW_OFFSET) >> 30) & 0x1ff)
  mov upper_512gib_page_table_phys(%rip), %rdi
  add $PDP_HIGH_SHADOW_OFFSET * 8, %rdi
  movl $X86_KERNEL_KASAN_PDP_ENTRIES, %ecx
  lea kasan_shadow_pd(%rip), %rax
  sub %rbx, %rax  // Translate to physical address
  or $X86_KERNEL_KASAN_INITIAL_PD_FLAGS, %rax
  rep stosq
#endif  // __has_feature(address_sanitizer)

  // Set %gs.base to &bp_percpu.  It's statically initialized
  // with kernel_unsafe_sp set, so after this it's safe to call
  // into C code that might use safe-stack and/or stack-protector.
  lea bp_percpu(%rip), %rax
  mov %rax, %rdx
  shr $32, %rdx
  mov $X86_MSR_IA32_GS_BASE, %ecx
  wrmsr

  // Set up the idt
  lea _idt_startup(%rip), %rdi
  call idt_setup
  call load_startup_idt

  // Initialize CPUID value cache - and do so before functions (like
  // x86_init_percpu) begin to access CPUID data.
  call InitializeBootCpuid

  // Assign this core CPU# 0 and initialize its per cpu state
  xor %edi, %edi
  call x86_init_percpu

  // Collect the time stamp of entering "normal" C++ code in virtual space.
  sample_ticks kernel_virtual_entry_ticks(%rip)

  // Call the main module
  mov %r14, %rdi
  call lk_main

0:                          /* just sit around waiting for interrupts */
  hlt                     /* interrupts will unhalt the processor */
  pause
  jmp 0b                  /* so jump back to halt to conserve power */
.end_function

.object _kstack, bss, global, align=16
  .skip 8192
_kstack_end:
.end_object

// This symbol is used by gdb python to know the base of the kernel module
.label KERNEL_BASE_ADDRESS, global, value=KERNEL_BASE
