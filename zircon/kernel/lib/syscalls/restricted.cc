// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <platform.h>
#include <stdint.h>
#include <stdlib.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/syscalls/policy.h>
#include <zircon/types.h>

#include <kernel/restricted.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object.h>
#include <vm/vm_object_paged.h>

#define LOCAL_TRACE 0

zx_status_t sys_restricted_enter(uint32_t options, uintptr_t vector_table_ptr, uintptr_t context) {
  LTRACEF("options %#x vector %#" PRIx64 " context %#" PRIx64 "\n", options, vector_table_ptr,
          context);

  // Only x86 and arm64 are supported.
#if !defined(__x86_64__) && !defined(__aarch64__)
  return ZX_ERR_NOT_SUPPORTED;
#endif

  // Reject invalid option bits.
  if (options & (~ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL)) {
    return ZX_ERR_INVALID_ARGS;
  }

#if !defined(__x86_64__) && !defined(__aarch64__)
  // TODO(https://fxbug.dev/121512): Lift restriction once we have in-thread exceptions implemented.
  if (!(options & ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL)) {
    return ZX_ERR_NOT_SUPPORTED;
  }
#endif

  return RestrictedEnter(options, vector_table_ptr, context);
}

zx_status_t sys_restricted_bind_state(uint32_t options, user_out_handle* out) {
  LTRACEF("options 0x%x\n", options);

  // Only x86 and arm64 are supported.
#if !defined(__x86_64__) && !defined(__aarch64__)
  return ZX_ERR_NOT_SUPPORTED;
#endif

  // No options allowed.
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Are we allowed to create a VMO?
  auto up = ProcessDispatcher::GetCurrent();
  zx_status_t status = up->EnforceBasicPolicy(ZX_POL_NEW_VMO);
  if (status != ZX_OK) {
    return status;
  }

  // Create it.
  zx::result<ktl::unique_ptr<RestrictedState>> result = RestrictedState::Create();
  if (result.is_error()) {
    return result.error_value();
  }

  // Now wrap the VMO in a VmObjectDispatcher so we can give a handle back to the user.
  ktl::unique_ptr<RestrictedState> rs = ktl::move(result.value());
  fbl::RefPtr<VmObjectPaged> vmo = rs->vmo();
  fbl::RefPtr<ContentSizeManager> content_size_manager;
  status = ContentSizeManager::Create(vmo->size(), &content_size_manager);
  if (status != ZX_OK) {
    return status;
  }
  KernelHandle<VmObjectDispatcher> kernel_handle;
  zx_rights_t rights;
  status = VmObjectDispatcher::Create(ktl::move(vmo), ktl::move(content_size_manager),
                                      VmObjectDispatcher::InitialMutability::kMutable,
                                      &kernel_handle, &rights);
  if (status != ZX_OK) {
    return status;
  }

  // Wrap the VmObjectDispatcher in a Handle.
  status = out->make(ktl::move(kernel_handle), rights);
  if (status != ZX_OK) {
    return ZX_OK;
  }

  // Finally, set this thread's restricted state.  Note, it's possible the copy-out of the new
  // handle will fail, but that's OK.  If that happens the handle will be destroyed along with the
  // VmObjectDispatcher.  However, the VMO itself (VmObjectPaged) will persist until the thread is
  // destroyed, the user calls unbind, or the user binds different VMO.
  Thread::Current::Get()->set_restricted_state(ktl::move(rs));

  return ZX_OK;
}

zx_status_t sys_restricted_unbind_state(uint32_t options) {
  LTRACEF("options 0x%x\n", options);

  // Only x86 and arm64 are supported.
#if !defined(__x86_64__) && !defined(__aarch64__)
  return ZX_ERR_NOT_SUPPORTED;
#endif

  // No options allowed.
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  Thread::Current::Get()->set_restricted_state(nullptr);

  return ZX_OK;
}

// zx_restricted_kick
zx_status_t sys_restricted_kick(zx_handle_t handle, uint32_t options) {
  LTRACEF("options 0x%x\n", options);

  // Only x86 and arm64 are supported.
#if !defined(__x86_64__) && !defined(__aarch64__)
  return ZX_ERR_NOT_SUPPORTED;
#endif

  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<ThreadDispatcher> thread;
  // TODO(https://fxbug.dev/126688): Decide if this is the correct right for this operation.
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_MANAGE_THREAD, &thread);
  if (status != ZX_OK) {
    return status;
  }

  return thread->RestrictedKick();
}
