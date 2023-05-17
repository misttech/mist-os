// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#include <chrono>
#include <thread>

#include <gtest/gtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/testing/coordinator-provider/fake/service.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace {

constexpr std::chrono::milliseconds kSleepTime(10);

class FakeHardwareDisplayCoordinatorProviderTest : public gtest::TestLoopFixture {
 public:
  FakeHardwareDisplayCoordinatorProviderTest() = default;
  ~FakeHardwareDisplayCoordinatorProviderTest() override = default;

  void SetUp() override {
    TestLoopFixture::SetUp();

    service_ = std::make_unique<fake_display::ProviderService>(MockDevice::FakeRootParent(),
                                                               nullptr, dispatcher());
  }

  void TearDown() override {
    // TODO(fxbug.dev/66466): this shouldn't be necessary, but without it there will be ASAN
    // failures, as the coordinator connections established by the tests haven't finished being torn
    // down.
    while (service_->coordinator_claimed() || service_->virtcon_coordinator_claimed()) {
      std::this_thread::sleep_for(kSleepTime);
      RunLoopUntilIdle();
    }

    service_.reset();
    RunLoopUntilIdle();
  }

  fake_display::ProviderService* service() { return service_.get(); }

 private:
  std::unique_ptr<fake_display::ProviderService> service_;
};

struct Request {
  fidl::InterfacePtr<fuchsia::hardware::display::Coordinator> coordinator;
};

Request NewRequest() { return Request(); }

}  // anonymous namespace

TEST_F(FakeHardwareDisplayCoordinatorProviderTest, NoConflictWithVirtcon) {
  // Count the number of connections that were ever made.
  uint32_t num_connections = 0;
  uint32_t num_virtcon_connections = 0;

  auto req = NewRequest();
  service()->OpenCoordinatorForPrimary(
      req.coordinator.NewRequest(), [&num_connections](zx_status_t status) { ++num_connections; });

  auto req2 = NewRequest();
  service()->OpenCoordinatorForVirtcon(
      req2.coordinator.NewRequest(),
      [&num_virtcon_connections](zx_status_t status) { ++num_virtcon_connections; });

  EXPECT_EQ(num_connections, 1U);
  EXPECT_EQ(num_virtcon_connections, 1U);
}

TEST_F(FakeHardwareDisplayCoordinatorProviderTest, MultipleConnections) {
  // Count the number of connections that were ever made.
  uint32_t num_connections = 0;

  auto req = NewRequest();
  service()->OpenCoordinatorForPrimary(
      req.coordinator.NewRequest(),
      [&num_connections](zx_status_t status) { EXPECT_EQ(++num_connections, 1U); });

  auto req2 = NewRequest();
  service()->OpenCoordinatorForPrimary(
      req2.coordinator.NewRequest(),
      [&num_connections](zx_status_t status) { EXPECT_EQ(++num_connections, 2U); });

  auto req3 = NewRequest();
  service()->OpenCoordinatorForPrimary(
      req3.coordinator.NewRequest(),
      [&num_connections](zx_status_t status) { EXPECT_EQ(++num_connections, 3U); });

  EXPECT_EQ(num_connections, 1U);
  EXPECT_EQ(service()->num_queued_requests(), 2U);

  // Drop the first connection, which will enable the second connection to be made.
  req.coordinator.Unbind();
  while (service()->num_queued_requests() == 2) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
    RunLoopUntilIdle();
  }

  EXPECT_EQ(num_connections, 2U);
  EXPECT_EQ(service()->num_queued_requests(), 1U);

  // Drop the second connection, which will enable the third connection to be made.
  req2.coordinator.Unbind();
  while (service()->num_queued_requests() == 1) {
    // Real wall clock time must elapse for the service to handle a kernel notification
    // that the channel has closed.
    std::this_thread::sleep_for(kSleepTime);
    RunLoopUntilIdle();
  }

  EXPECT_EQ(num_connections, 3U);
  EXPECT_EQ(service()->num_queued_requests(), 0U);
}
