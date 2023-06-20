// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_system_semaphore.h"

#include "magma_util/macros.h"

namespace msd {
MagmaSystemSemaphore::MagmaSystemSemaphore(
    std::unique_ptr<magma::PlatformSemaphore> platform_semaphore,
    std::unique_ptr<msd::Semaphore> msd_semaphore_t)
    : platform_semaphore_(std::move(platform_semaphore)),
      msd_semaphore_(std::move(msd_semaphore_t)) {}

std::unique_ptr<MagmaSystemSemaphore> MagmaSystemSemaphore::Create(
    msd::Driver* driver, std::unique_ptr<magma::PlatformSemaphore> platform_semaphore) {
  if (!platform_semaphore)
    return MAGMA_DRETP(nullptr, "null platform semaphore");

  uint32_t handle;
  if (!platform_semaphore->duplicate_handle(&handle))
    return MAGMA_DRETP(nullptr, "failed to get duplicate handle");

  std::unique_ptr<msd::Semaphore> msd_semaphore;
  magma_status_t status =
      driver->ImportSemaphore(zx::event(handle), platform_semaphore->id(), &msd_semaphore);

  if (status != MAGMA_STATUS_OK)
    return MAGMA_DRETP(nullptr, "msd_semaphore_import failed: %d", status);

  return std::unique_ptr<MagmaSystemSemaphore>(
      new MagmaSystemSemaphore(std::move(platform_semaphore), std::move(msd_semaphore)));
}

}  // namespace msd
