// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MIST_OS_UTIL_INCLUDE_LIB_MISTOS_UTIL_PROCESS_H_
#define ZIRCON_KERNEL_LIB_MIST_OS_UTIL_INCLUDE_LIB_MISTOS_UTIL_PROCESS_H_

#include <fbl/ref_ptr.h>

class ProcessDispatcher;
class VmAddressRegion;
class JobDispatcher;
class ThreadDispatcher;

fbl::RefPtr<ProcessDispatcher> zx_process_self();
fbl::RefPtr<VmAddressRegion> zx_vmar_root_self();
fbl::RefPtr<JobDispatcher> zx_job_default();
fbl::RefPtr<JobDispatcher> zx_job_root();
fbl::RefPtr<ThreadDispatcher> zx_thread_self();

#endif  // ZIRCON_KERNEL_LIB_MIST_OS_UTIL_INCLUDE_LIB_MISTOS_UTIL_PROCESS_H_
