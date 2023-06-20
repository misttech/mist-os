// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_SYSTEM_SEMAPHORE_H_
#define SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_SYSTEM_SEMAPHORE_H_

#include <memory>

#include "msd_cc.h"
#include "platform_semaphore.h"

namespace msd {
class MagmaSystemSemaphore {
 public:
  static std::unique_ptr<MagmaSystemSemaphore> Create(
      msd::Driver* device, std::unique_ptr<magma::PlatformSemaphore> platform_semaphore);

  magma::PlatformSemaphore* platform_semaphore() { return platform_semaphore_.get(); }

  msd::Semaphore* msd_semaphore() { return msd_semaphore_.get(); }

 private:
  MagmaSystemSemaphore(std::unique_ptr<magma::PlatformSemaphore> platform_semaphore,
                       std::unique_ptr<msd::Semaphore> msd_semaphore_t);
  std::unique_ptr<magma::PlatformSemaphore> platform_semaphore_;
  std::unique_ptr<msd::Semaphore> msd_semaphore_;
};

}  // namespace msd

#endif  // SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_SYSTEM_SEMAPHORE_H_
