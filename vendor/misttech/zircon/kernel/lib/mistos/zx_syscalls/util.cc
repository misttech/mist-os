// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx_syscalls/util.h"

#include <lib/lazy_init/lazy_init.h>
#include <zircon/process.h>
#include <zircon/types.h>

#include <lk/init.h>
#include <object/job_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>

static lazy_init::LazyInit<HandleTable> gHandleTable;

static zx_handle_t __zircon_vmar_root_self = ZX_HANDLE_INVALID;
static zx_handle_t __zircon_job_default = ZX_HANDLE_INVALID;

HandleTable& handle_table(ProcessDispatcher* dispatcher) {
  return dispatcher ? dispatcher->handle_table() : gHandleTable.Get();
}

zx_status_t MakeAndAddHandle(fbl::RefPtr<Dispatcher> dispatcher, zx_rights_t rights,
                             zx_handle_t* out) {
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  HandleOwner handle = Handle::Make(ktl::move(dispatcher), rights);
  if (!handle) {
    return ZX_ERR_NO_MEMORY;
  }
  *out = handle_table(up).MapHandleToValue(handle);
  handle_table(up).AddHandle(ktl::move(handle));
  return ZX_OK;
}

zx_status_t MakeAndAddHandle(KernelHandle<Dispatcher> kernel_handle, zx_rights_t rights,
                             zx_handle_t* out) {
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  HandleOwner handle = Handle::Make(ktl::move(kernel_handle), rights);
  if (!handle) {
    return ZX_ERR_NO_MEMORY;
  }
  *out = handle_table(up).MapHandleToValue(handle);
  handle_table(up).AddHandle(ktl::move(handle));
  return ZX_OK;
}

static void zx_syscall_init(uint level) {
  gHandleTable.Initialize();

  // Create a dispatcher for the root VMAR.
  zx_rights_t root_vmar_rights;
  KernelHandle<VmAddressRegionDispatcher> new_vmar_handle;
  zx_status_t result = VmAddressRegionDispatcher::Create(VmAspace::kernel_aspace()->RootVmar(), 0,
                                                         &new_vmar_handle, &root_vmar_rights);

  if (result == ZX_OK) {
    ASSERT(MakeAndAddHandle(ktl::move(new_vmar_handle), root_vmar_rights,
                            &__zircon_vmar_root_self) == ZX_OK);
  }

  ASSERT(MakeAndAddHandle(GetRootJobDispatcher(), JobDispatcher::default_rights(),
                          &__zircon_job_default) == ZX_OK);
}

ZX_HANDLE_ACQUIRE_UNOWNED zx_handle_t zx_vmar_root_self(void) { return __zircon_vmar_root_self; }
zx_handle_t zx_job_default(void) { return __zircon_job_default; }
zx_handle_t zx_thread_self(void) { return ZX_HANDLE_INVALID; }

LK_INIT_HOOK(mist_os_zx_syscall, zx_syscall_init, LK_INIT_LEVEL_USER - 2)
