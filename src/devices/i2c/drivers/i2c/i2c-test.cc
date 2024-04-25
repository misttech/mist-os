// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/i2c/drivers/i2c/i2c-test-env.h"

namespace i2c {

namespace {

fuchsia_hardware_i2c_businfo::I2CChannel CreateChannel(uint32_t address, uint32_t i2c_class,
                                                       uint32_t vid, uint32_t pid, uint32_t did) {
  return fuchsia_hardware_i2c_businfo::I2CChannel{{
      .address = address,
      .i2c_class = i2c_class,
      .vid = vid,
      .pid = pid,
      .did = did,
  }};
}

fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata CreateMetadata(
    fidl::AnyArena& arena, std::vector<fuchsia_hardware_i2c_businfo::I2CChannel> channels,
    uint32_t bus_id) {
  return fidl::ToWire(arena, fuchsia_hardware_i2c_businfo::I2CBusMetadata{{
                                 .channels = channels,
                                 .bus_id = bus_id,
                             }});
}
}  // namespace

class I2cDriverTest : public fdf_testing::DriverTestFixture<TestConfig> {
 public:
  void Init(fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata metadata) {
    RunInEnvironmentTypeContext([metadata](TestEnvironment& env) { env.AddMetadata(metadata); });
    EXPECT_TRUE(StartDriver().is_ok());
  }
};

TEST_F(I2cDriverTest, OneChannel) {
  const fuchsia_hardware_i2c_businfo::I2CChannel kChannel = CreateChannel(5, 10, 2, 4, 6);
  const uint32_t kBusId = 32;

  fidl::Arena arena;
  Init(CreateMetadata(arena, {kChannel}, kBusId));

  RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    EXPECT_TRUE(node.children().count("i2c-32-5"));
  });
}

TEST_F(I2cDriverTest, NoMetadata) { EXPECT_TRUE(StartDriver().is_error()); }

TEST_F(I2cDriverTest, MultipleChannels) {
  const fuchsia_hardware_i2c_businfo::I2CChannel kChannel1 = CreateChannel(5, 10, 2, 4, 6);
  const fuchsia_hardware_i2c_businfo::I2CChannel kChannel2 = CreateChannel(15, 30, 3, 6, 9);
  const uint32_t kBusId = 32;

  fidl::Arena arena;
  Init(CreateMetadata(arena, {kChannel1, kChannel2}, kBusId));

  RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_EQ(2u, node.children().size());
    EXPECT_TRUE(node.children().count("i2c-32-5"));
    EXPECT_TRUE(node.children().count("i2c-32-15"));
  });
}

TEST_F(I2cDriverTest, GetName) {
  const std::string kTestChildName = "i2c-16-5";
  const std::string kChannelName = "xyz";
  fuchsia_hardware_i2c_businfo::I2CChannel channel = CreateChannel(5, 10, 2, 4, 6);
  channel.name(kChannelName);

  constexpr uint32_t kBusId = 16;

  fidl::Arena arena;
  Init(CreateMetadata(arena, {channel}, kBusId));

  RunInNodeContext([expected_name = kTestChildName](fdf_testing::TestNode& node) {
    EXPECT_EQ(1u, node.children().size());
    EXPECT_TRUE(node.children().count("i2c-16-5"));
  });

  zx::result connect_result = Connect<fuchsia_hardware_i2c::Service::Device>(kTestChildName);
  ASSERT_TRUE(connect_result.is_ok());
  zx::result run_result = RunInBackground(
      [client_end = std::move(connect_result.value()), expected_name = kChannelName]() mutable {
        auto name_result = fidl::WireCall(client_end)->GetName();
        ASSERT_OK(name_result.status());
        ASSERT_TRUE(name_result->is_ok());
        ASSERT_EQ(std::string(name_result->value()->name.get()), expected_name);
      });
  ASSERT_OK(run_result.status_value());
}

}  // namespace i2c
