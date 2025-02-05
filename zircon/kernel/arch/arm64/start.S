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

/*
 * Register use:
 *  x0-x3   Arguments
 *  x9-x14  Scratch
 *  x21-x28 Globals
 */
tmp                     .req x9
tmp2                    .req x10
wtmp2                   .req w10
tmp3                    .req x11
tmp4                    .req x12
tmp5                    .req x13

cpuid                   .req x21
page_table0             .req x22
page_table1             .req x23
kernel_vaddr            .req x24
handoff_paddr           .req x25

// Collect timestamp in tmp, tmp2.  Also clobbers tmp3-5.
.macro sample_ticks
    mrs     tmp, cntpct_el0
    mrs     tmp2, cntvct_el0

    // Workaround for Cortex-A73 erratum 858921.
    // See kernel/dev/timer/arm_generic/arm_generic_timer.cc::read_cntpct_a73.
    mrs     tmp3, cntpct_el0
    mrs     tmp4, cntvct_el0
    eor     tmp5, tmp, tmp3
    tst     tmp5, #(1 << 32)
    csel    tmp, tmp, tmp3, eq
    eor     tmp5, tmp2, tmp4
    tst     tmp5, #(1 << 32)
    csel    tmp2, tmp2, tmp4, eq
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
.text
FUNCTION(_start)
.label PhysbootHandoff, global
    // As early as possible collect the time stamp.
    sample_ticks

    // This serves as a verification that code-patching was performed before
    // the kernel was booted; if unpatched, we would trap here and halt.
    .code_patching.start CASE_ID_SELF_TEST
    brk #0  // Same as __builtin_trap()
    .code_patching.end

    // Save boot info on the boot CPU alone. Secondaries can jump right to
    // .Lboot_info_ready.
    mrs     cpuid, mpidr_el1
    ubfx    cpuid, cpuid, #0, #15 /* mask Aff0 and Aff1 fields */
    cbnz    cpuid, .Lboot_info_ready

    // Record the entry time stamp.
    store_ticks kernel_entry_ticks

    // Save the x0 argument in a register that won't be clobbered.
    mov     handoff_paddr, x0

    // kernel_entry_paddr: physical address of `_start`.
    adr     tmp, _start
    adr_global tmp2, kernel_entry_paddr
    str     tmp, [tmp2]

    // arch_boot_el: EL at boot time.
    mrs     tmp, CurrentEL
    adr_global tmp2, arch_boot_el
    str     tmp, [tmp2]

    // root_lower_page_table_phys: boot CPU TTBR0_EL1.BADDR.
    mrs     tmp, ttbr0_el1
    and     tmp, tmp, MMU_PTE_OUTPUT_ADDR_MASK
    adr_global tmp2, root_lower_page_table_phys
    str     tmp, [tmp2]

    // root_kernel_page_table_phys: boot CPU TTBR1_EL1.BADDR.
    mrs     tmp, ttbr1_el1
    and     tmp, tmp, MMU_PTE_OUTPUT_ADDR_MASK
    adr_global tmp2, root_kernel_page_table_phys
    str     tmp, [tmp2]

