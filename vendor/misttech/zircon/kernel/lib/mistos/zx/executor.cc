// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/mistos/zx/executor.h"

#include <lk/init.h>

namespace zx {

static Executor gExecutor;

void Executor::Init() {
  KernelHandle<VmAddressRegionDispatcher> kernel_vmar_handle;
  zx_rights_t root_vmar_rights;
  zx_status_t result = VmAddressRegionDispatcher::Create(VmAspace::kernel_aspace()->RootVmar(), 0,
                                                         &kernel_vmar_handle, &root_vmar_rights);
  ZX_ASSERT(result == ZX_OK);

  // Create handle.
  kernel_vmar_handle_ = Handle::Make(ktl::move(kernel_vmar_handle), root_vmar_rights);
  ASSERT(kernel_vmar_handle_ != nullptr);
  auto dispatcher = kernel_vmar_handle_->dispatcher();
  kernel_vmar_ = DownCastDispatcher<VmAddressRegionDispatcher>(&dispatcher);
  ASSERT(kernel_vmar_ != nullptr);
}

Handle* GetKernelVmarHandle() { return gExecutor.GetKernelVmarHandle(); }

static void zx_init(uint level) TA_NO_THREAD_SAFETY_ANALYSIS { gExecutor.Init(); }

}  // namespace zx

LK_INIT_HOOK(libmistoszx, zx::zx_init, LK_INIT_LEVEL_USER - 1)
