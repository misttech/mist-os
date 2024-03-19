// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/drivers/msd-virtio-gpu/src/msd_virtio_device.h"

#include <lib/magma/util/macros.h>

MsdVirtioDevice::MsdVirtioDevice(VirtioGpuControl* control) : control_(control) {
  // Prevent unused warning-error
  (void)control_;
}

magma_status_t MsdVirtioDevice::GetIcdList(std::vector<msd::MsdIcdInfo>* icd_info_out) {
  MAGMA_DMESSAGE("MsdVirtioDevice::GetIcdList");

  icd_info_out->clear();
  icd_info_out->push_back({
      .component_url = "fuchsia-pkg://fuchsia.com/libvulkan_gfxstream#meta/vulkan.cm",
      .support_flags = msd::ICD_SUPPORT_FLAG_VULKAN,
  });

  return MAGMA_STATUS_OK;
}
