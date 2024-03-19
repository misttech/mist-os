// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef MSD_VIRTIO_CONTROL_SHIM_H
#define MSD_VIRTIO_CONTROL_SHIM_H

#include <fidl/fuchsia.gpu.virtio/cpp/wire.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/magma_service/msd.h>

struct msd::DeviceHandle {};

class VirtioGpuControl : public msd::DeviceHandle {
 public:
  virtual ~VirtioGpuControl() = default;
};

class VirtioGpuControlFidl : public VirtioGpuControl {
 public:
  zx::result<> Init(std::shared_ptr<fdf::Namespace> incoming);

 private:
  fidl::WireSyncClient<fuchsia_gpu_virtio::GpuControl> control_;
};

#endif  // MSD_VIRTIO_CONTROL_SHIM_H
