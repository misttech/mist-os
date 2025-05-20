// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "thread-allocator.h"

#include <lib/elfldltl/machine.h>

#include <concepts>
#include <utility>

#include "shadow-call-stack.h"
#include "threads_impl.h"

namespace LIBC_NAMESPACE_DECL {
namespace {

// If the thread's ZX_PROP_NAME is empty, the VMO will use this name instead.
constexpr std::string_view kVmoName = "thread-stacks+TLS";

// This is used in GuardedPageBlock::Allocate calls for clarity.
constexpr PageRoundedSize kNoGuard{};

// Each GuardedPageBlock member and its corresponding pointer member are
// represented by an object constructed as `T(block_, ptr_, stack, guard)` for
// the various kinds of blocks.  Each class below meets this same API contract
// once constructed with its corresponding pair of members and the thread's
// configured stack and guard sizes.
template <typename T>
concept BlockType = requires(T& t, zx::unowned_vmar vmar, AllocationVmo& vmo) {
  { std::as_const(t).VmoSize() } -> std::same_as<PageRoundedSize>;
  { t.Allocate(vmar->borrow(), vmo) } -> std::same_as<zx::result<>>;
};

// The machine and unsafe stacks grow down, so they get a guard below.
struct StackGrowsDown {
  PageRoundedSize VmoSize() const { return stack_size; }

  zx::result<> Allocate(zx::unowned_vmar vmar, AllocationVmo& vmo) {
    auto result = block.Allocate<uint64_t>(vmar->borrow(), vmo, stack_size, guard_size, kNoGuard);
    if (result.is_error()) [[unlikely]] {
      return result.take_error();
    }
    sp = result->data() + result->size();
    return zx::ok();
  }

  GuardedPageBlock& block;
  uint64_t*& sp;
  PageRoundedSize stack_size;
  PageRoundedSize guard_size;
};

// The shadow call stack grows up, so it gets a guard above.
// This is templated to dispatch to the real or the no-op version.
template <typename Block>
struct StackGrowsUp;

// The deduction guide will choose based on the shadow_call_stack_ member type.
template <typename Block, typename Sp>
StackGrowsUp(Block& block, Sp& sp, PageRoundedSize stack_size, PageRoundedSize guard_size)
    -> StackGrowsUp<Block>;

template <>
struct StackGrowsUp<GuardedPageBlock> {
  PageRoundedSize VmoSize() const { return stack_size; }

  zx::result<> Allocate(zx::unowned_vmar vmar, AllocationVmo& vmo) {
    auto result = block.Allocate<uint64_t>(vmar->borrow(), vmo, stack_size, kNoGuard, guard_size);
    if (result.is_error()) [[unlikely]] {
      return result.take_error();
    }
    sp = result->data();
    return zx::ok();
  }

  GuardedPageBlock& block;
  uint64_t*& sp;
  PageRoundedSize stack_size;
  PageRoundedSize guard_size;
};

// When there is no shadow call stack ABI, this version does nothing.
template <>
struct StackGrowsUp<NoShadowCallStack> {
  constexpr StackGrowsUp(NoShadowCallStack& no_stack, NoShadowCallStack& no_sp,
                         PageRoundedSize stack_size, PageRoundedSize guard_size) {}

  PageRoundedSize VmoSize() const { return {}; }

  zx::result<> Allocate(zx::unowned_vmar vmar, AllocationVmo& vmo) { return zx::ok(); }
};

// This includes both the TCB and the (runtime-dynamic) static TLS segments.
struct ThreadBlock {
  PageRoundedSize VmoSize() const { return tls_size; }

  zx::result<> Allocate(zx::unowned_vmar vmar, AllocationVmo& vmo) {
    PageRoundedSize page_size = PageRoundedSize::Page();
    auto result = block.Allocate(vmar->borrow(), vmo, tls_size, page_size, page_size);
    if (result.is_error()) [[unlikely]] {
      return result.take_error();
    }
    thread_block = *result;
    return zx::ok();
  }

