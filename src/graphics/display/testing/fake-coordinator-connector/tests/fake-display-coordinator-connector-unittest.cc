// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/status.h>
#include <zircon/types.h>

#include <chrono>
#include <thread>

#include <gtest/gtest.h>

#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/testing/fake-coordinator-connector/service.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace {

constexpr std::chrono::milliseconds kSleepTime(10);

class FakeDisplayCoordinatorConnectorTest : public gtest::TestLoopFixture {
 public:
  FakeDisplayCoordinatorConnectorTest() = default;
  ~FakeDisplayCoordinatorConnectorTest() override = default;

  void SetUp() override {
    TestLoopFixture::SetUp();

    constexpr fake_display::FakeDisplayDeviceConfig kFakeDisplayDeviceConfig = {
        .periodic_vsync = true,
        .no_buffer_access = false,
    };
    coordinator_connector_ = std::make_unique<display::FakeDisplayCoordinatorConnector>(
        dispatcher(), kFakeDisplayDeviceConfig);

    auto [client_end, server_end] = fidl::Endpoints<fuchsia_hardware_display::Provider>::Create();

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

TEST_F(FakeDisplayCoordinatorConnectorTest, TeardownClientChannelAfterCoordinatorConnector) {
  // Count the number of connections that were ever made.
  int num_connections = 0;

  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>>
      primary_result;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator1 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> listener1 =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client()
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator1.server),
          .coordinator_listener = std::move(listener1.client),
      }})
      .Then([&num_connections, &primary_result](
                fidl::Result<
                    fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>&
                    result) {
        primary_result.emplace(std::move(result));
        ++num_connections;
      });

  RunLoopUntilIdle();

  EXPECT_TRUE(primary_result.has_value());
  EXPECT_TRUE(primary_result->is_ok());

  coordinator_connector_.reset();
  // Client connection is closed with epitaphs so the test loop should have
  // pending task.
  EXPECT_TRUE(RunLoopUntilIdle());

  coordinator1.client.reset();
  // Now that the coordinator connector is closed.
  EXPECT_FALSE(RunLoopUntilIdle());
}

TEST_F(FakeDisplayCoordinatorConnectorTest, NoConflictWithVirtcon) {
  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>>
      primary_result;
  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForVirtcon>>
      virtcon_result;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator1 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> listener1 =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client()
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator1.server),
          .coordinator_listener = std::move(listener1.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            primary_result.emplace(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator2 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> listener2 =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client()
      ->OpenCoordinatorWithListenerForVirtcon({{
          .coordinator = std::move(coordinator2.server),
          .coordinator_listener = std::move(listener2.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForVirtcon>& result) {
            virtcon_result.emplace(std::move(result));
          });

  RunLoopUntilIdle();

  EXPECT_TRUE(primary_result.has_value());
  EXPECT_TRUE(primary_result->is_ok());

  EXPECT_TRUE(virtcon_result.has_value());
  EXPECT_TRUE(virtcon_result->is_ok());
}

TEST_F(FakeDisplayCoordinatorConnectorTest, MultipleConnections) {
  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>>
      primary_result_connection1, primary_result_connection2, primary_result_connection3;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator1 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> listener1 =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client()
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator1.server),
          .coordinator_listener = std::move(listener1.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            primary_result_connection1.emplace(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator2 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> listener2 =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client()
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator2.server),
          .coordinator_listener = std::move(listener2.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            primary_result_connection2.emplace(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator3 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> listener3 =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client()
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator3.server),
          .coordinator_listener = std::move(listener3.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            primary_result_connection3.emplace(std::move(result));
          });

  RunLoopUntilIdle();

  EXPECT_TRUE(primary_result_connection1.has_value());
  EXPECT_TRUE(primary_result_connection1->is_ok());
  EXPECT_FALSE(primary_result_connection2.has_value());
  EXPECT_FALSE(primary_result_connection3.has_value());
  primary_result_connection1.reset();

  // Drop the first connection, which will enable the second connection to be made.
  coordinator1.client.reset();

  while (!RunLoopUntilIdle()) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
  }

  EXPECT_FALSE(primary_result_connection1.has_value());
  EXPECT_TRUE(primary_result_connection2.has_value());
  EXPECT_TRUE(primary_result_connection2->is_ok());
  EXPECT_FALSE(primary_result_connection3.has_value());
  primary_result_connection2.reset();

  // Drop the second connection, which will enable the third connection to be made.
  coordinator2.client.reset();

  while (!RunLoopUntilIdle()) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
  }

  EXPECT_FALSE(primary_result_connection1.has_value());
  EXPECT_FALSE(primary_result_connection2.has_value());
  EXPECT_TRUE(primary_result_connection3.has_value());
  EXPECT_TRUE(primary_result_connection3->is_ok());
  primary_result_connection3.reset();
}

}  // namespace
