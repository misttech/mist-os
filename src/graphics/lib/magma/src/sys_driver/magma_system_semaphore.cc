// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_system_semaphore.h"

#include "magma_util/macros.h"
#include "platform_object.h"

namespace msd {
MagmaSystemSemaphore::MagmaSystemSemaphore(uint64_t global_id,
                                           std::unique_ptr<msd::Semaphore> msd_semaphore_t)
    : global_id_(global_id), msd_semaphore_(std::move(msd_semaphore_t)) {}

std::unique_ptr<MagmaSystemSemaphore> MagmaSystemSemaphore::Create(msd::Driver* driver,
                                                                   zx::event event,
                                                                   uint64_t client_id,
                                                                   uint64_t flags) {
  uint64_t global_id = 0;
  if (!magma::PlatformObject::IdFromHandle(event.get(), &global_id))
    return MAGMA_DRETP(nullptr, "couldn't get global id");

  std::unique_ptr<msd::Semaphore> msd_semaphore;
  magma_status_t status =
      driver->ImportSemaphore(std::move(event), client_id, flags, &msd_semaphore);

  if (status != MAGMA_STATUS_OK)
    return MAGMA_DRETP(nullptr, "ImportSemaphore failed: %d", status);

  return std::unique_ptr<MagmaSystemSemaphore>(
      new MagmaSystemSemaphore(global_id, std::move(msd_semaphore)));
}

}  // namespace msd
