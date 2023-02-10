// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.environment.test/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver-integration-test/fixture.h>
#include <zircon/syscalls.h>

#include <unordered_set>

#include <zxtest/zxtest.h>

using driver_integration_test::IsolatedDevmgr;
using fuchsia_device_environment_test::TestDevice;

class EnvironmentTest : public zxtest::Test {
 public:
  ~EnvironmentTest() override = default;
  void SetUp() override {
    IsolatedDevmgr::Args args;

    board_test::DeviceEntry dev = {};
    dev.vid = PDEV_VID_TEST;
    dev.pid = PDEV_PID_ENVIRONMENT_TEST;
    dev.did = 0;
    args.device_list.push_back(dev);

    zx_status_t status = IsolatedDevmgr::Create(&args, &devmgr_);
    ASSERT_OK(status);
    zx::result channel = device_watcher::RecursiveWaitForFile(
        devmgr_.devfs_root().get(), "sys/platform/11:14:0/ddk-environment-test");
    ASSERT_OK(channel);
    client_.Bind(fidl::ClientEnd<TestDevice>{std::move(channel.value())});
  }

 protected:
  fidl::WireSyncClient<TestDevice> client_;
  IsolatedDevmgr devmgr_;
};

TEST_F(EnvironmentTest, GetServiceList) {
  const fidl::WireResult result = client_->GetServiceList();
  ASSERT_OK(result.status());
  const fidl::WireResponse response = result.value();
  ASSERT_EQ(response.services.count(), 3);

  std::unordered_set<std::string> actual;
  for (const auto& service : response.services) {
    actual.emplace(service.data(), service.size());
  }
  std::unordered_set<std::string> kExpectedServices = {
      "/svc/fuchsia.logger.LogSink",
      "/svc/fuchsia.scheduler.ProfileProvider",
      "/svc/fuchsia.tracing.provider.Registry",
  };
  ASSERT_EQ(actual, kExpectedServices);
}
