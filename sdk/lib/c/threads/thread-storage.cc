// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "thread-storage.h"

#include <lib/elfldltl/machine.h>

#include <concepts>
#include <utility>

#include "shadow-call-stack.h"
#include "threads_impl.h"

namespace LIBC_NAMESPACE_DECL {
namespace {

using TlsLayout = elfldltl::TlsLayout<>;

// If the thread's ZX_PROP_NAME is empty, the VMO will use this name instead.
constexpr std::string_view kVmoName = "thread-stacks+TLS";

// Each of the blocks owned by ThreadStorage is represented by a Block
// specialization for some &ThreadStorage::member_.  Each class below meets
// this same API contract.
template <typename T>
concept BlockType = requires(T& t, ThreadStorage& storage, PageRoundedSize tls_size,
                             zx::unowned_vmar vmar, AllocationVmo& vmo) {
  // Returns the size needed in the AllocationVmo.
  { std::as_const(t).VmoSize(std::as_const(storage), tls_size) } -> std::same_as<PageRoundedSize>;

  // Maps the block into the VMAR from the AllocationVmo.
  { t.Map(std::as_const(storage), tls_size, vmar->borrow(), vmo).is_ok() } -> std::same_as<bool>;

  // Commits the block to the successfully-allocated ThreadStorage object.
  { t.Commit(storage) };
};

template <BlockType T>
using BlockTypeCheck = T;

template <auto Member>
class Block;

template <auto Member>
using BlockFor = BlockTypeCheck<Block<Member>>;

// This handles each of the stack blocks.
template <uintptr_t ThreadStorage::* Member>
class Block<Member> {
 public:
  PageRoundedSize VmoSize(const ThreadStorage& storage, PageRoundedSize tls_size) const {
    return storage.stack_size();
  }

  auto Map(const ThreadStorage& storage, PageRoundedSize tls_size, zx::unowned_vmar vmar,
           AllocationVmo& vmo) {
    PageRoundedSize guard_below, guard_above;
    if constexpr (ThreadStorage::StackGrowsUp(Member)) {
      guard_above = storage.guard_size();
    } else {
      guard_below = storage.guard_size();
    }
    return block_.Allocate<uint64_t>(vmar->borrow(), vmo, storage.stack_size(), guard_below,
                                     guard_above);
  }

  void Commit(ThreadStorage& storage) { storage.*Member = block_.release(); }

 private:
  GuardedPageBlock block_;
};

// This handles shadow_call_stack_ when it's a no-op.
template <NoShadowCallStack ThreadStorage::* Member>
class Block<Member> {
 public:
  PageRoundedSize VmoSize(const ThreadStorage& storage, PageRoundedSize tls_size) const {
    return {};
  }

  zx::result<std::span<uint64_t>> Map(const ThreadStorage& storage, PageRoundedSize tls_size,
                                      zx::unowned_vmar vmar, AllocationVmo& vmo) {
    return zx::ok(std::span<uint64_t>{});
  }

  void Commit(ThreadStorage& storage) {}
};

// This handles thread_block_, which includes both the TCB and the
// (runtime-dynamic) static TLS segments.  It always gets one-page guards both
// above and below, regardless of the configured guard size for the stacks.
template <GuardedPageBlock ThreadStorage::* Member>
class Block<Member> {
 public:
  PageRoundedSize VmoSize(const ThreadStorage& storage, PageRoundedSize tls_size) const {
    return tls_size;
  }

  auto Map(const ThreadStorage& storage, PageRoundedSize tls_size, zx::unowned_vmar vmar,
           AllocationVmo& vmo) {
    const PageRoundedSize page_size = PageRoundedSize::Page();
    return block_.Allocate(vmar->borrow(), vmo, tls_size, page_size, page_size);
  }

  void Commit(ThreadStorage& storage) { storage.*Member = std::move(block_); }

