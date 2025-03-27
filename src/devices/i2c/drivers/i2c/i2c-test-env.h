// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_TEST_ENV_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_TEST_ENV_H_

#include <fidl/fuchsia.scheduler/cpp/wire_test_base.h>
#include <lib/driver/metadata/cpp/metadata_server.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include "src/devices/i2c/drivers/i2c/fake-i2c-impl.h"
#include "src/devices/i2c/drivers/i2c/i2c.h"
#include "src/lib/testing/predicates/status.h"

namespace i2c {

class TestEnvironment : public fdf_testing::Environment {
 public:
  TestEnvironment() : i2c_impl_(1024) {}

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    if (zx::result result = metadata_server_.Serve(
            to_driver_vfs, fdf::Dispatcher::GetCurrent()->async_dispatcher());
        result.is_error()) {
      return result.take_error();
    }

    // Add the i2c service.
    if (zx::result result = to_driver_vfs.AddService<fuchsia_hardware_i2cimpl::Service>(
            i2c_impl_.CreateInstanceHandler());
        result.is_error()) {
      return result.take_error();
    }
    return zx::ok();
  }

  void AddMetadata(const fuchsia_hardware_i2c_businfo::I2CBusMetadata& metadata) {
    ZX_ASSERT(metadata_server_.SetMetadata(metadata).is_ok());
  }

  FakeI2cImpl& i2c_impl() { return i2c_impl_; }

 private:
  fdf_metadata::MetadataServer<fuchsia_hardware_i2c_businfo::I2CBusMetadata> metadata_server_;
  FakeI2cImpl i2c_impl_;
};

class TestConfig final {
 public:
  using DriverType = I2cDriver;
  using EnvironmentType = TestEnvironment;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_TEST_ENV_H_
