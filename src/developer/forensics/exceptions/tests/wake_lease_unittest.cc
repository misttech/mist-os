// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/exceptions/handler/wake_lease.h"

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/test_base.h>
#include <lib/async/cpp/executor.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/promise.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <memory>
#include <string>
#include <type_traits>
#include <utility>

#include <gtest/gtest.h>

#include "src/developer/forensics/exceptions/constants.h"
#include "src/developer/forensics/testing/gpretty_printers.h"
#include "src/developer/forensics/testing/stubs/power_broker_lessor.h"
#include "src/developer/forensics/testing/stubs/power_broker_topology.h"
#include "src/developer/forensics/testing/stubs/system_activity_governor.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/developer/forensics/utils/errors.h"

namespace forensics::exceptions::handler {
namespace {

using ::fidl::testing::TestBase;

constexpr zx::duration kTimeout = zx::sec(5);

namespace fpb = fuchsia_power_broker;
namespace fps = fuchsia_power_system;

class WakeLeaseTest : public UnitTestFixture {
 protected:
  WakeLeaseTest() : executor_(dispatcher()) {}

  async::Executor& GetExecutor() { return executor_; }

  template <typename Impl>
  static std::optional<std::tuple<fidl::ClientEnd<fps::ActivityGovernor>, std::unique_ptr<Impl>>>
  CreateSag(async_dispatcher_t* dispatcher) {
    static_assert(std::is_base_of_v<TestBase<fps::ActivityGovernor>, Impl>);

    auto endpoints = fidl::CreateEndpoints<fps::ActivityGovernor>();
    if (!endpoints.is_ok()) {
      return std::nullopt;
    }

    auto stub = std::make_unique<Impl>(std::move(endpoints->server), dispatcher);
    return std::make_tuple(std::move(endpoints->client), std::move(stub));
  }

  template <typename Impl>
  static std::optional<std::tuple<fidl::ClientEnd<fpb::Topology>, std::unique_ptr<Impl>>>
  CreateTopology(async_dispatcher_t* dispatcher,
                 typename Impl::ConstructLessorFn construct_lessor) {
    static_assert(std::is_base_of_v<TestBase<fpb::Topology>, Impl>);

    auto endpoints = fidl::CreateEndpoints<fpb::Topology>();
    if (!endpoints.is_ok()) {
      return std::nullopt;
    }

    auto stub = std::make_unique<Impl>(std::move(endpoints->server), dispatcher, construct_lessor);
    return std::make_tuple(std::move(endpoints->client), std::move(stub));
  }

 private:
  async::Executor executor_;
};

TEST_F(WakeLeaseTest, AcquiresLeaseSuccessfully) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kSatisfied);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<fidl::Client<fuchsia_power_broker::LeaseControl>> lease;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([&lease](fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            lease = std::move(acquired_lease);
          })
          .or_else([](const Error& error) {
            FX_LOGS(FATAL) << "Unexpected error while acquiring lease: " << ToString(error);
          }));

  RunLoopUntilIdle();

  ASSERT_TRUE(lease.has_value());
  EXPECT_TRUE(lease->is_valid());
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_TRUE(topology->IsLeaseActive("exceptions-element-001"));

  ASSERT_EQ(topology->Dependencies("exceptions-element-001").size(), 1u);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001").front().dependency_type(),
            fpb::DependencyType::kOpportunistic);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001").front().dependent_level(),
            kPowerLevelActive);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001")
                .front()
                .requires_level_by_preference()
                .front(),
            fidl::ToUnderlying(fps::ExecutionStateLevel::kWakeHandling));
}