 private:
  GuardedPageBlock block_;
};

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
  size_t tp_offset;      // Point $tp this far inside the block allocated.
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
ThreadBlockSize ComputeThreadBlockSize(TlsLayout static_tls_layout) {
  // This layout is used only on x86 (both EM_386 and EM_X86_64).  To be
  // pedantic, the ABI requirement kTpSelfPointer indicates is orthogonal;
  // but it's related, and also unique to x86.  kTlsLocalExecOffset is also
  // zero on some machines other than x86, but it's especially helpful to
  // ignore a possible nonzero value in explaining the kTlsNegative layout.
  static_assert(TlsTraits::kTpSelfPointer);
  static_assert(TlsTraits::kTlsLocalExecOffset == 0);

  // *----------------------------------------------------------------------*
  // |     unused    | TLSn | ... | TLS1 | $tp . (DTV) . FC . TCB | (align) |
  // *---------------^------^-----^------^-----^-------^----^-----*---------*
  // |               Sn     S2    S1    $tp=T  +8      +16  +32             |
  // *----------------------------------------------------------------------*
  // Note: T == $tp
  //
  // TLS offsets are negative, but the layout size is computed ascending from
  // zero with PT_TLS p_memsz and p_align requirements; then each segment's
  // offset is just negated at the end.  So the layout size is how many bytes
  // below $tp will be used.  The start address of each TLSn (Sn) must meet
  // TLSn's p_align requirement (capped at one OS page, as in PT_LOAD), but
  // TLS1 is always assigned first (closest to $tp).  The whole block must be
  // aligned to the maximum of the alignment requirements of each TLSn and the
  // TCB.  Then the offsets will be "aligned up" before being negated, such
  // that each address Sn is correctly aligned (the first TLSn block in memory,
  // for the largest n, will have the same alignment as $tp--the maximum of any
  // block).  Once the offsets have been assigned in this way, each TLSn block
  // starts at $tp + (negative) offset.  That may be followed by unused padding
  // space as required to make S(n+1) be 0 mod the TLS(n+1) p_align.  When TLS1
  // is the LE segment from the executable, that padding will be included in
  // its (negative offset) such that $tp itself will have at least the same
  // alignment as TLS1.  Layout mechanics require that $tp be thus aligned to
  // the maximum needed by any TLSn or the TCB.  If this is larger than the
  // TCB's own size, then there can be some unused space wasted at the very end
  // of the allocation: `alignof($tp) - sizeof(Thread)` bytes.
  //
  // The Fuchsia Compiler ABI <zircon/tls.h> slots are early in the TCB,
  // appearing in Thread just after the $tp slot and the second slot
  // traditionally used for the DTV (but formally just reserved for private
  // implementation use).
  //
  // Since the allocation will be in whole pages, there will often be some
  // unused space beyond any (usual small) amount wasted for alignment.  That
  // (usually larger) unused portion is placed at the beginning of the first
  // page, such that the end of the TCB is at (or close to) the end of the
  // whole allocation and the space "off the end" of TLSn (in the negative
  // direction from $tp) is available.  Reusing that space opportunistically
  // for PT_TLS segments of modules loaded later (e.g. dlopen) is fairly
  // straightforward, though notably a new PT_TLS segment with p_align greater
  // than static_tls_layout.alignment() can never be placed there (even if it
  // fits), as each thread's separate allocation can only be presumed to be
  // aligned to the original layout's requirement.
  const size_t tls_size = static_tls_layout.Align(  //
      static_tls_layout.size_bytes(), alignof(Thread));
  const size_t aligned_thread_size = static_tls_layout.Align(sizeof(Thread), alignof(Thread));
  const PageRoundedSize allocation_size{tls_size + aligned_thread_size};
  return {
      .size = allocation_size,
      .tp_offset = allocation_size.get() - aligned_thread_size,
  };
}

template <class TlsTraits = elfldltl::TlsTraits<>>
  requires(!TlsTraits::kTlsNegative)
ThreadBlockSize ComputeThreadBlockSize(TlsLayout static_tls_layout) {
  // This style of layout is used on all machines other than x86.
  // The kTlsLocalExecOffset value differs by machine.
  //
  // *----------------------------------------------------------------------*
  // |  (align) | TCB, FC | [psABI] | TLS1 | ... | TLSn |     unused        |
  // *----------^---------^---------^------^-----^------^-------------------*
  // |          T        $tp        S1     S2    Sn     (Sn+p_memsz)        |
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
  const size_t aligned_thread_size = static_tls_layout.Align(sizeof(Thread));
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
      .tp_offset = aligned_thread_size,
  };
}

}  // namespace

