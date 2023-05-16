// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/fusb302.h"

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>
#include <lib/mock-i2c/mock-i2c.h>
#include <lib/zx/interrupt.h>
#include <zircon/types.h>

#include <cstdint>
#include <optional>
#include <utility>

#include <zxtest/zxtest.h>

#include "src/devices/power/drivers/fusb302/pd-sink-state-machine.h"
#include "src/devices/power/drivers/fusb302/typec-port-state-machine.h"
#include "src/devices/power/drivers/fusb302/usb-pd-defs.h"

namespace fusb302 {

namespace {

class Fusb302Test : public inspect::InspectTestHelper, public zxtest::Test {
 public:
  void SetUp() override {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_i2c::Device>();
    EXPECT_TRUE(endpoints.is_ok());
    fidl::ClientEnd<fuchsia_hardware_i2c::Device> mock_i2c_client = std::move(endpoints->client);

    EXPECT_OK(loop_.StartThread());
    fidl::BindServer<fuchsia_hardware_i2c::Device>(loop_.dispatcher(), std::move(endpoints->server),
                                                   &mock_i2c_);
    zx::interrupt gpio_interrupt;
    ASSERT_OK(
        zx::interrupt::create(zx::resource(), /*vector=*/0, ZX_INTERRUPT_VIRTUAL, &gpio_interrupt));
    device_.emplace(nullptr, std::move(mock_i2c_client), std::move(gpio_interrupt));
  }

  void ExpectInspectPropertyEquals(const char* node_name, const char* property_name,
                                   int64_t expected_value) {
    ASSERT_NO_FATAL_FAILURE(ReadInspect(device_->InspectorForTesting().DuplicateVmo()));
    auto* node_root = hierarchy().GetByPath({node_name});
    ASSERT_TRUE(node_root);
    CheckProperty(node_root->node(), property_name, inspect::IntPropertyValue(expected_value));
  }

 protected:
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  mock_i2c::MockI2c mock_i2c_;
  std::optional<Fusb302> device_;
};

TEST_F(Fusb302Test, ConstructorInspectState) {
  ExpectInspectPropertyEquals("Sensors", "CCTermination",
                              static_cast<int64_t>(usb_pd::ConfigChannelTermination::kUnknown));
  ExpectInspectPropertyEquals("PortStateMachine", "State",
                              static_cast<int64_t>(TypeCPortState::kSinkUnattached));
  ExpectInspectPropertyEquals("SinkPolicyEngineStateMachine", "State",
                              static_cast<int64_t>(SinkPolicyEngineState::kStartup));
}

}  // namespace

}  // namespace fusb302
