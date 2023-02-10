// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/async/dispatcher.h>

#include <ddktl/device.h>

namespace i2c {

class I2cDevice;
using I2cDeviceType = ddk::Device<I2cDevice>;

class I2cDevice : public I2cDeviceType {
 public:
  I2cDevice(zx_device_t* parent,
            ddk::DecodedMetadata<fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata> metadata)
      : I2cDeviceType(parent), metadata_(std::move(metadata)) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);
  static zx_status_t Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher);

  void DdkRelease() { delete this; }

 private:
  zx_status_t Init(const ddk::I2cImplProtocolClient& i2c, uint32_t bus_base, uint32_t bus_count,
                   async_dispatcher_t* dispatcher);

  // Retain ownership of the metadata so we can pass references to children.
  ddk::DecodedMetadata<fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata> metadata_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_
