// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_C_THREADS_THREAD_STORAGE_H_
#define LIB_C_THREADS_THREAD_STORAGE_H_

#include <lib/elfldltl/tls-layout.h>
#include <lib/zx/result.h>

#include <cstddef>
#include <span>
#include <string_view>
#include <type_traits>

#include "../zircon/vmar.h"
#include "shadow-call-stack.h"
#include "src/__support/macros/config.h"

struct __pthread;  // Forward declared for legacy "threads_impl.h".

namespace LIBC_NAMESPACE_DECL {

using Thread = ::__pthread;  // Legacy C type.

// ThreadStorage handles the memory allocation and ownership for Thread.  It's
// responsible for allocating all the various kinds of stacks, and the thread
// area that underlies both the implementation's private Thread Control Block
// (TCB) as well as the public Fuchsia Compiler ABI and ELF TLS.  ThreadStorage
// initializes only the parts of the TCB that are part of the public ABI
// (including all of ELF Initial Exec TLS) and then leaves the rest of the TCB
// zero-initialized for later use by libc internals.
//
// Initially it's used at process startup and thread creation.  It owns those
// allocations and cleans them up on destruction e.g. if thread creation fails.
// During the thread's lifetime, the ThreadStorage is moved into its Thread.
// This is a bit tricky, as the Thread object's own memory resides in one of
// the blocks that ThreadStorage owns.  So to destroy Thread, its ThreadStorage
// must be moved back out before explicitly calling ~Thread().
//
// ThreadStorage can only be default-constructed or moved.  Before Allocate()
// has returned successfully (or after it fails), it should only be destroyed
// and no other methods used (except for moves).
class ThreadStorage {
 public:
  constexpr ThreadStorage() = default;
  ThreadStorage(ThreadStorage&& other) { *this = std::move(other); }

  ThreadStorage& operator=(ThreadStorage&& other) {
    auto move_members = [this, &other](auto... m) {
      ((this->*m = std::exchange(other.*m, {})), ...);
    };
    move_members(&ThreadStorage::stack_size_, &ThreadStorage::guard_size_,
                 &ThreadStorage::thread_block_, &ThreadStorage::machine_stack_,
                 &ThreadStorage::unsafe_stack_, &ThreadStorage::shadow_call_stack_);
    return *this;
  }

  // This allocates everything and holds ownership in this object.  If it
  // returns an error, some resources may now be owned but nothing more should
  // be done with the object but to destroy it (and thus reclaim them).
  //
  // When this returns success, the remaining methods return useful values.
  // The machine stack, unsafe stack (for the SafeStack ABI, enabled on all
  // machines), and shadow call stack (enabled on some machines), are all
  // allocated with guard pages and all-zeroes contents.  In the thread area:
  //
  //  * The unsafe stack pointer at $tp + ZX_TLS_UNSAFE_SP_OFFSET is set to
  //    the top of the unsafe stack.
  //
  //  * If the machine's TLS ABI has *$tp = $tp (like x86), that is set.
  //
  //  * The ZX_TLS_STACK_GUARD_OFFSET word still must be set.  For the initial
  //    thread, the caller will choose random bits; for new threads, the caller
  //    will copy the value found via its own $tp.
  //
  //  * All the ELF Initial Exec TLS data (static TLS) is initialized.
  //
  //  * The DTV for the dynamic TLS implementation is allocated and filled in.
  //    TODO(https://fxbug.dev/397084454): The new //sdk/lib/dl TLS runtime
  //    does not require a bespoke ABI contract for a DTV.
  //
  // The name should match the ZX_PROP_NAME used for the zx::thread.  The VMAR
  // handle is saved and used for destruction, so it must remain valid for the
  // lifetime of the object (it's just the long-lived primary allocation / root
  // VMAR handle, except in tests).
  //
  // The returned Thread* points somewhere inside the thread area block owned
  // by this ThreadStorage object, which also contains the static TLS area
  // already initialized with all the Initial Exec TLS data.  The ABI rules
  // govern where that is in relation to the thread pointer ($tp) and thereby,
  // indirectly, where the TCB lies relative to $tp.  (Note that here we
  // consider the Fuchsia Compiler ABI <zircon/tls.h> fixed slots, as well as
  // the $tp->self pointer on x86, to be part of the TCB--at one end of it or
  // the other--though nothing else about the TCB is part of any public ABI.)
  zx::result<Thread*> Allocate(zx::unowned_vmar allocate_from, std::string_view thread_name,
                               PageRoundedSize stack, PageRoundedSize guard);

