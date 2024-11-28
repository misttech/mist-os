// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_TEST_ENV_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_TEST_ENV_H_

#include <fidl/fuchsia.scheduler/cpp/wire_test_base.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include "src/devices/i2c/drivers/i2c/fake-i2c-impl.h"
#include "src/devices/i2c/drivers/i2c/i2c.h"
#include "src/lib/testing/predicates/status.h"

namespace i2c {

class TestEnvironment : public fdf_testing::Environment {
 public:
  TestEnvironment() : i2c_impl_(1024) {}

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    // Set up and add the compat device server.
    device_server_.Initialize("default");
    ZX_ASSERT(device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   &to_driver_vfs) == ZX_OK);

    // Add the i2c service.
    ZX_ASSERT(to_driver_vfs
                  .AddService<fuchsia_hardware_i2cimpl::Service>(i2c_impl_.CreateInstanceHandler())
                  .is_ok());
    return zx::ok();
  }

  void AddMetadata(fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata metadata) {
    fit::result persisted = fidl::Persist(metadata);
    ZX_ASSERT(persisted.is_ok());
    ZX_ASSERT(device_server_.AddMetadata(DEVICE_METADATA_I2C_CHANNELS, persisted->data(),
                                         persisted->size()) == ZX_OK);
  }

  FakeI2cImpl& i2c_impl() { return i2c_impl_; }

 private:
  compat::DeviceServer device_server_;
  FakeI2cImpl i2c_impl_;
};

class TestConfig final {
 public:
  using DriverType = I2cDriver;
  using EnvironmentType = TestEnvironment;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_TEST_ENV_H_
