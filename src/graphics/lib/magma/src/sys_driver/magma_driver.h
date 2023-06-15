// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_DRIVER_H_
#define SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_DRIVER_H_

#include <lib/fit/function.h>

#include "magma_system_device.h"
#include "magma_util/dlog.h"
#include "magma_util/macros.h"
#include "msd_cc.h"

class MagmaDriver {
 public:
  explicit MagmaDriver(std::unique_ptr<msd::Driver> msd_drv) : msd_drv_(std::move(msd_drv)) {}

  std::unique_ptr<MagmaSystemDevice> CreateDevice(msd::DeviceHandle* device_handle) {
    std::unique_ptr<msd::Device> msd_dev = msd_drv_->CreateDevice(device_handle);
    if (!msd_dev)
      return MAGMA_DRETP(nullptr, "msd_create_device failed");

    return MagmaSystemDevice::Create(msd_drv_.get(), std::move(msd_dev));
  }

  static std::unique_ptr<MagmaDriver> Create() {
    std::unique_ptr<msd::Driver> msd_drv = msd::Driver::Create();
    if (!msd_drv)
      return MAGMA_DRETP(nullptr, "msd_create returned null");

    return std::make_unique<MagmaDriver>(std::move(msd_drv));
  }

  std::optional<inspect::Inspector> DuplicateInspector() { return msd_drv_->DuplicateInspector(); }

 private:
  std::unique_ptr<msd::Driver> msd_drv_;
};

#endif  // MAGMA_DRIVER_H