  // This frees just the blocks for all the stacks, leaving the thread block
  // (where the Thread object itself resides) intact until destruction.
  void FreeStacks();

  // This moves ownership of the ThreadStorage out of the Thread, making it
  // possible to destroy the Thread before destroying the ThreadStorage makes
  // its memory inaccessible.  (When Thread becomes a true C++ type, this will
  // be replaced with plain move-construction from its ThreadStorage member.)
  static ThreadStorage FromThread(Thread& thread, zx::unowned_vmar vmar);

  // This moves ownership from this ThreadStorage into the Thread.  (When
  // Thread becomes a true C++ type, this will be replaced with plain
  // move-assignment to its ThreadStorage member.)
  void ToThread(Thread& thread) &&;

  // This returns the initial value for the machine SP.  This is always the
  // limit of the stack, where a push (`*--sp = ...`) will be the first thing
  // done.  This makes it appropriate for a call site, and on most machines for
  // the entry to a C function.  But on x86 it needs a return address pushed
  // before it can be used at a C function's entry point.
  uint64_t* machine_sp() const { return GrowsDown(machine_stack_); }

  // This returns the initial value for the unsafe SP.  This is always the
  // limit of the stack, which is always the protocol for function entry.
  // (This is already stored at $tp + ZX_TLS_UNSAFE_SP_OFFSET, too.)
  uint64_t* unsafe_sp() const { return GrowsDown(unsafe_stack_); }

  // This returns the initial value for the shadow call stack pointer.
  // That stack grows up, so the next operation will be `*sp++ = ...`.
  uint64_t* shadow_call_sp() const { return GrowsUp(shadow_call_stack_); }

  PageRoundedSize stack_size() const { return stack_size_; }
  PageRoundedSize guard_size() const { return guard_size_; }

  ~ThreadStorage() { FreeStacks(); }

  // This takes a pointer-to-data-member and all the members are private.  It's
  // only used in implementation template code outside the class that's called
  // by method implementations.
  template <typename T>
    requires(!std::is_function_v<T>)
  static constexpr bool StackGrowsUp(T ThreadStorage::* stack) {
    if constexpr (kShadowCallStackAbi) {
      return stack == &ThreadStorage::shadow_call_stack_;
    }
    return false;
  }

 private:
  // The shadow call stack grows up with guard above, so the initial pointer is
  // just the base of the mapping.
  static uint64_t* GrowsUp(NoShadowCallStack) { return nullptr; }
  static uint64_t* GrowsUp(uintptr_t base) { return reinterpret_cast<uint64_t*>(base); }

  // The other stacks grow down with guard below, so the initial pointer is at
  // the end of the whole mapping.
  uint64_t* GrowsDown(uintptr_t base) const {
    return reinterpret_cast<uint64_t*>(base + guard_size_.get() + stack_size_.get());
  }

  // This acquires the information from the dynamic linker or from a static
  // PIE's own PT_TLS segment.
  static elfldltl::TlsLayout<> GetTlsLayout();

  // Given that `thread_block.data() + tp_offset` will become $tp for the new
  // thread and that the space set aside for static TLS is all zero bytes now,
  // this will initialize all its PT_TLS segments properly.
  static void InitializeTls(std::span<std::byte> thread_block, size_t tp_offset);

  GuardedPageBlock thread_block_;
  PageRoundedSize stack_size_, guard_size_;
  uintptr_t machine_stack_ = 0;
  uintptr_t unsafe_stack_ = 0;
  [[no_unique_address]] IfShadowCallStack<uintptr_t> shadow_call_stack_{};
};
static_assert(std::is_default_constructible_v<ThreadStorage>);
static_assert(std::is_move_constructible_v<ThreadStorage>);
static_assert(std::is_move_assignable_v<ThreadStorage>);

}  // namespace LIBC_NAMESPACE_DECL

#endif  // LIB_C_THREADS_THREAD_STORAGE_H_
