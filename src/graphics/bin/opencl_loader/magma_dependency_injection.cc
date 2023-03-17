// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/bin/opencl_loader/magma_dependency_injection.h"

#include <fuchsia/gpu/magma/cpp/fidl.h>
#include <lib/fdio/directory.h>

zx_status_t MagmaDependencyInjection::Initialize() {
  gpu_dependency_injection_watcher_ = fsl::DeviceWatcher::Create(
      "/dev/class/gpu-dependency-injection",
      [this](const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) {
        if (filename == ".") {
          return;
        }
        fuchsia::gpu::magma::DependencyInjectionSyncPtr dependency_injection;
        zx_status_t status =
            fdio_service_connect_at(dir.channel().get(), filename.c_str(),
                                    dependency_injection.NewRequest().TakeChannel().release());
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "Failed to connect to " << filename;
          return;
        }

        dependency_injection->SetMemoryPressureProvider(
            context_->svc()->Connect<fuchsia::memorypressure::Provider>());
      });
  if (!gpu_dependency_injection_watcher_)
    return ZX_ERR_INTERNAL;
  return ZX_OK;
}