.Lboot_info_ready:
    // If we entered at a higher EL than 1, drop to EL1.
    //
    // Make sure to clear the stack pointer first. The call will reuse in EL1
    // whatever is currently installed, but on a cold boot this might be
    // garbage and run afoul of EL1 sp alignment checks if blindly installed.
    mov     x0, xzr
    mov     sp, x0
    bl      ArmDropToEl1WithoutEl2Monitor

    // Make sure no old exception handlers are installed.
    msr     vbar_el1, xzr

    /* enable caches so atomics and spinlocks work */
    mrs     tmp, sctlr_el1
    and     tmp, tmp, #~(1<<19) /* Clear WXN */
    orr     tmp, tmp, #(1<<12) /* Enable icache */
    orr     tmp, tmp, #(1<<2)  /* Enable dcache/ucache */
    msr     sctlr_el1, tmp

    // This can be any arbitrary (page-aligned) address >= KERNEL_ASPACE_BASE.
    // TODO(https://fxbug.dev/42098994): Choose it randomly.
    ldr_global kernel_vaddr, kernel_relocated_base

    // Load the base of the translation tables.
    ldr_global page_table0, root_lower_page_table_phys
    ldr_global page_table1, root_kernel_page_table_phys

    // In between here and .Lmmu_enable is where the boot CPU will have set up
    // the trampoline page tables. If we're a secondary we can jump straight to
    // enabling paging.
    cbnz    cpuid, .Lmmu_enable

    /* set up a functional stack pointer */
    adr_global tmp, boot_cpu_kstack_end
    mov     sp, tmp

    /* save the physical address the kernel is loaded at */
    adr_global x0, __executable_start
    adr_global x1, kernel_base_phys
    str     x0, [x1]

    /* call into the arm64 boot mapping code to give it a chance to initialize its page tables */
    sub     x0, kernel_vaddr, x0 /* compute the delta between virtual and physical addresses */
    bl      arm64_boot_map_init

    /* set up the mmu according to mmu_initial_mappings */

    /* void arm64_boot_map(pte_t* kernel_table0, vaddr_t vaddr, paddr_t paddr, size_t len,
     *                     pte_t flags, bool allow_large_pages);
     */

    /* map a large run of physical memory at the base of the kernel's address space
     * TODO(https://fxbug.dev/42124648): Only map the arenas. */
    mov     x0, page_table1
    mov     x1, KERNEL_ASPACE_BASE
    mov     x2, 0
    mov     x3, ARCH_PHYSMAP_SIZE
    movlit  x4, MMU_PTE_KERNEL_DATA_FLAGS
    mov     x5, 1 /* allow large pages */
    bl      arm64_boot_map

.Lmmu_enable:
    /* set up the mmu */

    /* Invalidate the entire TLB */
    tlbi    vmalle1is
    dsb     sy
    isb

    // Initialize Memory Attribute Indirection Register
    //
    // In the case of the boot CPU, care is taken to ensure that currently
    // active attributes and indices are the same as those in MMU_MAIR_VAL.
    movlit  tmp, MMU_MAIR_VAL
    msr     mair_el1, tmp

    /* Initialize TCR_EL1 */
    /* set cacheable attributes on translation walk */
    /* (SMP extensions) non-shareable, inner write-back write-allocate */
    /* both aspaces active, current ASID in TTBR1 */
    movlit  tmp, MMU_TCR_FLAGS_IDENT
    msr     tcr_el1, tmp
    isb

    /* Write the ttbrs with phys addr of the translation table */
    msr     ttbr0_el1, page_table0
    /* Or in 0x1 (GLOBAL_ASID) bits. Keep in sync with mmu.h  */
    orr     tmp, page_table1, #(0x1 << 48)
    msr     ttbr1_el1, tmp
    isb

    /* Read SCTLR */
    mrs     tmp, sctlr_el1

    /* Turn on the MMU */
    orr     tmp, tmp, #(1<<0)

    /* Write back SCTLR */
    msr     sctlr_el1, tmp
.Lmmu_on_pc:
    isb

    // Map our current physical PC to the virtual PC and jump there.
    // PC = next_PC - __executable_start + kernel_vaddr
    adr     tmp, .Lmmu_on_vaddr
    adr_global tmp2, __executable_start
    sub     tmp, tmp, tmp2
    add     tmp, tmp, kernel_vaddr
    br      tmp

.Lmmu_on_vaddr:
    /* Disable trampoline page-table in ttbr0 */
    movlit  tmp, MMU_TCR_FLAGS_KERNEL
    msr     tcr_el1, tmp
    isb

    /* Invalidate the entire TLB */
    tlbi    vmalle1
    dsb     sy
    isb

    cbnz    cpuid, .Lsecondary_boot

    // set up the boot stack for real
    adr_global tmp, boot_cpu_kstack_end
    mov     sp, tmp

    // Set the thread pointer early so compiler-generated references
    // to the stack-guard and unsafe-sp slots work.  This is not a
    // real 'struct thread' yet, just a pointer to (past, actually)
    // the two slots used by the ABI known to the compiler.  This avoids
    // having to compile-time disable safe-stack and stack-protector
    // code generation features for all the C code in the bootstrap
    // path, which (unlike on x86, e.g.) is enough to get annoying.
    adr_global tmp, boot_cpu_fake_thread_pointer_location
    msr     tpidr_el1, tmp
