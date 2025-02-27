// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/status.h>
#include <zircon/types.h>

#include <chrono>
#include <thread>
#include <utility>

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
        .periodic_vsync = false,
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

 protected:
  fidl::Client<fuchsia_hardware_display::Provider> provider_client_;
  std::unique_ptr<display::FakeDisplayCoordinatorConnector> coordinator_connector_;
};

TEST_F(FakeDisplayCoordinatorConnectorTest, CoordinatorChannelCloseFollowedByClientChannelClose) {
  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>>
      open_primary_result;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_primary =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> primary_listener =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client_
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator_primary.server),
          .coordinator_listener = std::move(primary_listener.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            open_primary_result.emplace(std::move(result));
          });

  RunLoopUntilIdle();

  ASSERT_TRUE(open_primary_result.has_value());
  EXPECT_TRUE(open_primary_result->is_ok()) << open_primary_result->error_value();

  coordinator_connector_.reset();
  EXPECT_TRUE(RunLoopUntilIdle())
      << "Coordinator-side FIDL connection closure did not generate a pending task";

  coordinator_primary.client.reset();
  EXPECT_FALSE(RunLoopUntilIdle())
      << "Coordinator-side FIDL connection was already closed. "
      << "Closing the client-side FIDL channel shouldn't generate any pending task";
}

TEST_F(FakeDisplayCoordinatorConnectorTest, OpenPrimaryAndVirtconConnections) {
  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>>
      open_primary_result;
  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForVirtcon>>
      open_virtcon_result;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_primary =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> primary_listener =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client_
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator_primary.server),
          .coordinator_listener = std::move(primary_listener.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            open_primary_result.emplace(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_virtcon =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> virtcon_listener =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client_
      ->OpenCoordinatorWithListenerForVirtcon({{
          .coordinator = std::move(coordinator_virtcon.server),
          .coordinator_listener = std::move(virtcon_listener.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForVirtcon>& result) {
            open_virtcon_result.emplace(std::move(result));
          });

  RunLoopUntilIdle();

  ASSERT_TRUE(open_primary_result.has_value());
  EXPECT_TRUE(open_primary_result->is_ok()) << open_primary_result->error_value();

  ASSERT_TRUE(open_virtcon_result.has_value());
  EXPECT_TRUE(open_virtcon_result->is_ok()) << open_virtcon_result->error_value();
}

TEST_F(FakeDisplayCoordinatorConnectorTest, ThreeSimultaneousPrimaryConnections) {
  std::optional<
      fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>>
      open_primary1_result, open_primary2_result, open_primary3_result;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_primary1 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> primary1_listener =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client_
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator_primary1.server),
          .coordinator_listener = std::move(primary1_listener.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            open_primary1_result.emplace(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_primary2 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> primary2_listener =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client_
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator_primary2.server),
          .coordinator_listener = std::move(primary2_listener.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            open_primary2_result.emplace(std::move(result));
          });

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_primary3 =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener> primary3_listener =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  provider_client_
      ->OpenCoordinatorWithListenerForPrimary({{
          .coordinator = std::move(coordinator_primary3.server),
          .coordinator_listener = std::move(primary3_listener.client),
      }})
      .Then(
          [&](fidl::Result<
              fuchsia_hardware_display::Provider::OpenCoordinatorWithListenerForPrimary>& result) {
            open_primary3_result.emplace(std::move(result));
          });

  RunLoopUntilIdle();

  ASSERT_TRUE(open_primary1_result.has_value());
  EXPECT_TRUE(open_primary1_result->is_ok()) << open_primary1_result->error_value();
  EXPECT_FALSE(open_primary2_result.has_value())
      << "Second connection completed while first connection was still open";
  EXPECT_FALSE(open_primary3_result.has_value())
      << "Third connection completed while first connection was still open";

  open_primary1_result.reset();

  // Drop the first connection, which will enable the second connection to be made.
  coordinator_primary1.client.reset();

  while (!RunLoopUntilIdle()) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
  }

  EXPECT_FALSE(open_primary1_result.has_value())
      << "First connection completed again after closing";
  ASSERT_TRUE(open_primary2_result.has_value())
      << "Second connection not completed after first connection closed";
  EXPECT_TRUE(open_primary2_result->is_ok()) << open_primary2_result->error_value();
  EXPECT_FALSE(open_primary3_result.has_value())
      << "Third connection completed while second connection was still open";

  open_primary2_result.reset();

  // Drop the second connection, which will enable the third connection to be made.
  coordinator_primary2.client.reset();

  while (!RunLoopUntilIdle()) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
  }

  EXPECT_FALSE(open_primary1_result.has_value())
      << "First connection completed again after second connection closed";
  EXPECT_FALSE(open_primary2_result.has_value())
      << "Second connection completed againt after first connection closed";
  ASSERT_TRUE(open_primary3_result.has_value())
      << "Third connection not completed after second connection closed";
  EXPECT_TRUE(open_primary3_result->is_ok()) << open_primary3_result->error_value();
}

}  // namespace