void ThreadStorage::FreeStacks() {
  auto unmap = [this, block_size = stack_size_ + guard_size_](uintptr_t base) {
    if (base != 0) {
      ZX_DEBUG_ASSERT(thread_block_.vmar());
      zx::result result = zx::make_result(thread_block_.vmar().unmap(base, block_size.get()));
      ZX_ASSERT_MSG(result.is_ok(), "zx_vmar_unmap: %s", result.status_string());
    }
  };
  unmap(machine_stack_);
  unmap(unsafe_stack_);
  OnShadowCallStack(shadow_call_stack_, unmap);
}

// Translate from the legacy C struct representation for ownership.
ThreadStorage ThreadStorage::FromThread(Thread& thread, zx::unowned_vmar vmar) {
  using Sizes = std::array<size_t, 2>;  // Stack size, guard size.
  constexpr auto infer_sizes = [](iovec stack, iovec region, bool grows_up = false) -> Sizes {
    assert(PageRoundedSize{stack.iov_len}.get() == stack.iov_len);
    assert(PageRoundedSize{region.iov_len}.get() == region.iov_len);
    assert(stack.iov_len <= region.iov_len);
    assert(stack.iov_base >= region.iov_base);
    if (stack.iov_base == region.iov_base) {
      assert(grows_up || stack.iov_len == region.iov_len);
    } else {
      assert(!grows_up);
      assert(reinterpret_cast<uintptr_t>(stack.iov_base) -
                 reinterpret_cast<uintptr_t>(region.iov_base) ==
             region.iov_len - stack.iov_len);
    }
    return {stack.iov_len, region.iov_len - stack.iov_len};
  };

  constexpr auto take_stack = [](iovec& stack, iovec& region) -> uintptr_t {
    stack = {};
    return reinterpret_cast<uintptr_t>(std::exchange(region, {}).iov_base);
  };

  Sizes stack_sizes = infer_sizes(thread.safe_stack, thread.safe_stack_region);
  assert(infer_sizes(thread.unsafe_stack, thread.unsafe_stack_region) == stack_sizes);
#if HAVE_SHADOW_CALL_STACK
  assert(infer_sizes(thread.shadow_call_stack, thread.shadow_call_stack_region) == stack_sizes);
#endif

  assert(*vmar);
  ThreadStorage result;
  result.thread_block_ = {std::exchange(thread.tcb_region, {}), vmar->borrow()};
  std::tie(result.stack_size_.rounded_size_, result.guard_size_.rounded_size_) = stack_sizes;
  result.machine_stack_ = take_stack(thread.safe_stack, thread.safe_stack_region);
  result.unsafe_stack_ = take_stack(thread.unsafe_stack, thread.unsafe_stack_region);
#if HAVE_SHADOW_CALL_STACK
  result.shadow_call_stack_ = take_stack(thread.shadow_call_stack, thread.shadow_call_stack_region);
#endif

  return result;
}

void ThreadStorage::ToThread(Thread& thread) && {
  auto take_stack = [this](iovec& stack, iovec& region, uintptr_t& base, bool grows_up = false) {
    assert(!stack.iov_base);
    assert(stack.iov_len == 0);
    assert(!region.iov_base);
    assert(region.iov_len == 0);
    region = {
        .iov_base = reinterpret_cast<void*>(base),
        .iov_len = (stack_size_ + guard_size_).get(),
    };
    stack = {
        .iov_base = reinterpret_cast<void*>(base + (grows_up ? 0 : guard_size_.get())),
        .iov_len = stack_size_.get(),
    };
    base = 0;
  };

  thread.tcb_region = std::move(thread_block_).TakeIovec();

  take_stack(thread.safe_stack, thread.safe_stack_region, machine_stack_);
  take_stack(thread.unsafe_stack, thread.unsafe_stack_region, unsafe_stack_);
#if HAVE_SHADOW_CALL_STACK
  take_stack(thread.shadow_call_stack, thread.shadow_call_stack_region, shadow_call_stack_, true);
#endif
}

