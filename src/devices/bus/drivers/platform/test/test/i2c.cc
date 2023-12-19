// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>

#include <memory>

#include <ddktl/device.h>

#define DRIVER_NAME "test-i2c"

namespace i2c {

class TestI2cDevice;
using DeviceType = ddk::Device<TestI2cDevice>;

class TestI2cDevice : public DeviceType,
                      public ddk::I2cImplProtocol<TestI2cDevice, ddk::base_protocol> {
 public:
  static zx_status_t Create(zx_device_t* parent);

  explicit TestI2cDevice(zx_device_t* parent) : DeviceType(parent) {}

  zx_status_t Create(std::unique_ptr<TestI2cDevice>* out);

  zx_status_t I2cImplGetMaxTransferSize(size_t* out_size);
  zx_status_t I2cImplSetBitrate(uint32_t bitrate);
  zx_status_t I2cImplTransact(const i2c_impl_op_t* op_list, size_t op_count);

  // Methods required by the ddk mixins
  void DdkRelease();
};

zx_status_t TestI2cDevice::Create(zx_device_t* parent) {
  pdev_protocol_t pdev;
  zx_status_t status = device_get_protocol(parent, ZX_PROTOCOL_PDEV, &pdev);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not get ZX_PROTOCOL_PDEV", __func__);
    return status;
  }

  pdev_device_info_t dev_info;
  status = pdev_get_device_info(&pdev, &dev_info);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_get_device_info failed: %s", zx_status_get_string(status));
    return status;
  }

  auto dev = std::make_unique<TestI2cDevice>(parent);

  zxlogf(INFO, "TestI2cDevice::Create: %s ", DRIVER_NAME);

  status = dev->DdkAdd(
      ddk::DeviceAddArgs("test-i2c").forward_metadata(parent, DEVICE_METADATA_I2C_CHANNELS));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAdd failed: %d", __func__, status);
    return status;
  }
  // devmgr is now in charge of dev.
  [[maybe_unused]] auto ptr = dev.release();

  return ZX_OK;
}

void TestI2cDevice::DdkRelease() { delete this; }

zx_status_t TestI2cDevice::I2cImplGetMaxTransferSize(size_t* out_size) {
  *out_size = 1024;
  return ZX_OK;
}

zx_status_t TestI2cDevice::I2cImplSetBitrate(uint32_t bitrate) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t TestI2cDevice::I2cImplTransact(const i2c_impl_op_t* op_list, size_t op_count) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t test_i2c_bind(void* ctx, zx_device_t* parent) { return TestI2cDevice::Create(parent); }

constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = test_i2c_bind;
  return driver_ops;
}();

}  // namespace i2c

ZIRCON_DRIVER(test_i2c, i2c::driver_ops, "zircon", "0.1");
