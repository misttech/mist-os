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

#include "src/developer/forensics/exceptions/constants.h"
#include "src/developer/forensics/testing/stubs/power_broker_topology.h"
#include "src/developer/forensics/testing/stubs/system_activity_governor.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/developer/forensics/utils/errors.h"

namespace forensics::exceptions::handler {
namespace {

using ::testing::HasSubstr;

namespace fpb = fuchsia_power_broker;
namespace fps = fuchsia_power_system;

class WakeLeaseTest : public UnitTestFixture {
 protected:
  WakeLeaseTest() : executor_(dispatcher()) {}

  async::Executor& GetExecutor() { return executor_; }

  template <typename Impl>
  static std::optional<std::tuple<fidl::ClientEnd<fps::ActivityGovernor>, std::unique_ptr<Impl>>>
  CreateSag(async_dispatcher_t* dispatcher) {
    auto endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
    if (!endpoints.is_ok()) {
      return std::nullopt;
    }

    auto stub = std::make_unique<Impl>(std::move(endpoints->server), dispatcher);
    return std::make_tuple(std::move(endpoints->client), std::move(stub));
  }

  template <typename Impl>
  static std::optional<std::tuple<fidl::ClientEnd<fpb::Topology>, std::unique_ptr<Impl>>>
  CreateTopology(async_dispatcher_t* dispatcher) {
    auto endpoints = fidl::CreateEndpoints<fpb::Topology>();
    if (!endpoints.is_ok()) {
      return std::nullopt;
    }

    auto stub = std::make_unique<Impl>(std::move(endpoints->server), dispatcher);
    return std::make_tuple(std::move(endpoints->client), std::move(stub));
  }

 private:
  async::Executor executor_;
};

TEST_F(WakeLeaseTest, AddsElementSuccessfully) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(dispatcher());
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), std::move(sag_client), std::move(topology_client));

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
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernorClosesConnection>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(dispatcher());
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), std::move(sag_client), std::move(topology_client));

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
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernorNoPowerElements>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(dispatcher());
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), std::move(sag_client), std::move(topology_client));

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
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernorNoTokens>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(dispatcher());
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), std::move(sag_client), std::move(topology_client));

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
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub =
      CreateTopology<stubs::PowerBrokerTopologyClosesConnection>(dispatcher());
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), std::move(sag_client), std::move(topology_client));

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
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(dispatcher());
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), std::move(sag_client), std::move(topology_client));

  GetExecutor().schedule_task(wake_lease.AddPowerElement("exceptions-element-001"));
  RunLoopUntilIdle();

  ASSERT_DEATH(wake_lease.AddPowerElement("exceptions-element-002"), HasSubstr("Check failed"));
}

}  // namespace
}  // namespace forensics::exceptions::handler
