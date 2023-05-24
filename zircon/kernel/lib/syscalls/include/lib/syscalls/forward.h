// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_SYSCALLS_INCLUDE_LIB_SYSCALLS_FORWARD_H_
#define ZIRCON_KERNEL_LIB_SYSCALLS_INCLUDE_LIB_SYSCALLS_FORWARD_H_

#include <lib/user_copy/user_ptr.h>
#include <zircon/syscalls/types.h>
#include <zircon/types.h>

#include <object/handle.h>
#include <object/process_dispatcher.h>

// This is the type of handle result parameters in system call
// implementation functions (sys_*).  zither recognizes return values of
// type zx_handle_t and converts them into user_out_handle* instead of into
// user_out_ptr<zx_handle_t>.  System call implementation functions use the
// make, dup, or transfer method to turn a Dispatcher pointer or another
// handle into a handle received by the user.
//
// user_out_handle will generate a policy exception if destroyed while the
// enclosed HandleOwner is non-null, as we expect all syscalls to successfully
// move the Handle to userspace.
class user_out_handle final {
 public:
  ~user_out_handle() {
    if (h_) {
      // user_out_handle should never go out of scope with a valid handle, so
      // if it does we raise an exception to the process.
      Thread::Current::SignalPolicyException(ZX_EXCP_POLICY_CODE_HANDLE_LEAK, 0u);
    }
  }

  zx_status_t make(fbl::RefPtr<Dispatcher> dispatcher, zx_rights_t rights) {
    h_ = Handle::Make(ktl::move(dispatcher), rights);
    return h_ ? ZX_OK : ZX_ERR_NO_MEMORY;
  }

  // Note that if this call fails to allocate the Handle, the underlying
  // Dispatcher's on_zero_handles() will be called.
  zx_status_t make(KernelHandle<Dispatcher> handle, zx_rights_t rights) {
    h_ = Handle::Make(ktl::move(handle), rights);
    return h_ ? ZX_OK : ZX_ERR_NO_MEMORY;
  }

  zx_status_t dup(Handle* source, zx_rights_t rights) {
    h_ = Handle::Dup(source, rights);
    return h_ ? ZX_OK : ZX_ERR_NO_MEMORY;
  }

  zx_status_t transfer(HandleOwner&& source) {
    h_.swap(source);
    return ZX_OK;
  }

  // These methods are called by the zither-generated wrapper_* functions
  // (syscall-kernel-wrappers.inc).  See KernelWrappersOutput.

  zx_status_t begin_copyout(ProcessDispatcher* current_process,
                            user_out_ptr<zx_handle_t> out) const {
    // The result of `copy_to_user` will be propagated to the syscall wrapper.
    // Changing the status values returned will result in a user-observable
    // change of behaviour.
    if (h_)
      return out.copy_to_user(current_process->handle_table().MapHandleToValue(h_));
    return ZX_ERR_INVALID_ARGS;
  }

  void finish_copyout(ProcessDispatcher* current_process) {
    if (h_)
      current_process->handle_table().AddHandle(ktl::move(h_));
  }

 private:
  HandleOwner h_;
};

// One of these macros is invoked by kernel.inc for each syscall.

// These don't have kernel entry points.
#define VDSO_SYSCALL(...)

// These are the direct kernel entry points.
#define KERNEL_SYSCALL(name, type, attrs, nargs, arglist, prototype) \
  attrs type sys_##name prototype;
#define INTERNAL_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)
#define BLOCKING_SYSCALL(...) KERNEL_SYSCALL(__VA_ARGS__)

#ifdef __clang__
#define _ZX_SYSCALL_ANNO(anno) __attribute__((anno))
#else
#define _ZX_SYSCALL_ANNO(anno)
#endif

#include <lib/syscalls/kernel.inc>

#undef VDSO_SYSCALL
#undef KERNEL_SYSCALL
#undef INTERNAL_SYSCALL
#undef BLOCKING_SYSCALL
#undef _ZX_SYSCALL_ANNO

#endif  // ZIRCON_KERNEL_LIB_SYSCALLS_INCLUDE_LIB_SYSCALLS_FORWARD_H_
