// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#define MAGMA_DLOG_ENABLE 1

#include <fidl/fuchsia.hardware.gpu.mali/cpp/wire.h>
#include <lib/component/incoming/cpp/service_member_watcher.h>
#include <lib/magma/magma.h>
#include <lib/magma/util/dlog.h>
#include <lib/magma/util/short_macros.h>
#include <lib/magma_client/test_util/magma_map_cpu.h>
#include <lib/magma_client/test_util/test_device_helper.h>
#include <lib/zx/channel.h>
#include <poll.h>

#include <thread>

#include <gtest/gtest.h>

#include "magma_arm_mali_types.h"
#include "mali_utils.h"
#include "src/graphics/drivers/msd-arm-mali/include/magma_vendor_queries.h"

namespace {

class TestConnection : public magma::TestDeviceBase {
 public:
  TestConnection() : magma::TestDeviceBase(MAGMA_VENDOR_ID_MALI) {
    magma_device_create_connection(device(), &connection_);
    DASSERT(connection_);

    magma_connection_create_context(connection_, &context_id_);
    helper_.emplace(connection_, context_id_);
  }

  ~TestConnection() {
    magma_connection_release_context(connection_, context_id_);

    if (connection_)
      magma_connection_release(connection_);
  }

  bool SupportsProtectedMode() {
    uint64_t value_out;
    EXPECT_EQ(MAGMA_STATUS_OK, magma_device_query(device(), kMsdArmVendorQuerySupportsProtectedMode,
                                                  nullptr, &value_out));
    return !!value_out;
  }

  void SubmitCommandBuffer(mali_utils::AtomHelper::How how, uint8_t atom_number,
                           uint8_t atom_dependency, bool protected_mode) {
    helper_->SubmitCommandBuffer(how, atom_number, atom_dependency, protected_mode);
  }

 private:
  magma_connection_t connection_;
  uint32_t context_id_;
  std::optional<mali_utils::AtomHelper> helper_;
};

TEST(PowerManagement, SuspendResume) {
  component::SyncServiceMemberWatcher<fuchsia_hardware_gpu_mali::UtilsService::Device> watcher;
  zx::result client_end = watcher.GetNextInstance(false);
  ASSERT_FALSE(client_end.is_error()) << client_end.status_string();

  auto client = fidl::WireSyncClient(std::move(*client_end));

  EXPECT_TRUE(client->SetPowerState(false).ok());

  std::unique_ptr<TestConnection> test;
  test.reset(new TestConnection());
  std::atomic_bool submit_returned{false};
  std::thread enable_thread([&] {
    // SubmitCommandBuffer waits 1 second, so delay less than that.
    zx::nanosleep(zx::deadline_after(zx::msec(500)));
    EXPECT_FALSE(submit_returned);
    EXPECT_TRUE(client->SetPowerState(true).ok());
  });
  test->SubmitCommandBuffer(mali_utils::AtomHelper::NORMAL, 1, 0, false);
  submit_returned = true;
  enable_thread.join();
}

// Repeatedly attempt to suspend/resume to GPU to attempt to trigger a soft stop.
TEST(PowerManagement, RepeatedSuspendResume) {
  component::SyncServiceMemberWatcher<fuchsia_hardware_gpu_mali::UtilsService::Device> watcher;
  zx::result client_end = watcher.GetNextInstance(false);
  ASSERT_FALSE(client_end.is_error()) << client_end.status_string();

  auto client = fidl::WireSyncClient(std::move(*client_end));

  std::unique_ptr<TestConnection> test;
  test.reset(new TestConnection());
  std::atomic_bool finished_test{false};
  std::thread enable_thread([&] {
    for (uint32_t i = 0; i < 20; i++) {
      constexpr uint32_t kMaxTimeToDelayMs = 10;
      zx::nanosleep(zx::deadline_after(zx::msec(rand() % kMaxTimeToDelayMs)));
      EXPECT_TRUE(client->SetPowerState(false).ok());
      zx::nanosleep(zx::deadline_after(zx::msec(1)));
      EXPECT_TRUE(client->SetPowerState(true).ok());
    }
    finished_test = true;
  });
  uint32_t i = 0;
  while (!finished_test) {
    {
      SCOPED_TRACE(std::to_string(i++));
      test->SubmitCommandBuffer(mali_utils::AtomHelper::NORMAL, 1, 0, false);
    }
  }
  enable_thread.join();
}
}  // namespace
