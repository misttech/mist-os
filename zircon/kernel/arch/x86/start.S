// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2009 Corey Tabaka
// Copyright (c) 2015 Intel Corporation
// Copyright (c) 2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <asm.h>
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

// This section name is known specially to kernel.ld and gen-kaslr-fixups.sh.
// This code has relocations for absolute physical addresses, which do not get
// adjusted by the boot-time fixups (which this code calls at the end).
.section .text.boot, "ax", @progbits
.align 8
FUNCTION_LABEL(_start)
.label PhysbootHandoff, global
    // As early as possible collect the time stamp.
    sample_ticks %r15

    // This serves as a verification that code-patching was performed before
    // the kernel was booted; if unpatched, we would trap here and halt.
    .code_patching.start CASE_ID_SELF_TEST
    ud2  // Same as __builtin_trap()
    .code_patching.end

    /* set up a temporary stack pointer */
    lea _kstack_end(%rip), %rsp

    // Save off the handoff pointer in a register that won't get clobbered.
    mov %rdi, %r14

    mov %r15, kernel_entry_ticks(%rip)

    lea __executable_start(%rip), %rax
    mov %rax, kernel_base_phys(%rip)

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

    /* set up a linear map of the first 64GB */
    lea linear_map_pdp(%rip), %rsi
    movl $32768, %ecx
    xor  %eax, %eax

    /* loop across these page tables, incrementing the address by 2MB */
0:
    mov  %eax, %ebx
    shll $21, %ebx
    orl  $X86_KERNEL_PD_LP_FLAGS, %ebx    // lower word of the entry
    movl %ebx, (%esi)
    mov  %eax, %ebx
    shrl $11, %ebx      // upper word of the entry
    movl %ebx, 4(%esi)
    addl $8,%esi
    inc  %eax
    loop 0b

    // Point the high pdp at our linear mapping page tables
    mov upper_512gib_page_table_phys(%rip), %rsi
    movl $64, %ecx
    lea linear_map_pdp(%rip), %eax
    orl  $X86_KERNEL_PD_FLAGS, %eax

0:
    movl %eax, (%esi)
    add  $8, %esi
    addl $4096, %eax
    loop 0b

#if __has_feature(address_sanitizer)
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
    or $X86_KERNEL_KASAN_INITIAL_PT_FLAGS, %rax
    rep stosq

    // Make the Page Directory point to the Page Table
    movl $NO_OF_PT_ENTRIES, %ecx
    lea kasan_shadow_pd(%rip), %rdi
    lea kasan_shadow_pt(%rip), %rax
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
    or $X86_KERNEL_KASAN_INITIAL_PD_FLAGS, %rax
    rep stosq
#endif  // __has_feature(address_sanitizer)

    // Long jump to our code selector and the high address relocated.
    jmp *.Lhigh_entry_addr(%rip)

.pushsection .data.rel.ro, "aw", %progbits
.Lhigh_entry_addr:
    .quad .Lhigh_entry
.popsection

// This code runs at the final virtual address, so it should be pure PIC.
.Lhigh_entry:
    // load the high kernel stack
    lea _kstack_end(%rip), %rsp

    // Load our new GDT before touching the segment registers, as the latter
    // will cause the CPU to verify the GDTR points to valid memory for the
    // GDT.
    lgdt _temp_gdtr(%rip)

    // Now zero the non-CS segment registers: DS, ES, and SS are vestigial; FS
    // is unused in the kernel; GS will be reset below.
    xor %eax, %eax
    mov %eax, %ds
    mov %eax, %es
    mov %eax, %fs
    mov %eax, %gs
    mov %eax, %ss

    // Load our new CS from the newly-loaded GDT with a long return.
    pushq $CODE_64_SELECTOR
    lea .Lreload_cs(%rip), %rax
    pushq %rax
    lretq
.Lreload_cs:

    // Set %gs.base to &bp_percpu.  It's statically initialized
    // with kernel_unsafe_sp set, so after this it's safe to call
    // into C code that might use safe-stack and/or stack-protector.
    lea bp_percpu(%rip), %rax
    mov %rax, %rdx
    shr $32, %rdx
    mov $X86_MSR_IA32_GS_BASE, %ecx
    wrmsr

    /* set up the idt */
    lea _idt_startup(%rip), %rdi
    call idt_setup
    call load_startup_idt

    // Initialize CPUID value cache - and do so before functions (like
    // x86_init_percpu) begin to access CPUID data.
    call InitializeBootCpuid

    /* assign this core CPU# 0 and initialize its per cpu state */
    xor %edi, %edi
    call x86_init_percpu

    // Fill the stack canary with a random value as early as possible.
    // This isn't done in x86_init_percpu because the hw_rng_get_entropy
    // call would make it eligible for stack-guard checking itself.  But
    // %gs is not set up yet in the prologue of the function, so it would
    // crash if it tried to use the stack-guard.
    call choose_stack_guard

    // Move it into place.
    mov %rax, %gs:ZX_TLS_STACK_GUARD_OFFSET
    // Don't leak that value to other code.
    xor %eax, %eax

    // Collect the time stamp of entering "normal" C++ code in virtual space.
    sample_ticks kernel_virtual_entry_ticks(%rip)

    /* call the main module */
    mov %r14, %rdi
    call lk_main

0:                          /* just sit around waiting for interrupts */
    hlt                     /* interrupts will unhalt the processor */
    pause
    jmp 0b                  /* so jump back to halt to conserve power */

.bss
.align 16
DATA(_kstack)
    .skip 8192
DATA(_kstack_end)

// This symbol is used by gdb python to know the base of the kernel module
.global KERNEL_BASE_ADDRESS
KERNEL_BASE_ADDRESS = KERNEL_BASE