#if __has_feature(shadow_call_stack)
    // The shadow call stack grows up.
    adr_global shadow_call_sp, boot_cpu_shadow_call_kstack
#endif

    // set the per cpu pointer for cpu 0
    adr_global percpu_ptr, arm64_percpu_array

    // Choose a good (ideally random) stack-guard value as early as possible.
    bl      choose_stack_guard
    mrs     tmp, tpidr_el1
    str     x0, [tmp, #ZX_TLS_STACK_GUARD_OFFSET]
    // Don't leak the value to other code.
    mov     x0, xzr

    // Collect the time stamp of entering "normal" C++ code in virtual space.
    sample_ticks
    store_ticks kernel_virtual_entry_ticks

    mov x0, handoff_paddr
    bl  lk_main
    b   .

.Lsecondary_boot:
    // Before we set sp, ensure that it's 'linked' to the current SP_ELX and
    // not SP_EL0.
    msr     SPSel, #1

    bl      arm64_get_secondary_sp
    cbz     x0, .Lunsupported_cpu_trap
    mov     sp, x0

    msr     tpidr_el1, x1
#if __has_feature(shadow_call_stack)
    mov     shadow_call_sp, x2
#endif

    bl      arm64_secondary_entry

.Lunsupported_cpu_trap:
    wfe
    b       .Lunsupported_cpu_trap
END_FUNCTION(_start)

.ltorg

// These are logically .bss (uninitialized data).  But they're set before
// clearing the .bss, so put them in .data so they don't get zeroed.
.data
    .balign 64
DATA(arch_boot_el)
    .quad 0xdeadbeef00ff00ff
END_DATA(arch_boot_el)
DATA(kernel_entry_paddr)
    .quad -1
END_DATA(kernel_entry_paddr)
DATA(kernel_entry_ticks)
    .quad -1, -1
END_DATA(kernel_entry_ticks)

    .balign 8
LOCAL_DATA(boot_cpu_fake_arch_thread)
    .quad 0xdeadbeef1ee2d00d // stack_guard
#if __has_feature(safe_stack)
    .quad boot_cpu_unsafe_kstack_end
#else
    .quad 0
#endif
LOCAL_DATA(boot_cpu_fake_thread_pointer_location)
END_DATA(boot_cpu_fake_arch_thread)

.bss
LOCAL_DATA(boot_cpu_kstack)
    .skip ARCH_DEFAULT_STACK_SIZE
    .balign 16
LOCAL_DATA(boot_cpu_kstack_end)
END_DATA(boot_cpu_kstack)

#if __has_feature(safe_stack)
LOCAL_DATA(boot_cpu_unsafe_kstack)
    .skip ARCH_DEFAULT_STACK_SIZE
    .balign 16
LOCAL_DATA(boot_cpu_unsafe_kstack_end)
END_DATA(boot_cpu_unsafe_kstack)
#endif

#if __has_feature(shadow_call_stack)
    .balign 8
LOCAL_DATA(boot_cpu_shadow_call_kstack)
    .skip PAGE_SIZE
END_DATA(boot_cpu_shadow_call_kstack)
#endif

.section .bss.prebss.translation_table, "aw", @nobits
.align 3 + MMU_IDENT_PAGE_TABLE_ENTRIES_TOP_SHIFT
DATA(tt_trampoline)
    .skip 8 * MMU_IDENT_PAGE_TABLE_ENTRIES_TOP
END_DATA(tt_trampoline)

// This symbol is used by gdb python to know the base of the kernel module
.global KERNEL_BASE_ADDRESS
KERNEL_BASE_ADDRESS = KERNEL_BASE
