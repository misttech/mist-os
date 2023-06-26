// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <zircon/types.h>

#include <chrono>
#include <thread>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "gmock/gmock.h"
#include "lib/fidl/cpp/wire/status.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/testing/coordinator-connector/fake/service.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/lib/testing/predicates/status.h"

namespace {

constexpr std::chrono::milliseconds kSleepTime(10);

class FakeDisplayCoordinatorConnectorTest : public gtest::TestLoopFixture {
 public:
  FakeDisplayCoordinatorConnectorTest() = default;
  ~FakeDisplayCoordinatorConnectorTest() override = default;

  void SetUp() override {
    TestLoopFixture::SetUp();

    coordinator_connector_ = std::make_unique<display::FakeDisplayCoordinatorConnector>(
        MockDevice::FakeRootParent(), dispatcher());

    zx::result<fidl::Endpoints<fuchsia_hardware_display::Provider>> provider_endpoints_result =
        fidl::CreateEndpoints<fuchsia_hardware_display::Provider>();
    ASSERT_OK(provider_endpoints_result.status_value());
    auto [client_end, server_end] = std::move(provider_endpoints_result.value());

    fidl::BindServer(dispatcher(), std::move(server_end), coordinator_connector_.get());
    provider_client_ = fidl::Client(std::move(client_end), dispatcher());
  }

  void TearDown() override {
    RunLoopUntilIdle();
    coordinator_connector_.reset();
  }

  fidl::Client<fuchsia_hardware_display::Provider>& provider_client() { return provider_client_; }
  display::FakeDisplayCoordinatorConnector* coordinator_connector() {
    return coordinator_connector_.get();
  }

 protected:
  fidl::Client<fuchsia_hardware_display::Provider> provider_client_;
  std::unique_ptr<display::FakeDisplayCoordinatorConnector> coordinator_connector_;
};

fidl::Endpoints<fuchsia_hardware_display::Coordinator> NewCoordinatorEndpoints() {
  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> endpoints_result =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  EXPECT_OK(endpoints_result.status_value());
  return std::move(endpoints_result.value());
}

}  // anonymous namespace

TEST_F(FakeDisplayCoordinatorConnectorTest, TeardownClientChannelAfterCoordinatorConnector) {
  // Count the number of connections that were ever made.
  int num_connections = 0;

  std::vector<fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>>
      primary_results;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator1 = NewCoordinatorEndpoints();
  provider_client()
      ->OpenCoordinatorForPrimary(std::move(coordinator1.server))
      .Then(
          [&num_connections, &primary_results](
              fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>& result) {
            primary_results.push_back(std::move(result));
            ++num_connections;
          });

  RunLoopUntilIdle();

  EXPECT_THAT(primary_results,
              testing::ElementsAre(
                  fidl::Response<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>(
                      {.s = ZX_OK})));

  coordinator_connector_.reset();
  // Client connection is closed with epitaphs so the test loop should have
  // pending task.
  EXPECT_TRUE(RunLoopUntilIdle());

  coordinator1.client.reset();
  // Now that the coordinator connector is closed.
  EXPECT_FALSE(RunLoopUntilIdle());
}

TEST_F(FakeDisplayCoordinatorConnectorTest, NoConflictWithVirtcon) {
  std::vector<fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>>
      primary_results;
  std::vector<fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForVirtcon>>
      virtcon_results;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator1 = NewCoordinatorEndpoints();
  provider_client()
      ->OpenCoordinatorForPrimary(std::move(coordinator1.server))
      .Then(
          [&](fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>& result) {
            primary_results.push_back(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator2 = NewCoordinatorEndpoints();
  provider_client()
      ->OpenCoordinatorForVirtcon(std::move(coordinator2.server))
      .Then(
          [&](fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForVirtcon>& result) {
            virtcon_results.push_back(std::move(result));
          });

  RunLoopUntilIdle();

  EXPECT_THAT(primary_results,
              testing::ElementsAre(
                  fidl::Response<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>(
                      {.s = ZX_OK})));
  EXPECT_THAT(virtcon_results,
              testing::ElementsAre(
                  fidl::Response<fuchsia_hardware_display::Provider::OpenCoordinatorForVirtcon>(
                      {.s = ZX_OK})));
}

TEST_F(FakeDisplayCoordinatorConnectorTest, MultipleConnections) {
  std::vector<fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>>
      primary_results_connection1, primary_results_connection2, primary_results_connection3;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator1 = NewCoordinatorEndpoints();
  provider_client()
      ->OpenCoordinatorForPrimary(std::move(coordinator1.server))
      .Then(
          [&](fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>& result) {
            primary_results_connection1.push_back(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator2 = NewCoordinatorEndpoints();
  provider_client()
      ->OpenCoordinatorForPrimary(std::move(coordinator2.server))
      .Then(
          [&](fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>& result) {
            primary_results_connection2.push_back(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator3 = NewCoordinatorEndpoints();
  provider_client()
      ->OpenCoordinatorForPrimary(std::move(coordinator3.server))
      .Then(
          [&](fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>& result) {
            primary_results_connection3.push_back(std::move(result));
          });

  RunLoopUntilIdle();

  EXPECT_THAT(primary_results_connection1,
              testing::ElementsAre(
                  fidl::Response<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>(
                      {.s = ZX_OK})));
  EXPECT_THAT(primary_results_connection2, testing::IsEmpty());
  EXPECT_THAT(primary_results_connection3, testing::IsEmpty());
  primary_results_connection1.clear();

  // Drop the first connection, which will enable the second connection to be made.
  coordinator1.client.reset();

  while (!RunLoopUntilIdle()) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
  }

  EXPECT_THAT(primary_results_connection1, testing::IsEmpty());
  EXPECT_THAT(primary_results_connection2,
              testing::ElementsAre(
                  fidl::Response<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>(
                      {.s = ZX_OK})));
  EXPECT_THAT(primary_results_connection3, testing::IsEmpty());
  primary_results_connection2.clear();

  // Drop the second connection, which will enable the third connection to be made.
  coordinator2.client.reset();

  while (!RunLoopUntilIdle()) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
  }

  EXPECT_THAT(primary_results_connection1, testing::IsEmpty());
  EXPECT_THAT(primary_results_connection2, testing::IsEmpty());
  EXPECT_THAT(primary_results_connection3,
              testing::ElementsAre(
                  fidl::Response<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>(
                      {.s = ZX_OK})));
  primary_results_connection3.clear();
}