zx::result<Thread*> ThreadStorage::Allocate(zx::unowned_vmar allocate_from,
                                            std::string_view thread_name, PageRoundedSize stack,
                                            PageRoundedSize guard) {
  if (thread_name.empty()) {
    thread_name = kVmoName;
  }

  // The thread block size is a complex calculation, while the others depend
  // only on the stack and guard sizes.
  auto [thread_block_size, tp_offset] = ComputeThreadBlockSize(GetTlsLayout());

  stack_size_ = stack;
  guard_size_ = guard;

  std::span<std::byte> thread_block;

  // The VMO space and mapping is handled the same for each block.
  auto allocate_blocks = [&](Block<&ThreadStorage::thread_block_> tcb,
                             BlockType auto... stacks) -> zx::result<> {
    // Allocate a single VMO for all the blocks.
    const PageRoundedSize vmo_size =
        tcb.VmoSize(*this, thread_block_size) + (stacks.VmoSize(*this, thread_block_size) + ...);
    zx::result vmo = AllocationVmo::New(vmo_size);
    if (vmo.is_error()) [[unlikely]] {
      return vmo.take_error();
    }

    auto map_one_block = [&]<BlockType B>(B& block) -> zx::result<> {
      zx::result result = block.Map(*this, thread_block_size, allocate_from->borrow(), *vmo);
      if (result.is_error()) [[unlikely]] {
        return result.take_error();
      }
      if constexpr (std::is_same_v<B, Block<&ThreadStorage::thread_block_>>) {
        thread_block = result.value();
      }
      return fit::ok();
    };

    // Map in each block's portion of that VMO.  After this, the mappings
    // (including guards) cannot be modified, only unmapped whole from above.
    auto map_blocks = [&](BlockType auto&&... blocks) {
      zx::result<> result = zx::ok();
      ((result = map_one_block(blocks)).is_ok() && ...);
      return result;
    };

    // Allocate the largest blocks first to minimize fragmentation in the VMAR.
    // The stacks all have the same size.
    if (zx::result<> result = tcb.VmoSize(*this, thread_block_size) >= stack
                                  ? map_blocks(tcb, stacks...)
                                  : map_blocks(stacks..., tcb);
        result.is_error()) [[unlikely]] {
      return result;
    }

    // Now that everything is mapped in, the ownership can move into this
    // ThreadStorage object.  Everything will be cleaned up on destruction.
    tcb.Commit(*this);
    (stacks.Commit(*this), ...);

    // Name the VMO to match the thread.
    return zx::make_result(
        vmo->vmo.set_property(ZX_PROP_NAME, thread_name.data(), thread_name.size()));
  };

  // Allocate all the blocks together in a single VMO and map each separately.
  if (zx::result<> result = allocate_blocks(        //
          Block<&ThreadStorage::thread_block_>{},   //
          Block<&ThreadStorage::machine_stack_>{},  //
          Block<&ThreadStorage::unsafe_stack_>{},   //
          Block<&ThreadStorage::shadow_call_stack_>{});
      result.is_error()) [[unlikely]] {
    return result.take_error();
  }

  // Initialize the static TLS data from PT_TLS segments.
  InitializeTls(thread_block, tp_offset);

  // The location of the Thread object inside the thread block is part of the
  // complex sizing calculation, while all the stack pointers are just at one
  // end of their block or the other.
  Thread* thread = tp_to_pthread(thread_block.data() + tp_offset);
  if constexpr (elfldltl::TlsTraits<>::kTpSelfPointer) {
    void* tp = pthread_to_tp(thread);
    assert(&thread->head.tp == tp);
    thread->head.tp = reinterpret_cast<uintptr_t>(tp);
  }

  // The unsafe stack pointer is always part of the Thread rather than using a
  // machine register, so it can be initialized right here.
  thread->abi.unsafe_sp = reinterpret_cast<uintptr_t>(unsafe_sp());

  return zx::ok(thread);
}

}  // namespace LIBC_NAMESPACE_DECL
