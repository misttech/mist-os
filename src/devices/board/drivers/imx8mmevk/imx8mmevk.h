// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_IMX8MMEVK_IMX8MMEVK_H_
#define SRC_DEVICES_BOARD_DRIVERS_IMX8MMEVK_IMX8MMEVK_H_

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fuchsia/hardware/gpioimpl/cpp/banjo.h>
#include <threads.h>

#include <ddktl/device.h>

namespace imx8mm_evk {

class Imx8mmEvk : public ddk::Device<Imx8mmEvk> {
 public:
  Imx8mmEvk(zx_device_t* parent, fdf::ClientEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus,
            fuchsia_hardware_platform_bus::TemporaryBoardInfo board_info)
      : ddk::Device<Imx8mmEvk>(parent),
        pbus_(std::move(pbus)),
        board_info_(std::move(board_info)) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  void DdkRelease() { delete this; }

 private:
  zx_status_t Start();
  int Thread();

  zx_status_t GpioInit();
  zx_status_t I2cInit();

  const fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> pbus_;
  const fuchsia_hardware_platform_bus::TemporaryBoardInfo board_info_;
  thrd_t thread_;
};

}  // namespace imx8mm_evk

#endif  // SRC_DEVICES_BOARD_DRIVERS_IMX8MMEVK_IMX8MMEVK_H_
