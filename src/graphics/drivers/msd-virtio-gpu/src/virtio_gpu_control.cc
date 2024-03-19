// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "virtio_gpu_control.h"

#include <lib/magma/platform/platform_logger.h>
#include <lib/magma/util/macros.h>

zx::result<> VirtioGpuControlFidl::Init(std::shared_ptr<fdf::Namespace> incoming) {
  auto control = incoming->Connect<fuchsia_gpu_virtio::Service::Control>();
  if (control.is_error()) {
    MAGMA_LOG(ERROR, "Error requesting virtio gpu control service: %s", control.status_string());
    return control.take_error();
  }

  control_ = fidl::WireSyncClient(std::move(*control));

  MAGMA_DMESSAGE("VirtioGpuControlFidl::Init success");

  return zx::ok();
}
