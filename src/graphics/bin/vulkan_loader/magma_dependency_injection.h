// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_BIN_VULKAN_LOADER_MAGMA_DEPENDENCY_INJECTION_H_
#define SRC_GRAPHICS_BIN_VULKAN_LOADER_MAGMA_DEPENDENCY_INJECTION_H_

#include <fidl/fuchsia.memorypressure/cpp/wire.h>

#include "src/lib/fsl/io/device_watcher.h"

class MagmaDependencyInjection {
 public:
  static zx::result<MagmaDependencyInjection> Create(
      fit::function<zx::result<fidl::ClientEnd<fuchsia_memorypressure::Provider>>()>
          provider_factory);

 private:
  explicit MagmaDependencyInjection(std::unique_ptr<fsl::DeviceWatcher> watcher)
      : watcher_(std::move(watcher)) {}
  std::unique_ptr<fsl::DeviceWatcher> watcher_;
};

#endif  // SRC_GRAPHICS_BIN_VULKAN_LOADER_MAGMA_DEPENDENCY_INJECTION_H_