TEST_F(WakeLeaseTest, AddsElementOnlyOnce) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kSatisfied);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  {
    std::optional<fidl::Client<fuchsia_power_broker::LeaseControl>> lease;
    GetExecutor().schedule_task(
        wake_lease.Acquire(kTimeout)
            .and_then([&lease](fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
              lease = std::move(acquired_lease);
            })
            .or_else([](const Error& error) {
              FX_LOGS(FATAL) << "Unexpected error while acquiring lease: " << ToString(error);
            }));

    RunLoopUntilIdle();

    ASSERT_TRUE(lease.has_value());
    EXPECT_TRUE(lease->is_valid());
    EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
    EXPECT_TRUE(topology->IsLeaseActive("exceptions-element-001"));
  }

  // Lease fell out of scope.
  RunLoopUntilIdle();
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));

  std::optional<fidl::Client<fuchsia_power_broker::LeaseControl>> lease;

  // Acquiring a lease again would check-fail if the element was added to the topology twice.
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([&lease](fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            lease = std::move(acquired_lease);
          })
          .or_else([](const Error& error) {
            FX_LOGS(FATAL) << "Unexpected error while acquiring lease: " << ToString(error);
          }));

  RunLoopUntilIdle();

  ASSERT_TRUE(lease.has_value());
  EXPECT_TRUE(lease->is_valid());
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_TRUE(topology->IsLeaseActive("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, WaitsForAddElementToComplete) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopologyDelaysResponse>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kSatisfied);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<fidl::Client<fuchsia_power_broker::LeaseControl>> lease;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([&lease](fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            lease = std::move(acquired_lease);
          })
          .or_else([](const Error& error) {
            FX_LOGS(FATAL) << "Unexpected error while acquiring lease: " << ToString(error);
          }));

  // The element is in the topology, but the topology hasn't returned a response to WakeLease yet
  // because PopResponse hasn't been called.
  RunLoopUntilIdle();
  EXPECT_FALSE(lease.has_value());
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));

  std::optional<fidl::Client<fuchsia_power_broker::LeaseControl>> lease2;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([&lease2](fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            lease2 = std::move(acquired_lease);
          })
          .or_else([](const Error& error) {
            FX_LOGS(FATAL) << "Unexpected error while acquiring lease: " << ToString(error);
          }));

  RunLoopUntilIdle();
  EXPECT_FALSE(lease.has_value());
  EXPECT_FALSE(lease2.has_value());

  topology->PopResponse();
  RunLoopUntilIdle();
  ASSERT_TRUE(lease.has_value());
  ASSERT_TRUE(lease2.has_value());

  EXPECT_TRUE(lease->is_valid());
  EXPECT_TRUE(lease2->is_valid());
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_TRUE(topology->IsLeaseActive("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, WaitsUntilLeaseSatisfied) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kPending);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<fidl::Client<fuchsia_power_broker::LeaseControl>> lease;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([&lease](fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            lease = std::move(acquired_lease);
          })
          .or_else([](const Error& error) {
            FX_LOGS(FATAL) << "Unexpected error while acquiring lease: " << ToString(error);
          }));

  RunLoopUntilIdle();
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_TRUE(topology->IsLeaseActive("exceptions-element-001"));

  ASSERT_EQ(topology->Dependencies("exceptions-element-001").size(), 1u);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001").front().dependency_type(),
            fpb::DependencyType::kOpportunistic);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001").front().dependent_level(),
            kPowerLevelActive);
  EXPECT_EQ(topology->Dependencies("exceptions-element-001")
                .front()
                .requires_level_by_preference()
                .front(),
            fidl::ToUnderlying(fps::ExecutionStateLevel::kWakeHandling));

  EXPECT_FALSE(lease.has_value());

  topology->SetLeaseStatus("exceptions-element-001", fuchsia_power_broker::LeaseStatus::kSatisfied);
  RunLoopUntilIdle();

  ASSERT_TRUE(lease.has_value());
  EXPECT_TRUE(lease->is_valid());
}

TEST_F(WakeLeaseTest, GetPowerElementsFails) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernorClosesConnection>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kSatisfied);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, GracefulSubsequentFailuresAfterFailureToAddElement) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernorClosesConnection>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kSatisfied);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));

  // Subsequent requests should also fail gracefully.
  error = std::nullopt;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, GetPowerElementsNoSagPowerElements) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernorNoPowerElements>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kSatisfied);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, GetPowerElementsNoTokens) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernorNoTokens>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kSatisfied);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_FALSE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, AddElementFails) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_endpoints = fidl::CreateEndpoints<fpb::Topology>();
  ASSERT_TRUE(topology_endpoints.is_ok());

  auto topology = std::make_unique<stubs::PowerBrokerTopologyClosesConnection>(
      std::move(topology_endpoints->server), dispatcher());

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_endpoints->client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();
  ASSERT_EQ(error, Error::kBadValue);
}

TEST_F(WakeLeaseTest, LeaseFails) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessorClosesConnection>(std::move(server_end),
                                                                          dispatcher);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopUntilIdle();

  ASSERT_EQ(error, Error::kBadValue);
  EXPECT_TRUE(topology->ElementInTopology("exceptions-element-001"));
  EXPECT_FALSE(topology->IsLeaseActive("exceptions-element-001"));
}

TEST_F(WakeLeaseTest, LeaseFailsOnTimeout) {
  auto sag_client_and_stub = CreateSag<stubs::SystemActivityGovernor>(dispatcher());
  ASSERT_TRUE(sag_client_and_stub.has_value());
  auto& [sag_client, sag] = *sag_client_and_stub;

  auto topology_client_and_stub = CreateTopology<stubs::PowerBrokerTopology>(
      dispatcher(),
      /*construct_lessor=*/[dispatcher = dispatcher()](
                               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) {
        return std::make_unique<stubs::PowerBrokerLessor>(std::move(server_end), dispatcher,
                                                          fpb::LeaseStatus::kPending);
      });
  ASSERT_TRUE(topology_client_and_stub.has_value());
  auto& [topology_client, topology] = *topology_client_and_stub;

  WakeLease wake_lease(dispatcher(), "exceptions-element-001", std::move(sag_client),
                       std::move(topology_client));

  std::optional<Error> error;
  GetExecutor().schedule_task(
      wake_lease.Acquire(kTimeout)
          .and_then([](const fidl::Client<fuchsia_power_broker::LeaseControl>& acquired_lease) {
            FX_LOGS(FATAL) << "Unexpected success while acquiring lease";
          })
          .or_else([&error](const Error& result) { error = result; }));

  RunLoopFor(kTimeout);
  EXPECT_EQ(error, Error::kTimeout);
}

}  // namespace
}  // namespace forensics::exceptions::handler
