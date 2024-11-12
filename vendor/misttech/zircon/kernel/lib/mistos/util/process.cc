// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/assert.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <object/job_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <vm/vm_address_region.h>

// These functions are part of Fuchsia's libc runtime, but implemented in the kernel for MistOS.

fbl::RefPtr<ProcessDispatcher> zx_process_self() {
  return fbl::RefPtr(ProcessDispatcher::GetCurrent());
}

fbl::RefPtr<VmAddressRegion> zx_vmar_root_self() { return VmAspace::kernel_aspace()->RootVmar(); }

fbl::RefPtr<JobDispatcher> zx_job_default() { return GetRootJobDispatcher(); }

fbl::RefPtr<JobDispatcher> zx_job_root() { return GetRootJobDispatcher(); }

fbl::RefPtr<ThreadDispatcher> zx_thread_self() {
  return fbl::RefPtr(ThreadDispatcher::GetCurrent());
}
