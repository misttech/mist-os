// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_THREADS_THREAD_ALLOCATOR_H_
#define ZIRCON_SYSTEM_ULIB_C_THREADS_THREAD_ALLOCATOR_H_

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

// ThreadAllocator is a temporary object used at process startup and thread
// creation.  It's responsible for allocating all the various kinds of stacks,
// and the thread area that underlies both the implementation's private Thread
// Control Block (TCB) as well as the public Fuchsia Compiler ABI and ELF TLS.
// It initializes only the parts of the TCB that are part of the public ABI
// (including all of ELF Initial Exec TLS) and then leaves the rest of the TCB
// zero-initialized for later use by libc internals.
//
// ThreadAllocator can only be default-constructed or moved.  Before Allocate()
// has returned successfully (or after it fails), it should only be destroyed
// and no other methods used (except for moves).
//
// After Allocate() succeeds, the Take*() methods must transfer all the
// ownership out of the object before it dies.  The accessor methods for the
// initial register values can be used before or after ownership transfers.
class ThreadAllocator {
 public:
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
  //  * All the ELF Initial Exec TLS data (static TLS) is initialized.
  //
  //  * The DTV for the dynamic TLS implementation is allocated and filled in.
  //    TODO(https://fxbug.dev/397084454): The new //sdk/lib/dl TLS runtime
  //    does not require a bespoke ABI contract for a DTV.
  //
  // The name should match the ZX_PROP_NAME used for the zx::thread.
  zx::result<> Allocate(zx::unowned_vmar allocate_from, std::string_view thread_name,
                        PageRoundedSize stack, PageRoundedSize guard);

  // This returns the TCB.  It is mostly guaranteed to be zero, but the unsafe
  // stack pointer and the $tp->self pointer (if any) are already set.  For the
  // Fuchsia Compiler ABI, the ZX_TLS_STACK_GUARD_OFFSET word still must be
  // set.  For the initial thread, the caller will choose random bits; for new
  // threads, the caller will copy the value found via its own $tp.
  //
  // This points somewhere inside the block returned by TakeTls(), which also
  // contains the static TLS area already initialized with all the Initial Exec
  // TLS data.  The ABI rules govern where that is in relation to the thread
  // pointer ($tp) and thereby, indirectly, where the TCB lies relative to $tp.
  // (Note that here we consider the Fuchsia Compiler ABI <zircon/tls.h> fixed
  // slots, as well as the $tp->self pointer on x86, to be part of the TCB--at
  // one end of it or the other--though nothing else about the TCB is part of
  // any public ABI.)
  Thread* thread() const { return thread_; }

  // This returns the initial value for the machine SP.  This is always the
  // limit of the stack, where a push (`*--sp = ...`) will be the first thing
  // done.  This makes it appropriate for a call site, and on most machines for
  // the entry to a C function.  But on x86 it needs a return address pushed
  // before it can be used at a C function's entry point.
  uint64_t* machine_sp() const { return machine_sp_; }

  // This returns the initial value for the shadow call stack pointer.
  // That stack grows up, so the next operation will be `*sp++ = ...`.
  uint64_t* shadow_call_sp() const { return IfShadowCallSp(shadow_call_sp_); }

  // These methods transfer the ownership of each block to the caller, so they
  // can be destroyed properly at the end of the thread's lifetime.  The stacks
  // ultimately are owned by the thread control block object, and are destroyed
  // when the live thread exits.  The Thread object (TCB) nominally also owns
  // the TLS block.  But that object itself resides in part of the TLS block,
  // so the GuardedPageBlock itself must be moved out of there again before
  // it's destroyed, which is either on join or on detached exit.
  GuardedPageBlock TakeThreadBlock() { return std::exchange(thread_block_, {}); }
  GuardedPageBlock TakeMachineStack() { return std::exchange(machine_stack_, {}); }
  GuardedPageBlock TakeUnsafeStack() { return std::exchange(unsafe_stack_, {}); }
  IfShadowCallStack<GuardedPageBlock> TakeShadowCallStack() {
    return std::exchange(shadow_call_stack_, {});
  }

 private:
  static constexpr uint64_t* IfShadowCallSp(uint64_t* ptr) { return ptr; }
  static constexpr uint64_t* IfShadowCallSp(NoShadowCallStack) { return nullptr; }

  // This acquires the information from the dynamic linker or from a static
  // PIE's own PT_TLS segment.
  static elfldltl::TlsLayout<> GetTlsLayout();

  // Given that `thread_block.data() + tp_offset` will become $tp for the new
  // thread and that the space set aside for static TLS is all zero bytes now,
  // this will initialize all its PT_TLS segments properly.
  static void InitializeTls(std::span<std::byte> thread_block, ptrdiff_t tp_offset);

  GuardedPageBlock thread_block_;
  GuardedPageBlock machine_stack_;
  GuardedPageBlock unsafe_stack_;
  [[no_unique_address]] IfShadowCallStack<GuardedPageBlock> shadow_call_stack_;
  Thread* thread_ = nullptr;
  uint64_t* machine_sp_ = nullptr;
  [[no_unique_address]] IfShadowCallStack<uint64_t*> shadow_call_sp_{};
};
static_assert(std::is_default_constructible_v<ThreadAllocator>);
static_assert(std::is_move_constructible_v<ThreadAllocator>);
static_assert(std::is_move_assignable_v<ThreadAllocator>);

}  // namespace LIBC_NAMESPACE_DECL

#endif  // ZIRCON_SYSTEM_ULIB_C_THREADS_THREAD_ALLOCATOR_H_
