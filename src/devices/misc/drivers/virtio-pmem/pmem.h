// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_MISC_DRIVERS_VIRTIO_PMEM_PMEM_H_
#define SRC_DEVICES_MISC_DRIVERS_VIRTIO_PMEM_PMEM_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/virtio/backends/backend.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <stdlib.h>
#include <zircon/compiler.h>

#include <memory>

namespace virtio {

class PmemDevice final : public virtio::Device {
 public:
  PmemDevice(zx::bti bti, std::unique_ptr<Backend> backend, zx::resource mmio_resource);
  ~PmemDevice() final;

  // virtio::Device overrides.
  zx_status_t Init() final;

  void IrqRingUpdate() final;
  void IrqConfigChange() final;
  const char* tag() const final { return "virtio-pmem"; }

 private:
  // VMO representing the shared buffer.
  zx::vmo phys_vmo_;

  zx::resource mmio_resource_;
};

class PmemDriver final : public fdf::DriverBase {
 public:
  static constexpr char kDriverName[] = "virtio-pmem";

  PmemDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher);
  ~PmemDriver() final = default;

  zx::result<> Start() final;

 private:
  zx::result<std::unique_ptr<PmemDevice>> CreatePmemDevice();

  std::unique_ptr<PmemDevice> device_;
};

}  // namespace virtio

#endif  // SRC_DEVICES_MISC_DRIVERS_VIRTIO_PMEM_PMEM_H_
