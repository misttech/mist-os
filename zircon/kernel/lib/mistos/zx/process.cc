// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/process.h"

#include <lib/mistos/zx/job.h>
#include <lib/mistos/zx/thread.h>
#include <lib/mistos/zx/vmar.h>
#include <zircon/errors.h>

#include <object/process_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

zx_status_t process::create(const job& job, const char* name, uint32_t name_len, uint32_t flags,
                            process* proc, vmar* vmar) {
  // Assume |proc|, |vmar| and |job| must refer to different containers, due
  // to strict aliasing.

  // currently, the only valid option values are 0 or ZX_PROCESS_SHARED
  if (flags != ZX_PROCESS_SHARED && flags != 0)
    return ZX_ERR_INVALID_ARGS;

  // copy out the name
  char buf[ZX_MAX_NAME_LEN];
  ktl::string_view sp;

  // Silently truncate the given name.
  if (name_len > ZX_MAX_NAME_LEN)
    name_len = ZX_MAX_NAME_LEN;
  memcpy(buf, name, name_len);

  // ensure zero termination
  size_t str_len = (name_len == ZX_MAX_NAME_LEN ? name_len - 1 : name_len);
  buf[str_len] = 0;
  sp = ktl::string_view(buf);

  LTRACEF("name %s\n", buf);

  // create a new process dispatcher
  KernelHandle<ProcessDispatcher> new_process_handle;
  KernelHandle<VmAddressRegionDispatcher> new_vmar_handle;
  zx_rights_t proc_rights, vmar_rights;
  zx_status_t status = ProcessDispatcher::Create(job.get(), sp, flags, &new_process_handle,
                                                 &proc_rights, &new_vmar_handle, &vmar_rights);
  if (status != ZX_OK)
    return status;

  proc->reset(new_process_handle.release());
  vmar->reset(new_vmar_handle.release()->vmar());
  return ZX_OK;
}

zx_status_t process::start(const thread& thread_handle, uintptr_t entry, uintptr_t stack,
                           /*handle arg_handle*/ uintptr_t arg1, uintptr_t arg2) const {
  const fbl::RefPtr<ProcessDispatcher> process = get();
  const fbl::RefPtr<ThreadDispatcher> thread = thread_handle.get();

  if (!process || !thread) {
    return ZX_ERR_BAD_HANDLE;
  }

  TRACEF("phandle %p, thandle %p, pc %#" PRIxPTR ", sp %#" PRIxPTR ", arg1 %#" PRIxPTR
         ", arg2 %#" PRIxPTR "\n",
         process.get(), thread.get(), entry, stack, arg1, arg2);

  // test that the thread belongs to the starting process
  if (thread->process() != process.get())
    return ZX_ERR_ACCESS_DENIED;

  zx_status_t status = thread->Start(ThreadDispatcher::EntryState{entry, stack, arg1, arg2},
                                     /* ensure_initial_thread */ true);
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

zx_status_t process::get_child(uint64_t koid, zx_rights_t rights, thread* result) const {
  // Assume |result| and |this| are distinct containers, due to strict
  // aliasing.
  // return zx_object_get_child(value_, koid, rights, result->reset_and_get_address());
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t process::get_info(uint32_t topic, void* buffer, size_t buffer_size,
                              size_t* actual_count, size_t* avail_count) const {
  LTRACE;
  if (!get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  switch (topic) {
    case ZX_INFO_PROCESS: {
      zx_info_process_t info = {};
      get()->GetInfo(&info);
      return single_record_result(buffer, buffer_size, actual_count, avail_count, info);
    }
    default:
      LTRACEF("[NOT_SUPPORTED] Topic %d\n", topic);
      return ZX_ERR_NOT_SUPPORTED;
  }
}

}  // namespace zx
