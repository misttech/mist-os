// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_STATE_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_STATE_H_

#include <lib/user_copy/internal.h>
#include <lib/zx/result.h>
#include <zircon/syscalls-next.h>

#include <arch/exception.h>
#include <arch/regs.h>
#include <fbl/macros.h>
#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

// Per thread state to support restricted mode.
// Intentionally kept simple to keep the amount of kernel/thread.h dependencies to a minimum.

class AttributionObject;
class VmObjectPaged;
class VmMapping;

// Must provide a definition of struct ArchSavedNormalState
#include <arch/restricted.h>

// Encapsulates a thread's restricted mode state.
//
// Note everything in this class should be accessed only by the thread that it belongs to,
// since there is no internal locking for efficiency reasons.
class RestrictedState {
 public:
  static zx::result<ktl::unique_ptr<RestrictedState>> Create(
      fbl::RefPtr<AttributionObject> attribution_object);

  ~RestrictedState();
  DISALLOW_COPY_ASSIGN_AND_MOVE(RestrictedState);

  bool in_restricted() const { return in_restricted_; }
  uintptr_t vector_ptr() const { return vector_ptr_; }
  uintptr_t context() const { return context_; }
  bool in_thread_exceptions_enabled() const { return in_thread_exceptions_enabled_; }
  const ArchSavedNormalState& arch_normal_state() const { return arch_; }
  ArchSavedNormalState& arch_normal_state() { return arch_; }
  template <typename T>
  T* state_ptr_as() const {
    static_assert(internal::is_copy_allowed<T>::value);
    return reinterpret_cast<T*>(state_mapping_ptr_);
  }
  zx_restricted_state_t* state_ptr() const { return state_ptr_as<zx_restricted_state_t>(); }

  fbl::RefPtr<VmObjectPaged> vmo() const;

  void set_in_restricted(bool r) { in_restricted_ = r; }
  void set_vector_ptr(uintptr_t v) { vector_ptr_ = v; }
  void set_context(uintptr_t c) { context_ = c; }
  void set_in_thread_exceptions_enabled(bool enable) { in_thread_exceptions_enabled_ = enable; }

  // Each arch must fill out the following routines prefixed with Arch:
  //
  // Prior to entering restricted mode, ask the arch layer to validate the saved register state is
  // valid. Return ZX_OK if valid.
  // Possible invalid states: program counter outside of user space, invalid processor flags, etc.
  static zx_status_t ArchValidateStatePreRestrictedEntry(const zx_restricted_state_t& state);

  // Just prior to entering restricted mode, give the arch layer a chance to save any state it
  // may need for the return trip back to normal mode into the ArchSavedNormalState state argument.
  // For example, the GS/FS base is saved here for x86.
  static void ArchSaveStatePreRestrictedEntry(ArchSavedNormalState& state);

  // Use an architcturally-specific mechanism to directly enter user space in restricted mode.
  // Does not return.
  [[noreturn]] static void ArchEnterRestricted(const zx_restricted_state_t& state);

  // Having just exited from restricted mode via a syscall, save the necessary restricted mode
  // state from a pointer to the syscall state saved by the exception handler.
  static void ArchSaveRestrictedSyscallState(zx_restricted_state_t& state,
                                             const syscall_regs_t& regs);

  // Having just exited from restricted mode via an interrupt, save the necessary restricted mode
  // state from a pointer to the interrupt frame state saved by the exception handler.
  static void ArchSaveRestrictedIframeState(zx_restricted_state_t& state, const iframe_t& frame);

  // Having exited from restricted mode via a synchronous exception, save the necessary
  // restricted mode state.
  static void ArchSaveRestrictedExceptionState(zx_restricted_state_t& state);

  // Update the exception context so that we will return to normal mode to allow normal
  // mode to handle a restricted exception.
  static void ArchRedirectRestrictedExceptionToNormal(const ArchSavedNormalState& arch_state,
                                                      uintptr_t vector_table, uintptr_t context);
  // Enter normal mode at the address pointed to by vector_table with arguments code and context
  // in an architecturally specific register in an architecturally specific way.
  [[noreturn]] static void ArchEnterFull(const ArchSavedNormalState& arch_state,
                                         uintptr_t vector_table, uintptr_t context, uint64_t code);

  // Dump the architecturally specific state out of the restricted mode state
  static void ArchDump(const zx_restricted_state_t& state);

 private:
  RestrictedState(fbl::RefPtr<VmObjectPaged> state_vmo, fbl::RefPtr<VmMapping> state_mapping);

  bool in_restricted_ = false;
  bool in_thread_exceptions_enabled_ = false;
  uintptr_t vector_ptr_ = 0;
  uintptr_t context_ = 0;

  // Ref pointer to a vmo holding the restricted state and a kernel mapping of the first page.
  const fbl::RefPtr<VmObjectPaged> state_vmo_;
  const fbl::RefPtr<VmMapping> state_mapping_;

  // Cached pointer to the mapping, to avoid needing to deref the mapping on every access.
  void* const state_mapping_ptr_ = nullptr;

  // Arch specific part of the save state
  ArchSavedNormalState arch_;
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_RESTRICTED_STATE_H_
