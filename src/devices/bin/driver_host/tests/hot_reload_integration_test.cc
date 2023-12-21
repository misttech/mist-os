// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.device.restarttest/cpp/wire.h>
#include <fidl/fuchsia.device.test/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/zx/channel.h>
#include <zircon/types.h>

#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

#include "src/devices/bin/driver_host/tests/test-metadata.h"

namespace {

using driver_integration_test::IsolatedDevmgr;
using fuchsia_device_restarttest::TestDevice;

constexpr std::string_view kDriverRestartUrl =
    "fuchsia-boot:///#meta/driver-host-restart-driver.cm";
constexpr std::string_view kTestDriverRestartUrl =
    "fuchsia-boot:///#meta/driver-host-test-driver.cm";
constexpr std::string_view kChildDriverRestartUrl =
    "fuchsia-boot:///#meta/driver-host-test-child-driver.cm";

zx::result<fidl::WireSyncClient<fuchsia_driver_development::DriverDevelopment>> SetupEnvironment(
    board_test::DeviceEntry dev, driver_integration_test::IsolatedDevmgr* devmgr) {
  driver_integration_test::IsolatedDevmgr::Args args;
  args.device_list.push_back(dev);

  if (zx_status_t status = IsolatedDevmgr::Create(&args, devmgr); status != ZX_OK) {
    return zx::error(status);
  }
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_driver_development::DriverDevelopment>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }

  if (zx_status_t status = devmgr->Connect(
          fidl::DiscoverableProtocolName<fuchsia_driver_development::DriverDevelopment>,
          endpoints->server.TakeChannel());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(fidl::WireSyncClient(std::move(endpoints->client)));
}

// Test restarting a driver host containing only one driver.
TEST(HotReloadIntegrationTest, DISABLED_TestRestartOneDriver) {
  driver_integration_test::IsolatedDevmgr devmgr;

  // Test device to add to devmgr.
  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata;
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_RESTART_TEST;
  dev.did = 0;

  // Setup the environment for testing.
  zx::result development = SetupEnvironment(dev, &devmgr);
  ASSERT_OK(development);

  uint64_t pid_before;
  {
    zx::result channel = device_watcher::RecursiveWaitForFile(
        devmgr.devfs_root().get(), "sys/platform/11:17:0/driver-host-restart-driver");
    ASSERT_OK(channel.status_value());
    fidl::ClientEnd<TestDevice> chan_driver{std::move(channel.value())};
    ASSERT_TRUE(chan_driver.is_valid());

    // Get pid of driver before restarting.
    const fidl::WireResult result = fidl::WireCall(chan_driver)->GetPid();
    ASSERT_OK(result);
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    pid_before = response.value()->pid;
  }

  // Need to create a DirWatcher to wait for the device to close.
  fbl::unique_fd fd(
      openat(devmgr.devfs_root().get(), "sys/platform/11:17:0", O_DIRECTORY | O_RDONLY));
  std::unique_ptr<device_watcher::DirWatcher> watcher;
  ASSERT_OK(device_watcher::DirWatcher::Create(fd.get(), &watcher));

  // Restart the driver host of the test driver.
  fidl::WireResult response =
      development.value()->RestartDriverHosts(fidl::StringView::FromExternal(kDriverRestartUrl),
                                              fuchsia_driver_development::RematchFlags());
  ASSERT_OK(response);
  ASSERT_EQ(response->value()->count, 1);

  // Make sure device has shut so that it isnt opened before it is restarted.
  ASSERT_OK(watcher->WaitForRemoval("driver-host-restart-driver", zx::duration::infinite()));

  // Get pid of driver after restarting.
  {
    zx::result channel = device_watcher::RecursiveWaitForFile(
        devmgr.devfs_root().get(), "sys/platform/11:17:0/driver-host-restart-driver");
    ASSERT_OK(channel.status_value());
    fidl::ClientEnd<TestDevice> chan_driver{std::move(channel.value())};
    ASSERT_TRUE(chan_driver.is_valid());

    const fidl::WireResult result = fidl::WireCall(chan_driver)->GetPid();
    ASSERT_OK(result);
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));

    ASSERT_NE(pid_before, response.value()->pid);
  }
}

