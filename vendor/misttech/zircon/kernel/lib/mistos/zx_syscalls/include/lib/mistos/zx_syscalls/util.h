// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_SYSCALLS_INCLUDE_LIB_MISTOS_UTIL_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_SYSCALLS_INCLUDE_LIB_MISTOS_UTIL_H_

#include <object/dispatcher.h>
#include <object/handle.h>
#include <object/handle_table.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>

HandleTable& handle_table(ProcessDispatcher* dispatcher);

// Allocates a handle with the given rights to the given dispatcher. The handle is added to the
// calling process' handle table, and its value is returned in out.
zx_status_t MakeAndAddHandle(fbl::RefPtr<Dispatcher> dispatcher, zx_rights_t rights,
                             zx_handle_t* out);
// Allocates a handle with the given rights to the dispatcher enclosed in the given kernel handle.
// The handle is added to the calling process' handle table, and its value is returned in out.
zx_status_t MakeAndAddHandle(KernelHandle<Dispatcher> kernel_handle, zx_rights_t rights,
                             zx_handle_t* out);

template <typename T>
zx_status_t TransferHandle(zx_handle_t dest_process_handle, zx_handle_t handle,
                           zx_handle_t* new_handle) {
  ProcessDispatcher* up = nullptr;
  if (ThreadDispatcher::GetCurrent()) {
    up = ProcessDispatcher::GetCurrent();
  }

  fbl::RefPtr<ProcessDispatcher> dest_process;
  zx_status_t status = handle_table(up).GetDispatcher(*up, dest_process_handle, &dest_process);
  if (status != ZX_OK) {
    return status;
  }
  ASSERT_MSG(dest_process, "invalid dest process handle");

  fbl::RefPtr<T> dispatcher;
  zx_rights_t rights;
  status = handle_table(up).GetDispatcherAndRights(*up, handle, &dispatcher, &rights);
  if (status != ZX_OK) {
    return status;
  }
  ASSERT_MSG(dispatcher, "invalid handle");

  return dest_process->MakeAndAddHandle(dispatcher, rights, new_handle);
}

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_SYSCALLS_INCLUDE_LIB_MISTOS_UTIL_H_
