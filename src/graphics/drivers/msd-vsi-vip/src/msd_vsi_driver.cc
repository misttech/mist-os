// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "msd_vsi_driver.h"

#include "magma_util/short_macros.h"
#include "msd.h"
#include "msd_vsi_device.h"
#include "src/graphics/drivers/msd-vsi-vip/src/msd_vsi_semaphore.h"

void MsdVsiDriver::Destroy(MsdVsiDriver* drv) { delete drv; }

std::unique_ptr<msd::Device> MsdVsiDriver::CreateDevice(msd::DeviceHandle* device_handle) {
  bool start_device_thread = (configure_flags() & MSD_DRIVER_CONFIG_TEST_NO_DEVICE_THREAD) == 0;

  std::unique_ptr<MsdVsiDevice> device = MsdVsiDevice::Create(device_handle, start_device_thread);
  if (!device) {
    return DRETP(nullptr, "Failed to create device");
  }

  return device;
}

std::unique_ptr<msd::Buffer> MsdVsiDriver::ImportBuffer(zx::vmo vmo, uint64_t client_id) {
  auto buffer = MsdVsiBuffer::Import(std::move(vmo), client_id);

  if (!buffer) {
    return DRETP(nullptr, "Failed to import buffer");
  }

  return std::make_unique<MsdVsiAbiBuffer>(std::move(buffer));
}

magma_status_t MsdVsiDriver::ImportSemaphore(zx::event handle, uint64_t client_id, uint64_t flags,
                                             std::unique_ptr<msd::Semaphore>* semaphore_out) {
  auto semaphore = magma::PlatformSemaphore::Import(std::move(handle), flags);
  if (!semaphore)
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "couldn't import semaphore handle");

  semaphore->set_local_id(client_id);

  *semaphore_out = std::make_unique<MsdVsiAbiSemaphore>(
      std::shared_ptr<magma::PlatformSemaphore>(std::move(semaphore)));

  return MAGMA_STATUS_OK;
}

// static
std::unique_ptr<msd::Driver> msd::Driver::Create() { return std::make_unique<MsdVsiDriver>(); }