// Test restarting a driver host containing a parent and child driver by calling restart on
// the parent.
TEST(HotReloadIntegrationTest, DISABLED_TestRestartTwoDriversParent) {
  driver_integration_test::IsolatedDevmgr devmgr;

  // Test device to add to devmgr.
  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata = {
      .init_reply_success = true,
  };
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_DEVHOST_TEST;
  dev.did = 0;

  // Setup the environment for testing.
  zx::result development = SetupEnvironment(dev, &devmgr);
  ASSERT_OK(development);

  zx::channel chan_child;

  // Open parent.
  zx::result parent_channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(parent_channel.status_value());
  auto chan_parent = fidl::ClientEnd<TestDevice>(std::move(parent_channel.value()));
  ASSERT_TRUE(chan_parent.is_valid());

  // Open child.
  zx::result child_channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child");
  ASSERT_OK(child_channel.status_value());
  chan_child = std::move(child_channel.value());
  ASSERT_NE(chan_child.get(), ZX_HANDLE_INVALID);
  ASSERT_TRUE(chan_child.is_valid());

  // Get pid of parent driver before restarting.
  auto parent_before = fidl::WireCall(chan_parent)->GetPid();
  ASSERT_OK(parent_before.status());
  ASSERT_FALSE(parent_before->is_error(), "GetPid for parent failed: %s",
               zx_status_get_string(parent_before->error_value()));

  // Need to create DirWatchers to wait for the device to close.
  fbl::unique_fd fd_watcher(
      openat(devmgr.devfs_root().get(), "sys/platform/11:0e:0", O_DIRECTORY | O_RDONLY));
  std::unique_ptr<device_watcher::DirWatcher> watcher;
  ASSERT_OK(device_watcher::DirWatcher::Create(fd_watcher.get(), &watcher));

  // Restart the driver host of the parent driver.
  fidl::WireResult response =
      development.value()->RestartDriverHosts(fidl::StringView::FromExternal(kTestDriverRestartUrl),
                                              fuchsia_driver_development::RematchFlags());
  ASSERT_OK(response);

  // Make sure device has shut so that it isn't opened before it is restarted.
  // Child is a subdirectory of this so if the parent is gone so must the child.
  ASSERT_OK(watcher->WaitForRemoval("devhost-test-parent", zx::duration::infinite()));

  // Reopen parent.
  parent_channel = device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                        "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(parent_channel.status_value());
  chan_parent = fidl::ClientEnd<TestDevice>(std::move(parent_channel.value()));
  ASSERT_TRUE(chan_parent.is_valid());

  // Get pid of parent driver after restarting.
  auto parent_after = fidl::WireCall(chan_parent)->GetPid();
  ASSERT_OK(parent_after.status());
  ASSERT_FALSE(parent_after->is_error(), "GetPid for parent failed: %s",
               zx_status_get_string(parent_after->error_value()));

  // Check pid of parent has changed.
  ASSERT_NE(parent_before->value()->pid, parent_after->value()->pid);

  // Check child has reopened.
  child_channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child");
  ASSERT_OK(child_channel.status_value());
  chan_child = std::move(child_channel.value());
  ASSERT_NE(chan_child.get(), ZX_HANDLE_INVALID);
  ASSERT_TRUE(chan_child.is_valid());
}

// Test restarting a driver host containing a parent and child driver by calling restart on
// the child.
TEST(HotReloadIntegrationTest, DISABLED_TestRestartTwoDriversChild) {
  driver_integration_test::IsolatedDevmgr devmgr;

  // Test device to add to devmgr.
  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata = {
      .init_reply_success = true,
  };
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_DEVHOST_TEST;
  dev.did = 0;

  // Setup the environment for testing.
  zx::result development = SetupEnvironment(dev, &devmgr);
  ASSERT_OK(development);

  // Open parent.
  zx::result parent_channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(parent_channel.status_value());
  auto chan_parent = fidl::ClientEnd<TestDevice>{std::move(parent_channel.value())};
  ASSERT_TRUE(chan_parent.is_valid());

  // Open child.
  zx::result child_channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child");
  ASSERT_OK(child_channel.status_value());

  // Need to create DirWatchers to wait for the device to close.
  fbl::unique_fd fd_watcher(
      openat(devmgr.devfs_root().get(), "sys/platform/11:0e:0", O_DIRECTORY | O_RDONLY));
  std::unique_ptr<device_watcher::DirWatcher> watcher;
  ASSERT_OK(device_watcher::DirWatcher::Create(fd_watcher.get(), &watcher));

  // Get pid of parent driver before restarting.
  auto parent_before = fidl::WireCall(chan_parent)->GetPid();
  ASSERT_OK(parent_before.status());
  ASSERT_FALSE(parent_before->is_error(), "GetPid for parent failed: %s",
               zx_status_get_string(parent_before->error_value()));

  // Restart the driver host of the child driver.
  fidl::WireResult response = development.value()->RestartDriverHosts(
      fidl::StringView::FromExternal(kChildDriverRestartUrl),
      fuchsia_driver_development::RematchFlags());
  ASSERT_OK(response);

  // Make sure device has shut so that it isn't opened before it is restarted.
  // Child is a subdirectory of this so if the parent is gone so must the child.
  ASSERT_OK(watcher->WaitForRemoval("devhost-test-parent", zx::duration::infinite()));

  // Reopen parent.
  parent_channel = device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                        "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(parent_channel.status_value());
  chan_parent = fidl::ClientEnd<TestDevice>{std::move(parent_channel.value())};
  ASSERT_TRUE(chan_parent.is_valid());

  // Get pid of parent driver after restarting.
  auto parent_after = fidl::WireCall(chan_parent)->GetPid();
  ASSERT_OK(parent_after.status());
  ASSERT_FALSE(parent_after->is_error(), "GetPid for parent failed: %s",
               zx_status_get_string(parent_after->error_value()));

  // Check pid of parent has changed.
  ASSERT_NE(parent_before->value()->pid, parent_after->value()->pid);

  // Check child has reopened.
  child_channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child");
  ASSERT_OK(child_channel.status_value());
}

}  // namespace
