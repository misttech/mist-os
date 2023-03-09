// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/registers.h"

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/mock-i2c/mock-i2c.h>
#include <lib/zx/result.h>

#include <utility>

#include <zxtest/zxtest.h>

namespace fusb302 {

namespace {

class Fusb302RegisterTest : public zxtest::Test {
 public:
  void SetUp() override {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_i2c::Device>();
    EXPECT_TRUE(endpoints.is_ok());
    mock_i2c_client_ = std::move(endpoints->client);

    EXPECT_OK(loop_.StartThread());
    fidl::BindServer<fuchsia_hardware_i2c::Device>(loop_.dispatcher(), std::move(endpoints->server),
                                                   &mock_i2c_);
  }

 protected:
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  mock_i2c::MockI2c mock_i2c_;
  fidl::ClientEnd<fuchsia_hardware_i2c::Device> mock_i2c_client_;
};

class ReadModifyWriteTestReg : public Fusb302Register<ReadModifyWriteTestReg> {
 public:
  DEF_FIELD(3, 0, documented_field);

  static auto Get() { return hwreg::I2cRegisterAddr<ReadModifyWriteTestReg>(0x42); }
};

TEST_F(Fusb302RegisterTest, ReadModifyWriteReportsOldValue) {
  mock_i2c_.ExpectWrite({0x42}).ExpectReadStop({0xf5});

  bool mutator_called = false;
  uint8_t old_documented_field_value = 0xff;
  zx::result<> read_modify_write_result = ReadModifyWriteTestReg::ReadModifyWrite(
      mock_i2c_client_, [&](ReadModifyWriteTestReg& test_register) {
        mutator_called = true;
        old_documented_field_value = test_register.documented_field();
      });
  EXPECT_OK(read_modify_write_result);
  EXPECT_TRUE(mutator_called);
  EXPECT_EQ(0x05, old_documented_field_value);
}

TEST_F(Fusb302RegisterTest, ReadModifyWriteNoChange) {
  mock_i2c_.ExpectWrite({0x42}).ExpectReadStop({0xf5});

  bool mutator_called = false;
  zx::result<> read_modify_write_result = ReadModifyWriteTestReg::ReadModifyWrite(
      mock_i2c_client_, [&](ReadModifyWriteTestReg& test_register) { mutator_called = true; });
  EXPECT_OK(read_modify_write_result);
  EXPECT_TRUE(mutator_called);
}

TEST_F(Fusb302RegisterTest, ReadModifyWriteChange) {
  mock_i2c_.ExpectWrite({0x42}).ExpectReadStop({0xf5});
  mock_i2c_.ExpectWriteStop({0x42, 0xfc});

  bool mutator_called = false;
  zx::result<> read_modify_write_result = ReadModifyWriteTestReg::ReadModifyWrite(
      mock_i2c_client_, [&](ReadModifyWriteTestReg& test_register) {
        mutator_called = true;
        test_register.set_documented_field(0x0c);
      });
  EXPECT_OK(read_modify_write_result);
  EXPECT_TRUE(mutator_called);
}

}  // namespace

}  // namespace fusb302
