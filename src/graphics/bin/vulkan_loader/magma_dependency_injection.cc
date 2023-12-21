// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/bin/vulkan_loader/magma_dependency_injection.h"

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

zx::result<MagmaDependencyInjection> MagmaDependencyInjection::Create(
    fidl::ClientEnd<fuchsia_memorypressure::Provider> provider) {
  std::unique_ptr watcher = fsl::DeviceWatcher::Create(
      "/dev/class/gpu-dependency-injection",
      [provider = std::move(provider)](const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                                       const std::string& filename) mutable {
        if (filename == ".") {
          return;
        }
        auto endpoints = fidl::CreateEndpoints<fuchsia_gpu_magma::DependencyInjection>();
        if (endpoints.is_error()) {
          FX_LOGS(ERROR) << "Failed to create endpoints: " << endpoints.status_string();
          return;
        }
        if (auto result = component::ConnectAt(dir, std::move(endpoints->server), filename);
            result.is_error()) {
          FX_LOGS(ERROR) << "Failed to connect to " << filename << ": " << result.status_string();
          return;
        }

        if (auto result =
                fidl::WireCall(endpoints->client)->SetMemoryPressureProvider(std::move(provider));
            !result.ok()) {
          FX_LOGS(ERROR) << "Failed to set memory pressure provider: " << result.status_string();
          return;
        }
      });
  if (!watcher) {
    FX_LOGS(ERROR) << "Failed to create device watcher!";
    return zx::error(ZX_ERR_INTERNAL);
  }
  return zx::ok(MagmaDependencyInjection(std::move(watcher)));
}
