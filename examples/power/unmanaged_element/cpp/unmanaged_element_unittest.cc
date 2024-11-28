// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/power/unmanaged_element/cpp/unmanaged_element.h"

#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/power/cpp/testing/fake_current_level.h>
#include <lib/driver/power/cpp/testing/fake_topology.h>
#include <lib/driver/power/cpp/testing/fidl_bound_server.h>
#include <lib/driver/power/cpp/testing/scoped_background_loop.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/bridge.h>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

namespace {

using examples::power::UnmanagedElement;
using fdf_power::testing::FidlBoundServer;
using fdf_power::testing::ScopedBackgroundLoop;
using FakeCurrentLevel = FidlBoundServer<fdf_power::testing::FakeCurrentLevel>;
using FakeTopology = FidlBoundServer<fdf_power::testing::FakeTopology>;

using fuchsia_power_broker::BinaryPowerLevel;
using fuchsia_power_broker::ElementSchema;
using fuchsia_power_broker::PowerLevel;
using fuchsia_power_broker::Topology;

const std::vector<PowerLevel> kBinaryPowerLevels = {fidl::ToUnderlying(BinaryPowerLevel::kOff),
                                                    fidl::ToUnderlying(BinaryPowerLevel::kOn)};

class UnmanagedElementTest : public gtest::RealLoopFixture {};

TEST_F(UnmanagedElementTest, AddToPowerTopologySendsCorrectSchema) {
  ScopedBackgroundLoop background;

  // Create and run the Topology server in the background.
  auto [topology_client, topology_server] = fidl::Endpoints<Topology>::Create();
  FakeTopology topology(background.dispatcher(), std::move(topology_server));

  // Create an unmanaged element and add it to the power topology.
  const std::string element_name = "unmanaged-element";
  auto _element = UnmanagedElement::Add(topology_client, element_name, kBinaryPowerLevels,
                                        kBinaryPowerLevels[0]);

  // Check the ElementSchema received by the Topology server. Run this task on the foreground loop.
  async::Executor executor(dispatcher());
  executor.schedule_task(topology.TakeSchemaPromise().then(
      [quit = QuitLoopClosure(), &element_name](fpromise::result<ElementSchema, void>& result) {
        EXPECT_TRUE(result.is_ok());
        ElementSchema schema = result.take_value();
        ASSERT_EQ(schema.element_name(), element_name);
        ASSERT_EQ(schema.valid_levels(), kBinaryPowerLevels);
        ASSERT_EQ(schema.initial_current_level(), kBinaryPowerLevels[0]);
        quit();
      }));
  RunLoop();
}

TEST_F(UnmanagedElementTest, SetLevelChangesTopologyCurrentLevel) {
  ScopedBackgroundLoop background;

  // Create and run the Topology server in the background.
  auto [topology_client, topology_server] = fidl::Endpoints<Topology>::Create();
  FakeTopology topology(background.dispatcher(), std::move(topology_server));

  // Create an unmanaged element and add it to the power topology.
  const std::string element_name = "unmanaged-element";
  const PowerLevel initial_level = kBinaryPowerLevels[0];
  auto element =
      UnmanagedElement::Add(topology_client, element_name, kBinaryPowerLevels, initial_level);
  EXPECT_TRUE(element.is_ok());

  // Get the CurrentLevel server end from the ElementSchema received via Topology.
  async::Executor executor(dispatcher());
  ElementSchema schema;
  executor.schedule_task(topology.TakeSchemaPromise().then(
      [quit = QuitLoopClosure(), &schema](fpromise::result<ElementSchema, void>& result) {
        EXPECT_TRUE(result.is_ok());
        schema = result.take_value();
        quit();
      }));
  RunLoop();

  // Create a CurrentLevel server to handle Update requests in the background. DispatcherBound lets
  // us make safe async calls between the foreground (this test) and background dispatchers.
  async_patterns::TestDispatcherBound<FakeCurrentLevel> current_level_server(
      background.dispatcher(), std::in_place, async_patterns::PassDispatcher,
      std::move(schema.level_control_channels()->current()), initial_level);

  // Check that CurrentLevel is still set to the initial value.
  ASSERT_EQ(current_level_server.SyncCall(&FakeCurrentLevel::current_level), initial_level);

  // Verify CurrentLevel changes after calling SetLevel on the unmanaged element.
  element->SetLevel(kBinaryPowerLevels[1]);
  ASSERT_EQ(current_level_server.SyncCall(&FakeCurrentLevel::current_level), kBinaryPowerLevels[1]);
}

}  // namespace
