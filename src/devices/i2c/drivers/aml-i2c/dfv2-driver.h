// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_AML_I2C_DFV2_DRIVER_H_
#define SRC_DEVICES_I2C_DRIVERS_AML_I2C_DFV2_DRIVER_H_

#include <lib/ddk/metadata.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>

#include "aml-i2c.h"

namespace aml_i2c {

class Dfv2Driver : public fdf::DriverBase {
 public:
  Dfv2Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

  void SetTimeout(zx::duration timeout) { aml_i2c_->SetTimeout(timeout); }

 private:
  compat::DeviceServer::BanjoConfig CreateBanjoConfig();
  zx_status_t CreateAmlI2cChildNode();

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> child_controller_;
  std::unique_ptr<AmlI2c> aml_i2c_;
  compat::DeviceServer device_server_;
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_DFV2_DRIVER_H_