  GuardedPageBlock& block;
  std::span<std::byte>& thread_block;
  PageRoundedSize tls_size;
};
static_assert(BlockType<ThreadBlock>);

// This is the result of computations for allocating the thread block.  The
// PT_TLS p_align fields affect the computations within, but regardless the
// whole block is always page-aligned (so any p_align larger than a page is
// effectively treated as only a page).  This is allocated via GuardedPageBlock
// with guards both above and below to minimize chances of overruns out of TLS
// into something else or out of something else into TLS or the TCB.  (In the
// x86 kTlsNegative layout, overruns from TLS into the TCB are unguarded while
// elsewhere only underruns from TLS back into the TCB are what's unguarded.)
struct ThreadBlockSize {
  PageRoundedSize size;  // Total size to allocate for TLS + TCB.
  ptrdiff_t tp_offset;   // Point $tp this far inside the block allocated.
};

// These abbreviations are used in the layout descriptions below:
//  * $tp: the thread pointer, as __builtin_thread_pointer() returns
//  * TLSn: the PT_TLS segment for static TLS module with ID n (n > 0)
//    - When the main executable has a PT_TLS of its own, then it has ID 1
//      and its $tp offset is fixed by an ABI calculation at static link time.
//  * Sn: the runtime address in a given thread where the TLSn block starts
//    - This must lie at an address 0 mod p_align, occupying p_memsz bytes.
//    - (Sn - $tp) is the value that appears in GOT slots for IE accesses.
//    - (S1 - $tp) for LE accesses is fixed by the ABI given TLS1.p_align.
//  * DTV: the Dynamic Thread Vector of traditional dynamic TLS implementations
//    - Not part of any public ABI contract, but sometimes described as if so.
//  * TCB: the Thread object
//    - This is a private implementation detail of libc (mostly).
//    - The <zircon/tls.h> Fuchsia Compiler ABI slots lie within this object,
//      though they are expressed as byte offsets from $tp.
//    - When kTlsLocalExecOffset is nonzero (ARM), it describes the final bytes
//      of the Thread object (psABI), so the Thread object in memory will
//      straddle $tp.  The ABI specifies that this much space above $tp is
//      reserved for the implementation and says nothing about what goes there.
//    - When kTpSelfPointer is nonzero (x86), the void* directly at $tp must be
//      set to the $tp address itself.  Only x86 has this and also only x86 has
//      kTlsNegative, so this first word above $tp is not part of the layout
//      calculations imposed by the ABI for TLS; instead, it's just a runtime
//      requirement and forms the first part of the Thread object.
//    - In traditional implementations, the second word above $tp (either in
//      the private TCB or in the ABI-specified reserved area) holds the DTV.
//      This has never been part of any ABI contract, except one private to
//      some particular dynamic linker, its __tls_get_addr implementation, and
//      its TLSDESC implementation hooks.
//  * T: the runtime address of the Thread object
//  * FC: the <zircon/tls.h> Fuchsia Compiler ABI slots
//    - <zircon/tls.h> defines two per-machine offsets from $tp for ABI use
//    - These are used by --target=*-fuchsia compilers by default, but are not
//      part of the basic machine ABI or the Fuchsia System ABI.  They are not
//      used by the startup dynamic linker or vDSO, nor provided by the system
//      program loader.  They are only provided here in libc.
//    - This implementation includes these in the TCB.
//  * psABI: ELF processor-specific ABI-mandated kTlsLocalExecOffset space
//    - The psABI for ARM and AArch64 specifies this, but not its use.
//    - Traditional implementations have used it just like the first two words
//      past $tp on x86: a $tp self-pointer (though nothing uses that); and the
//      DTV (a private implementation detail).
// TCB is used to refer to the whole Thread object and also sometimes to
// distinguish FC and psABI from the rest of the Thread object.  A future
// implementation might have clearer distinctions in the data structures.

template <class TlsTraits = elfldltl::TlsTraits<>>
  requires(TlsTraits::kTlsNegative)
ThreadBlockSize ComputeThreadBlockSize(elfldltl::TlsLayout<> static_tls_layout) {
  // This layout is used only on x86 (both EM_386 and EM_X86_64).  To be
  // pedantic, the ABI requirement kTpSelfPointer indicates is orthogonal;
  // but it's related, and also unique to x86.  kTlsLocalExecOffset is also
  // zero on some machines other than x86, but it's especially helpful to
  // ignore a possible nonzero value in explaining the kTlsNegative layout.
  static_assert(TlsTraits::kTpSelfPointer);
  static_assert(TlsTraits::kTlsLocalExecOffset == 0);

  // *----------------------------------------------------------------------*
  // |   TLSn | ... | TLS1 | $tp . (DTV) . FC . TCB... | unused             |
  // *---^----^-----^------^-----^-------^----^-----------------------------*
  // |   Sn   S2    S1    $tp=T  +8      +16  +32                            |
  // *----------------------------------------------------------------------*
  // Note: T == $tp
  //
  // TLS offsets are negative, but the layout size is computed ascending from
  // zero with PT_TLS p_memsz and p_align requirements; then each segment's
  // offset is just negated at the end.  So the layout size is how many bytes
  // below $tp will be used.  The start address of each TLSn (Sn) must meet
  // TLSn's p_align requirement (capped at one OS page, as in PT_LOAD), but
  // TLS1 is always assigned first (closest to $tp).  The whole block must be
  // aligned to the maximum of the alignment requirements of each TLSn and
  // the TCB.  Then the offsets will be "aligned up" before being negated,
  // such that each address Sn is correctly aligned (the first TLSn block in
  // memory, for the largest n, will have the same alignment as $tp--the
  // maximum of any block).  Once the offsets have been assigned in this way,
  // each TLSn block starts at $tp + (negative) offset.  That may be followed
  // by unused padding space as required to make S(n+1) be 0 mod the TLS(n+1)
  // p_align.  When TLS1 is the LE segment from the executable, that padding
  // will be included in its (negative offset) such that $tp itself will have
  // at least the same alignment as TLS1.  Layout mechanics require that $tp
  // be thus aligned to the maximum needed by any TLSn or the TCB.
  //
  // The Fuchsia Compiler ABI <zircon/tls.h> slots are early in the TCB,
  // appearing in Thread just after the $tp slot and the second slot
  // traditionally used for the DTV (but formally just reserved for private
  // implementation use).
  //
  // Since the allocation will be in whole pages, there will often be some
  // unused space still available off the end of the TCB.  It's feasible to
  // reuse this opportunistically for PT_TLS segments of modules loaded later
  // (e.g. dlopen)--nothing *requires* that these $tp offsets be negative at
  // runtime.  But the layout calculations would need to start anew to track
  // the space and alignment requirements.
  const size_t tls_size = static_tls_layout.Align(  //
      static_tls_layout.size_bytes(), alignof(Thread));
  return {
      .size{tls_size + sizeof(Thread)},
      .tp_offset = static_cast<ptrdiff_t>(tls_size),
  };
}

template <class TlsTraits = elfldltl::TlsTraits<>>
  requires(!TlsTraits::kTlsNegative)
ThreadBlockSize ComputeThreadBlockSize(elfldltl::TlsLayout<> static_tls_layout) {
  // This style of layout is used on all machines other than x86.
  // The kTlsLocalExecOffset value differs by machine.
  //
  // *----------------------------------------------------------------------*
  // |  unused | TCB, FC | [psABI] | TLS1 | ... | TLSn | unused             |
  // *---------^---------^---------^------^-----^------^--------------------*
  // |         T        $tp        S1     S2    Sn     (Sn+p_memsz)         |
  // *----------------------------------------------------------------------*
  // Note: T == $tp + kTlsLocalExecOffset - sizeof(Thread)
  //
  // TLS offsets are positive, so the TCB (Thread) will sit just below $tp.
  // Any ABI-specified reserved area (ABI) is the tail of the layout of
  // Thread.  This means that Thread straddles $tp, which points to the ABI
  // reserved area that forms the last kTlsLocalExecOffset bytes of the
  // Thread object.  When kTlsLocalExecOffset is zero, $tp points exactly
  // just past Thread, which is also exactly the S1 address where TLS1 starts
  // (the LE segment assigned per ABI at static link time if there is one) .
  //
  // **Note:** It is always $tp _itself_ that must be aligned to the maximum
  // TLSn p_align!  When kTlsLocalExecOffset is nonzero, the static linker
  // starts its LE offset assignments there and then rounds up if the p_align
  // in the LE (executable) TLS1 is larger than that ABI-specified offset.
  //
  // kTpSelfPointer is not required for any non-x86 machine yet, but if it
  // were then the ABI's kTlsLocalExecOffset value would account for it.
  //
  // Note also that the 16 bytes just _below_ $tp are reserved by the Fuchsia
  // Compiler ABI for the two <zircon/tls.h> slots.
  //
  // If the TLS layout's required alignment is less than alignof(Thread), the
  // allocation will be aligned for Thread.  Otherwise, the allocation will
  // be aligned for the TLS layout and may include unused padding bytes at
  // the start so the TCB sits at exactly `$tp - sizeof(Thread)` while still
  // meeting the TLS alignment requirement for $tp.
  //
  // Since the allocation will be in whole pages, there will often be some
  // unused space still available off the end of the last TLSn.  This layout
  // makes it easy to just iteratively do another static_tls_layout.Assign to
  // see if a correctly-placed new block fits in the space left over.  There
  // may also be space at the beginning of the block if the TLS alignment is
  // greater than alignof(Thread), which will not be recovered for reuse.
  const size_t aligned_thread_size = static_tls_layout.Align(sizeof(Thread), alignof(Thread));
  auto tls_layout_size = [static_tls_layout]() -> size_t {
    if (static_tls_layout.size_bytes() == 0) {
      return 0;
    }
    // The TLS layout size includes the reserved area, already part of Thread.
    assert(static_tls_layout.size_bytes() > TlsTraits::kTlsLocalExecOffset);
    return static_tls_layout.size_bytes() - TlsTraits::kTlsLocalExecOffset;
  };
  return {
      .size{aligned_thread_size + tls_layout_size()},
      .tp_offset = static_cast<ptrdiff_t>(aligned_thread_size),
  };
}

}  // namespace

zx::result<> ThreadAllocator::Allocate(zx::unowned_vmar allocate_from, std::string_view thread_name,
                                       PageRoundedSize stack, PageRoundedSize guard) {
  if (thread_name.empty()) {
    thread_name = kVmoName;
  }

  // The VMO space and mapping is handled the same for each block.
  auto allocate_blocks = [&](ThreadBlock thread_block, BlockType auto... stacks) -> zx::result<> {
    // Allocate a single VMO for all the blocks.
    zx::result vmo = AllocationVmo::New(thread_block.VmoSize() + (stacks.VmoSize() + ...));
    if (vmo.is_error()) [[unlikely]] {
      return vmo.take_error();
    }

    // Map in each block's portion of that VMO.  After this, the mappings
    // (including guards) cannot be modified, only unmapped whole from above.
    auto map_one_block = [allocate_from, &vmo = *vmo](BlockType auto&& block) {
      return block.Allocate(allocate_from->borrow(), vmo);
    };
    auto map_blocks = [map_one_block](BlockType auto&&... blocks) {
      zx::result<> result = zx::ok();
      ((result = map_one_block(blocks)).is_ok() && ...);
      return result;
    };

    // Allocate the largest blocks first to minimize fragmentation in the VMAR.
    // The stacks all have the same size.
    if (zx::result<> result = thread_block.VmoSize() >= stack  //
                                  ? map_blocks(thread_block, stacks...)
                                  : map_blocks(stacks..., thread_block);
        result.is_error()) [[unlikely]] {
      return result;
    }

    // Name the VMO to match the thread.
    return zx::make_result(
        vmo->vmo.set_property(ZX_PROP_NAME, thread_name.data(), thread_name.size()));
  };

  // The thread block size is a complex calculation, while the others depend
  // only on the stack and guard sizes.
  auto [thread_block_size, tp_offset] = ComputeThreadBlockSize(GetTlsLayout());

  // Allocate all the blocks together in a single VMO and map each separately.
  uint64_t* unsafe_sp = nullptr;
  std::span<std::byte> thread_block;
  if (zx::result<> result = allocate_blocks(  //
          ThreadBlock(thread_block_, thread_block, thread_block_size),
          StackGrowsDown(machine_stack_, machine_sp_, stack, guard),
          StackGrowsDown(unsafe_stack_, unsafe_sp, stack, guard),
          StackGrowsUp(shadow_call_stack_, shadow_call_sp_, stack, guard));
      result.is_error()) [[unlikely]] {
    return result;
  }

  // Initialize the static TLS data from PT_TLS segments.
  InitializeTls(thread_block, tp_offset);

  // The location of the Thread object inside the thread block is part of the
  // complex sizing calculation, while all the stack pointers are just at one
  // end of their block or the other.
  thread_ = tp_to_pthread(thread_block.data() + tp_offset);
  if constexpr (elfldltl::TlsTraits<>::kTpSelfPointer) {
    void* tp = pthread_to_tp(thread_);
    assert(&thread_->head.tp == tp);
    thread_->head.tp = reinterpret_cast<uintptr_t>(tp);
  }

  // The unsafe stack pointer is always part of the Thread rather than using a
  // machine register, so it doesn't have a ThreadAllocator member to fill one.
  thread_->abi.unsafe_sp = reinterpret_cast<uintptr_t>(unsafe_sp);

  return zx::ok();
}

}  // namespace LIBC_NAMESPACE_DECL
