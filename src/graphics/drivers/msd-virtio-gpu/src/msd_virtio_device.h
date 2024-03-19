// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef MSD_VIRTIO_DEVICE_H
#define MSD_VIRTIO_DEVICE_H

#include <lib/magma_service/msd.h>
#include <lib/zx/vmo.h>

#include <memory>

#include "virtio_gpu_control.h"

class MsdVirtioDevice : public msd::Device {
 public:
  explicit MsdVirtioDevice(VirtioGpuControl* control);

  magma_status_t GetIcdList(std::vector<msd::MsdIcdInfo>* icd_info_out) override;

 private:
  // Unowned.
  VirtioGpuControl* control_;
};

#endif  // MSD_VIRTIO_DEVICE_H
