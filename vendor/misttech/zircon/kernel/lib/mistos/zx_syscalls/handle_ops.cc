// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/mistos/zx_syscalls/util.h>
#include <lib/syscalls/forward.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <object/handle.h>
#include <object/process_dispatcher.h>
#include <object/user_handles.h>

#define LOCAL_TRACE 0

zx_status_t zx_handle_close(zx_handle_t handle_value) {
  LTRACEF("handle %x\n", handle_value);

  // Closing the "never a handle" invalid handle is not an error
  // It's like free(NULL).
  if (handle_value == ZX_HANDLE_INVALID)
    return ZX_OK;

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }
  HandleOwner handle(handle_table(up).RemoveHandle(*up, handle_value));
  if (!handle)
    return ZX_ERR_BAD_HANDLE;
  return ZX_OK;
}

#if 0
// zx_status_t zx_handle_close_many
zx_status_t sys_handle_close_many(user_in_ptr<const zx_handle_t> handles, size_t num_handles) {
  LTRACEF("handles %p, num_handles %zu\n", handles.get(), num_handles);

  auto up = ProcessDispatcher::GetCurrent();
  return RemoveUserHandles(handles, num_handles, up);
}

#endif

static zx_status_t handle_dup_replace(bool is_replace, zx_handle_t handle_value, zx_rights_t rights,
                                      zx_handle_t* out) {
  LTRACEF("handle %x\n", handle_value);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  AutoExpiringPreemptDisabler preempt_disable{Mutex::DEFAULT_TIMESLICE_EXTENSION};
  Guard<BrwLockPi, BrwLockPi::Writer> guard{handle_table(up).get_lock()};
  auto source = handle_table(up).GetHandleLocked(*up, handle_value);
  if (!source)
    return ZX_ERR_BAD_HANDLE;

  if (!is_replace) {
    if (!source->HasRights(ZX_RIGHT_DUPLICATE))
      return ZX_ERR_ACCESS_DENIED;
  }

  if (rights == ZX_RIGHT_SAME_RIGHTS) {
    rights = source->rights();
  } else if ((source->rights() & rights) != rights) {
    if (is_replace)
      handle_table(up).RemoveHandleLocked(source);
    return ZX_ERR_INVALID_ARGS;
  }

  HandleOwner handle = Handle::Dup(source, rights);
  if (!handle) {
    return ZX_ERR_NO_MEMORY;
  }

  if (is_replace)
    handle_table(up).RemoveHandleLocked(source);

  *out = handle_table(up).MapHandleToValue(handle);
  handle_table(up).AddHandleLocked(ktl::move(handle));
  return ZX_OK;
}

zx_status_t zx_handle_duplicate(zx_handle_t handle_value, zx_rights_t rights, zx_handle_t* out) {
  return handle_dup_replace(false, handle_value, rights, out);
}

zx_status_t zx_handle_replace(zx_handle_t handle_value, zx_rights_t rights, zx_handle_t* out) {
  return handle_dup_replace(true, handle_value, rights, out);
}
