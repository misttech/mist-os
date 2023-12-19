// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_AML_I2C_DFV1_DRIVER_H_
#define SRC_DEVICES_I2C_DRIVERS_AML_I2C_DFV1_DRIVER_H_

#include <ddktl/device.h>

#include "aml-i2c.h"

namespace aml_i2c {

class Dfv1Driver;
using DeviceType = ddk::Device<Dfv1Driver, ddk::GetProtocolable>;

class Dfv1Driver : public DeviceType {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  Dfv1Driver(zx_device_t* parent, std::unique_ptr<AmlI2c> aml_i2c);

  void DdkRelease() { delete this; }

  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  void SetTimeout(zx::duration timeout) { aml_i2c_->SetTimeout(timeout); }

 private:
  std::unique_ptr<AmlI2c> aml_i2c_;
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_DFV1_DRIVER_H_
