// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/exceptions/handler/wake_lease.h"

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/promise.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include <memory>
#include <string>
#include <utility>

#include <gtest/gtest.h>

#include "src/developer/forensics/testing/stubs/power_broker_topology.h"
#include "src/developer/forensics/testing/stubs/system_activity_governor.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/developer/forensics/utils/errors.h"

namespace forensics::exceptions::handler {
namespace {

using ::testing::HasSubstr;

namespace fpb = fuchsia_power_broker;
namespace fps = fuchsia_power_system;

constexpr uint8_t kPowerLevelActive = 1u;

class WakeLeaseTest : public UnitTestFixture {
 protected:
  WakeLeaseTest() : executor_(dispatcher()) {}

  async::Executor& GetExecutor() { return executor_; }

 private:
  async::Executor executor_;
};

// TODO(https://fxbug.dev/341104129): Create helper method for creating stubs.
TEST_F(WakeLeaseTest, AddsElementSuccessfully) {
  auto sag_endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
  ASSERT_TRUE(sag_endpoints.is_ok());

  auto sag = std::make_unique<stubs::SystemActivityGovernor>(std::move(sag_endpoints->server),
                                                             dispatcher());

  auto topology_endpoints = fidl::CreateEndpoints<fpb::Topology>();
  ASSERT_TRUE(topology_endpoints.is_ok());

  auto topology = std::make_unique<stubs::PowerBrokerTopology>(
      std::move(topology_endpoints->server), dispatcher());

  WakeLease wake_lease(dispatcher(), std::move(sag_endpoints->client),
                       std::move(topology_endpoints->client));

  bool add_element_returned = false;
  GetExecutor().schedule_task(
      wake_lease.AddPowerElement("exceptions-element-001")
          .and_then([&add_element_returned]() { add_element_returned = true; })
          .or_else([](const Error& error) {
            FX_LOGS(FATAL) << "Unexpected error while adding power element: " << ToString(error);
          }));

  RunLoopUntilIdle();
  ASSERT_TRUE(add_element_returned);
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));

  ASSERT_EQ(topology->Dependencies("exceptions-element-001").size(), 1u);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001").front().dependency_type(),
            fpb::DependencyType::kPassive);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001").front().dependent_level(),
            kPowerLevelActive);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001").front().requires_level(),
            fidl::ToUnderlying(fps::ExecutionStateLevel::kWakeHandling));
}

TEST_F(WakeLeaseTest, GetPowerElementsFails) {
  auto sag_endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
  ASSERT_TRUE(sag_endpoints.is_ok());

  auto sag = std::make_unique<stubs::SystemActivityGovernorClosesConnection>(
      std::move(sag_endpoints->server), dispatcher());

  auto topology_endpoints = fidl::CreateEndpoints<fpb::Topology>();
  ASSERT_TRUE(topology_endpoints.is_ok());

  auto topology = std::make_unique<stubs::PowerBrokerTopology>(
      std::move(topology_endpoints->server), dispatcher());

  WakeLease wake_lease(dispatcher(), std::move(sag_endpoints->client),
                       std::move(topology_endpoints->client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.AddPowerElement("exceptions-element-001")
          .and_then([]() { FX_LOGS(FATAL) << "Unexpected success while adding power element"; })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, GetPowerElementsNoSagPowerElements) {
  auto sag_endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
  ASSERT_TRUE(sag_endpoints.is_ok());

  auto sag = std::make_unique<stubs::SystemActivityGovernorNoPowerElements>(
      std::move(sag_endpoints->server), dispatcher());

  auto topology_endpoints = fidl::CreateEndpoints<fpb::Topology>();
  ASSERT_TRUE(topology_endpoints.is_ok());

  auto topology = std::make_unique<stubs::PowerBrokerTopology>(
      std::move(topology_endpoints->server), dispatcher());

  WakeLease wake_lease(dispatcher(), std::move(sag_endpoints->client),
                       std::move(topology_endpoints->client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.AddPowerElement("exceptions-element-001")
          .and_then([]() { FX_LOGS(FATAL) << "Unexpected success while adding power element"; })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, GetPowerElementsNoTokens) {
  auto sag_endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
  ASSERT_TRUE(sag_endpoints.is_ok());

  auto sag = std::make_unique<stubs::SystemActivityGovernorNoTokens>(
      std::move(sag_endpoints->server), dispatcher());

  auto topology_endpoints = fidl::CreateEndpoints<fpb::Topology>();
  ASSERT_TRUE(topology_endpoints.is_ok());

  auto topology = std::make_unique<stubs::PowerBrokerTopology>(
      std::move(topology_endpoints->server), dispatcher());

  WakeLease wake_lease(dispatcher(), std::move(sag_endpoints->client),
                       std::move(topology_endpoints->client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.AddPowerElement("exceptions-element-001")
          .and_then([]() { FX_LOGS(FATAL) << "Unexpected success while adding power element"; })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, AddElementFails) {
  auto sag_endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
  ASSERT_TRUE(sag_endpoints.is_ok());

  auto sag = std::make_unique<stubs::SystemActivityGovernor>(std::move(sag_endpoints->server),
                                                             dispatcher());

  auto topology_endpoints = fidl::CreateEndpoints<fpb::Topology>();
  ASSERT_TRUE(topology_endpoints.is_ok());

  auto topology = std::make_unique<stubs::PowerBrokerTopologyClosesConnection>(
      std::move(topology_endpoints->server), dispatcher());

  WakeLease wake_lease(dispatcher(), std::move(sag_endpoints->client),
                       std::move(topology_endpoints->client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.AddPowerElement("exceptions-element-001")
          .and_then([]() { FX_LOGS(FATAL) << "Unexpected success while adding power element"; })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
}

using WakeLeaseDeathTest = WakeLeaseTest;

TEST_F(WakeLeaseDeathTest, AddElementDiesIfCalledTwice) {
  auto sag_endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
  ASSERT_TRUE(sag_endpoints.is_ok());

  auto sag = std::make_unique<stubs::SystemActivityGovernor>(std::move(sag_endpoints->server),
                                                             dispatcher());

  auto topology_endpoints = fidl::CreateEndpoints<fpb::Topology>();
  ASSERT_TRUE(topology_endpoints.is_ok());

  auto topology = std::make_unique<stubs::PowerBrokerTopology>(
      std::move(topology_endpoints->server), dispatcher());

  WakeLease wake_lease(dispatcher(), std::move(sag_endpoints->client),
                       std::move(topology_endpoints->client));

  GetExecutor().schedule_task(wake_lease.AddPowerElement("exceptions-element-001"));
  RunLoopUntilIdle();

  ASSERT_DEATH(wake_lease.AddPowerElement("exceptions-element-002"), HasSubstr("Check failed"));
}

}  // namespace
}  // namespace forensics::exceptions::handler
