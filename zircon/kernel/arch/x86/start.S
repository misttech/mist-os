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
#include <lib/code-patching/asm.h>
#include <lib/instrumentation/asan.h>
#include <zircon/tls.h>

#define ADDR_OFFSET_MASK ((1 << ADDR_OFFSET)-1)
#define SHIFT_OFFSET(_s) ((_s) >> 3)
#define SHIFT_REMAIN(_s) ((_s) - (SHIFT_OFFSET(_s) << 3))

// Calculate the offset into `pdp_high` based on `KERNEL_ASPACE_SIZE`.
#define PDP_HIGH_OFFSET (512 - (KERNEL_ASPACE_SIZE >> 30))
// We assume `KERNEL_ASPACE_SIZE` <= 512GB.
.if KERNEL_ASPACE_SIZE > 0x0000008000000000
.err "KERNEL_ASPACE_SIZE must be less than or equal to 512GB"
.endif

// Set a page table entry for the kernel module relocated 64-bit virtual
// address in 32-bit code. Clobbers the %ecx register.
.macro set_relocated_page_table_entry table_reg, shift, value
    // Extract 32-bit chunk of kernel_relocated_base containing the index bits
    // for this page level shift.
    mov (kernel_relocated_base + SHIFT_OFFSET(\shift))(%rip), %rcx

    // Get the exact portion of the 32-bit value that is the index
    shrl $SHIFT_REMAIN(\shift), %ecx
    andl $ADDR_OFFSET_MASK, %ecx

    // Get the address on the page table of index * 8 and set the value
    shll $3, %ecx
    addl \table_reg, %ecx
    movl \value, (%ecx)
.endm

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
    mov %rsi, %r14

    // The fixup code in image.S runs in 64-bit mode with paging enabled,
    // so we can't run it too early.  But it overlaps the bss, so we move
    // it before zeroing the bss.  We can't delay zeroing the bss because
    // the page tables we're about to set up are themselves in bss.

    // The first word after the kernel image (at __data_end in our view)
    // gives the size of the following code.  Copy it to _end.
    mov __data_end(%rip), %ecx
    lea (__data_end + 4)(%rip), %esi
    lea _end(%rip), %edi

    // We are copying the relocation code right after the bss.
    // It is possible that the code is larger than the bss, so we have to
    // take into account the possibility of src and dst overlapping.
    mov %edi, %eax
    sub %esi, %eax
    cmp %ecx, %eax
    jnb .Lcopy
    // There's an overlap. Reverse the copy direction.
    lea -1(%edi, %ecx), %edi // edi = edi + ecx - 1;
    lea -1(%esi, %ecx), %esi // esi = esi + ecx - 1;
    std
.Lcopy:
    rep movsb
    cld

    // Now it's safe to zero the bss.
    lea __bss_start(%rip), %edi
    lea _end(%rip), %ecx
    sub %edi, %ecx              // Compute the length of the bss in bytes.
    xor %eax, %eax
    rep stosb // while (ecx-- > 0) *edi++ = al;

    // These variables are in the .bss, so now it's safe to set them.
    mov %r15, kernel_entry_ticks(%rip)
    lea __executable_start(%rip), %rax
    mov %rax, kernel_base_phys(%rip)

    // We modify the currently live address space below. Store the address of
    // the root page table in %rdx.
    mov $X86_CR3_BASE_MASK, %rax
    mov %cr3, %rdx
    andq %rax, %rdx

    // Link the table called `pdp_high` as the last entry in the root, which
    // covers [-512GiB, 0). %rdx still holds the address of the root table.
    lea pdp_high(%rip), %rax
    orl  $X86_KERNEL_PD_FLAGS, %eax
    set_relocated_page_table_entry %edx, PML4_SHIFT, %eax

    // We want to link the table that identity maps the first 1GiB to the
    // fourth-last entry pdp_high, effecting a mapping of [-4GiB, -3GiB]
    // virtual also to [0, 1GiB] physical.
    //
    // We first pick out the table mapping the first 1GiB by descending from
    // the root table, picking out the first entries.
    //
    // Note that %rdx still holds the address of the root table.

    mov (%rdx), %rax
    movabs $X86_PT_BASE_ADDRESS_MASK, %rbx
    and %rbx, %rax

    // %rax now holds the address of the table covering [0, 512GiB); descend
    // down the first entry again. %rbx still holds the address mask for a
    // table entry.
    mov (%rax), %rax
    and %rbx, %rax

    // %rax now holds the address of the table covering [0, 1GiB) and we make
    // the [-4GiB, -3GiB) -> [0, 1GiB) mapping.
    orl  $X86_KERNEL_PD_FLAGS, %eax
    lea pdp_high(%rip), %rbx
    set_relocated_page_table_entry %ebx, PDP_SHIFT, %eax

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

    /* point the high pdp at our linear mapping page tables */
    lea (pdp_high + PDP_HIGH_OFFSET * 8)(%rip), %rsi
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

    // Put the page directory entry into the pdp_high. It's 64 entries starting from
    // the index corresponding to the KASAN_SHADOW_OFFSET virtual address.
    // 64 pdp entries span 64GB of shadow map, covering 512 GB of kernel address space
#define PDP_HIGH_SHADOW_OFFSET (((KASAN_SHADOW_OFFSET) >> 30) & 0x1ff)
    lea (pdp_high + PDP_HIGH_SHADOW_OFFSET * 8)(%rip), %rdi
    movl $X86_KERNEL_KASAN_PDP_ENTRIES, %ecx
    lea kasan_shadow_pd(%rip), %rax
    or $X86_KERNEL_KASAN_INITIAL_PD_FLAGS, %rax
    rep stosq
#endif  // __has_feature(address_sanitizer)

    // Long jump to our code selector and the high address relocated.
    lea .Lhigh_entry(%rip), %rax
    addq kernel_relocated_base(%rip), %rax
    jmp *%rax

// This code runs at the final virtual address, so it should be pure PIC.
.Lhigh_entry:
    // load the high kernel stack
    lea _kstack_end(%rip), %rsp

    // move_fixups_and_zero_bss copied the fixup code to _end.
    // It expects %rdi to contain the actual runtime address of __executable_start.
    lea __executable_start(%rip), %rdi
    call _end
    // The fixup code won't be used again, so the memory can be reused now.

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

// These symbols are used by image.S
.global IMAGE_ELF_ENTRY
IMAGE_ELF_ENTRY = _start

// This symbol is used by gdb python to know the base of the kernel module
.global KERNEL_BASE_ADDRESS
KERNEL_BASE_ADDRESS = KERNEL_BASE - KERNEL_LOAD_OFFSET
