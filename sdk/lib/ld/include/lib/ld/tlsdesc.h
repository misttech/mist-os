// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TLSDESC_H_
#define LIB_LD_TLSDESC_H_

#include <lib/arch/asm.h>

// These declarations relate to the TLSDESC runtime ABI.  While the particular
// ABI details are specific to each machine, they all fit a common pattern.
//
// The R_*_TLSDESC relocation type directs dynamic linking to fill in a special
// pair of adjacent GOT slots.  The first slot is unfilled at link time and
// gets the PC of a special function provided by the dynamic linking runtime.
// For each TLS reference, the compiler generates an indirect call via this GOT
// slot.  The compiler also passes the address of its GOT slot to the function.
//
// This is a normal indirect call at the machine level.  However, it uses its
// own bespoke calling convention specified in the psABI for each machine
// rather than the standard C/C++ calling convention.  The convention for each
// machine is similar: the use of the return address register and/or stack is
// normal; one or two registers are designated for the argument (GOT address),
// return value, and scratch; all other registers are preserved by the call,
// except the condition codes.  The return value is a signed offset from the
// psABI-specified thread-pointer register.  Notably, it's expected to be added
// to the thread pointer to yield a valid pointer or nullptr for undefined weak
// symbol references, so it may be a difference of unrelated pointers to reach
// a heap address not near the thread pointer or to reach zero for nullptr.
//
// This makes the impact of the runtime call on code generation very minimal.
// The runtime implementation both can refer to the value stored in the GOT
// slot by dynamic linking and can in theory dynamically update both slots to
// lazily redirect to a different runtime entry point and argument data.
//
// The relocation's symbol and addend are meant to apply to the second GOT slot
// of the pair.  (For DT_REL format, the addend is stored in place there.)
// When dynamic linking chooses an entry point to store into the first GOT slot
// it also chooses the value to store in the second slot, which is some kind of
// offset or address that includes the addend and symbol value calculations.

#ifdef __ASSEMBLER__  // clang-format off

// The pseudo-op `.tlsdesc.cfi`, given `.cfi_startproc` initial state,
// resets CFI to indicate the special ABI for the R_*_TLSDESC callback
// function on this machine.
//
// Other conveniences are defined specific to each machine.

#if defined(__aarch64__)

.macro .tlsdesc.cfi
  // Almost all registers are preserved from the caller.  The integer set does
  // not include x30 (LR) or SP, which .cfi_startproc covered.
  .cfi.all_integer .cfi_same_value
  .cfi.all_vectorfp .cfi_same_value
.endm

// On AArch64 ILP32, GOT entries are 4 bytes, not 8.
# ifdef _LP64
tlsdesc.r0 .req x0
tlsdesc.r1 .req x1
tlsdesc.value_offset = 8
# else
tlsdesc.r0 .req w0
tlsdesc.r1 .req w1
tlsdesc.value_offset = 4
# endif

#elif defined(__riscv)

.macro .tlsdesc.cfi
  // Almost all registers are preserved from the caller.  The integer set does
  // not include sp, which .cfi_startproc covered.
  .cfi.all_integer .cfi_same_value
  .cfi.all_vectorfp .cfi_same_value

  // The return address is in t0 rather than the usual ra, and preserved there.
  .cfi_return_column t0
.endm

# ifdef _LP64
.macro tlsdesc.load reg, mem
  ld \reg, \mem
.endm
.macro tlsdesc.sub rd, r1, r2
  sub \rd, \r1, \r2
.endm
tlsdesc.value_offset = 8
# else
.macro tlsdesc.load reg, mem
  lw \reg, \mem
.endm
.macro tlsdesc.sub rd, r1, r2
  subw \rd, \r1, \r2
.endm
tlsdesc.value_offset = 4
# endif

#elif defined(__x86_64__)

.macro .tlsdesc.cfi
  // Almost all registers are preserved from the caller.  The integer set does
  // not include %rsp, which .cfi_startproc covered.
  .cfi.all_integer .cfi_same_value
  .cfi.all_vectorfp .cfi_same_value
.endm

# ifdef _LP64
#define tlsdesc_ax rax
tlsdesc.value_offset = 4
# else
#define tlsdesc_ax eax
tlsdesc.value_offset = 8
# endif

#else

// Not all machines have TLSDESC support specified in the psABI.

#endif

#else  // clang-format on

#include <lib/elfldltl/layout.h>

#include <cstddef>

namespace [[gnu::visibility("hidden")]] ld {

// These are callback functions to be used in the TlsDescGot::function slot
// at runtime.  Though they're declared here as C++ functions with an
// argument, they're actually implemented in assembly code with a bespoke
// calling convention for the argument, return value, and register usage
// that's different from normal functions, so these cannot actually be
// called from C++.  These symbol names are not visible anywhere outside
// the dynamic linking implementation itself and these functions are only
// ever called by compiler-generated TLSDESC references.

extern "C" {

// These are used for undefined weak definitions.  The value slot contains just
// the addend; the first entry-point ignores the addend and is cheaper for a
// zero addend (the most common case), while the second supports an addend.
// The implementation returns the addend minus the thread pointer, such that
// adding the thread pointer back to this offset produces zero with a zero
// addend, and thus nullptr.
ptrdiff_t _ld_tlsdesc_runtime_undefined_weak(const elfldltl::Elf<>::TlsDescGot& got);
ptrdiff_t _ld_tlsdesc_runtime_undefined_weak_addend(const elfldltl::Elf<>::TlsDescGot& got);

// In this minimal implementation used for PT_TLS segments in the static TLS
// set, desc.valueu is always simply a fixed offset from the thread pointer.
// Note this offset might be negative, but it's always handled as uintptr_t to
// ensure well-defined overflow arithmetic.
ptrdiff_t _ld_tlsdesc_runtime_static(const elfldltl::Elf<>::TlsDescGot& got);

}  // extern "C"

}  // namespace ld

#endif  // __ASSEMBLER__

#endif  // LIB_LD_TLSDESC_H_
