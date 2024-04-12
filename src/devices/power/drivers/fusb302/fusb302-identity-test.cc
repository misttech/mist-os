// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/fusb302-identity.h"

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>
#include <lib/mock-i2c/mock-i2c.h>

#include <optional>

#include <zxtest/zxtest.h>

namespace fusb302 {

namespace {

// Register addresses from Table 16 "Register Definitions" on page 18 of the
// Rev 5 datasheet.
constexpr int kDeviceIdAddress = 0x01;

class Fusb302IdentityTest : public inspect::InspectTestHelper, public zxtest::Test {
 public:
  void SetUp() override {
    fdf::Logger::SetGlobalInstance(&logger_);

    auto endpoints = fidl::Endpoints<fuchsia_hardware_i2c::Device>::Create();
    mock_i2c_client_ = std::move(endpoints.client);

    EXPECT_OK(loop_.StartThread());
    fidl::BindServer<fuchsia_hardware_i2c::Device>(loop_.dispatcher(), std::move(endpoints.server),
                                                   &mock_i2c_);

    identity_.emplace(mock_i2c_client_, inspect_.GetRoot().CreateChild("Identity"));
  }

  void TearDown() override { fdf::Logger::SetGlobalInstance(nullptr); }

  void ExpectInspectPropertyEquals(const char* property_name, const std::string& expected_value) {
    ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_.DuplicateVmo()));
    auto* identity_root = hierarchy().GetByPath({"Identity"});
    ASSERT_TRUE(identity_root);
    CheckProperty(identity_root->node(), property_name,
                  inspect::StringPropertyValue(expected_value));
  }

 protected:
  inspect::Inspector inspect_;

  fdf::Logger logger_{"fusb302-identity-test", FUCHSIA_LOG_DEBUG, zx::socket{},
                      fidl::WireClient<fuchsia_logger::LogSink>()};

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  mock_i2c::MockI2c mock_i2c_;
  fidl::ClientEnd<fuchsia_hardware_i2c::Device> mock_i2c_client_;
  std::optional<Fusb302Identity> identity_;
};

TEST_F(Fusb302IdentityTest, Vim3Identity) {
  mock_i2c_.ExpectWrite({kDeviceIdAddress}).ExpectReadStop({0x91});

  EXPECT_OK(identity_->ReadIdentity());

  ExpectInspectPropertyEquals("Product", "FUSB302BMPX");
  ExpectInspectPropertyEquals("Version", "FUSB302B_revB");
}

}  // namespace

}  // namespace fusb302
