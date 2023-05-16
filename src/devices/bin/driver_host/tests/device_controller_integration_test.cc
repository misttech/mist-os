// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.test/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/devmgr-integration-test/fixture.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/zx/channel.h>
#include <lib/zx/vmo.h>
#include <limits.h>

#include <map>

#include <ddk/metadata/test.h>
#include <zxtest/zxtest.h>

#include "src/devices/bin/driver_host/tests/test-metadata.h"

namespace {

using devmgr_integration_test::IsolatedDevmgr;

constexpr const char kPassDriverName[] = "unit-test-pass.cm";
constexpr const char kFailDriverName[] = "unit-test-fail.cm";

void CreateTestDevice(const IsolatedDevmgr& devmgr, const char* driver_name,
                      fidl::ClientEnd<fuchsia_device::Controller>* controller_channel,
                      fidl::ClientEnd<fuchsia_device_test::Device>* dev_channel) {
  zx::result channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), fuchsia_device_test::wire::kControlDevice);
  ASSERT_OK(channel.status_value());

  fidl::ClientEnd<fuchsia_device_test::RootDevice> test_client_end(std::move(channel.value()));
  fidl::WireSyncClient test_root{std::move(test_client_end)};

  zx::channel local, remote;
  ASSERT_OK(zx::channel::create(0, &local, &remote));

  auto result = test_root->CreateDevice(fidl::StringView::FromExternal(driver_name));
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok(), "CreateDevice failed %s",
              zx_status_get_string(result->error_value()));

  // Connect to the child that we created.
  std::string device_path;
  device_path.append(fuchsia_device_test::wire::kControlDevice);
  device_path.append("/");
  device_path.append(driver_name);
  {
    zx::result channel =
        device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), device_path.c_str());
    ASSERT_OK(channel.status_value());
    *dev_channel = fidl::ClientEnd<fuchsia_device_test::Device>{std::move(channel.value())};
  }
  // Connect to it's device controller.
  {
    device_path.append("/device_controller");

    zx::result channel =
        device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), device_path.c_str());
    ASSERT_OK(channel.status_value());
    *controller_channel = fidl::ClientEnd<fuchsia_device::Controller>{std::move(channel.value())};
  }
}

class DeviceControllerIntegrationTest : public zxtest::Test {
 public:
  // NB: this loop is never run. RealmBuilder::Build is in the call stack, and insists on a non-null
  // dispatcher.
  //
  // TODO(https://fxbug.dev/114254): Remove this.
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
};

// Test binding second time
TEST_F(DeviceControllerIntegrationTest, TestDuplicateBindSameDriver) {
  auto args = IsolatedDevmgr::DefaultArgs();

  args.root_device_driver = "/boot/meta/test-parent-sys.cm";

  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kPassDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kPassDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);
  call_status = ZX_OK;
  auto resp2 =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kPassDriverName));
  ASSERT_OK(resp2.status());
  if (resp2.value().is_error()) {
    call_status = resp2.value().error_value();
  }
  ASSERT_OK(resp2.status());
  ASSERT_STATUS(ZX_ERR_ALREADY_BOUND, call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

TEST_F(DeviceControllerIntegrationTest, TestRebindNoChildrenManualBind) {
  auto args = IsolatedDevmgr::DefaultArgs();
  args.root_device_driver = "/boot/meta/test-parent-sys.cm";

  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kPassDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Rebind(fidl::StringView::FromExternal(kPassDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

TEST_F(DeviceControllerIntegrationTest, TestRebindChildrenAutoBind) {
  using driver_integration_test::IsolatedDevmgr;
  driver_integration_test::IsolatedDevmgr::Args args;
  driver_integration_test::IsolatedDevmgr devmgr;

  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata = {
      .init_reply_success = true,
  };
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_DEVHOST_TEST;
  dev.did = 0;
  args.device_list.push_back(dev);

  zx_status_t status = IsolatedDevmgr::Create(&args, &devmgr);
  ASSERT_OK(status);

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform/11:0e:0")
                .status_value());
  zx::result channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(channel.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> parent_channel{std::move(channel.value())};

  // Do not open the child. Otherwise rebind will be stuck.
  zx_status_t call_status = ZX_OK;
  auto resp = fidl::WireCall(parent_channel)->Rebind(fidl::StringView(""));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                 "sys/platform/11:0e:0/devhost-test-parent")
                .status_value());
  ASSERT_OK(
      device_watcher::RecursiveWaitForFile(
          devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child")
          .status_value());
}

TEST_F(DeviceControllerIntegrationTest, TestRebindChildrenManualBind) {
  using driver_integration_test::IsolatedDevmgr;
  driver_integration_test::IsolatedDevmgr::Args args;
  driver_integration_test::IsolatedDevmgr devmgr;

  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata = {
      .init_reply_success = true,
  };
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_DEVHOST_TEST;
  dev.did = 0;
  args.device_list.push_back(dev);

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform/11:0e:0")
                .status_value());
  zx::result channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(channel.status_value());
  fidl::ClientEnd<fuchsia_device::Controller> parent_channel{std::move(channel.value())};

  // Do not open the child. Otherwise rebind will be stuck.
  zx_status_t call_status = ZX_OK;
  auto resp = fidl::WireCall(parent_channel)
                  ->Rebind(fidl::StringView::FromExternal("driver-host-test-child-driver.cm"));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                 "sys/platform/11:0e:0/devhost-test-parent")
                .status_value());
  ASSERT_OK(
      device_watcher::RecursiveWaitForFile(
          devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child")
          .status_value());
}

TEST_F(DeviceControllerIntegrationTest, TestUnbindChildrenSuccess) {
  using driver_integration_test::IsolatedDevmgr;
  driver_integration_test::IsolatedDevmgr::Args args;
  driver_integration_test::IsolatedDevmgr devmgr;

  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata = {
      .init_reply_success = true,
  };
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_DEVHOST_TEST;
  dev.did = 0;
  args.device_list.push_back(dev);

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform/11:0e:0")
                .status_value());
  zx::result channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(channel.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> parent_channel{std::move(channel.value())};

  ASSERT_OK(
      device_watcher::RecursiveWaitForFile(
          devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child")
          .status_value());

  zx_status_t call_status = ZX_OK;
  auto resp = fidl::WireCall(parent_channel)->UnbindChildren();
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                 "sys/platform/11:0e:0/devhost-test-parent")
                .status_value());

  {
    // Rebind the child driver.
    auto resp = fidl::WireCall(parent_channel)->Bind(fidl::StringView::FromExternal(""));
    ASSERT_OK(resp.status());
    if (resp.value().is_error()) {
      call_status = resp.value().error_value();
    }
    ASSERT_OK(call_status);
  }
  {
    // Call |UnbindChildren| on the child, which has no children.
    zx::result channel = device_watcher::RecursiveWaitForFile(
        devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child");
    ASSERT_OK(channel.status_value());

    fidl::ClientEnd<fuchsia_device::Controller> child_channel{std::move(channel.value())};
    zx_status_t call_status = ZX_OK;
    auto resp = fidl::WireCall(child_channel)->UnbindChildren();
    ASSERT_OK(resp.status());
    if (resp.value().is_error()) {
      call_status = resp.value().error_value();
    }
    ASSERT_OK(call_status);
  }
}

// Test binding again, but with different driver
TEST_F(DeviceControllerIntegrationTest, TestDuplicateBindDifferentDriver) {
  auto args = IsolatedDevmgr::DefaultArgs();

  args.root_device_driver = "/boot/meta/test-parent-sys.cm";

  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kPassDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kPassDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);

  call_status = ZX_OK;
  auto resp2 =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kFailDriverName));
  ASSERT_OK(resp2.status());
  if (resp2.value().is_error()) {
    call_status = resp2.value().error_value();
  }
  ASSERT_OK(resp2.status());
  ASSERT_STATUS(ZX_ERR_ALREADY_BOUND, call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

TEST_F(DeviceControllerIntegrationTest, AllTestsEnabledBind) {
  auto args = IsolatedDevmgr::DefaultArgs();

  args.root_device_driver = "/boot/meta/test-parent-sys.cm";
  args.driver_tests_enable_all = true;

  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kPassDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kPassDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

TEST_F(DeviceControllerIntegrationTest, AllTestsEnabledBindFail) {
  auto args = IsolatedDevmgr::DefaultArgs();

  args.root_device_driver = "/boot/meta/test-parent-sys.cm";
  args.driver_tests_enable_all = true;

  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kFailDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kFailDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_STATUS(ZX_ERR_BAD_STATE, call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

// Test the flag using bind failure as a proxy for "the unit test did run".
TEST_F(DeviceControllerIntegrationTest, SpecificTestEnabledBindFail) {
  auto args = IsolatedDevmgr::DefaultArgs();

  args.root_device_driver = "/boot/meta/test-parent-sys.cm";
  args.driver_tests_enable.push_back("unit_test_fail");

  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kFailDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kFailDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_STATUS(ZX_ERR_BAD_STATE, call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

// Test the flag using bind success as a proxy for "the unit test didn't run".
TEST_F(DeviceControllerIntegrationTest, DefaultTestsDisabledBind) {
  auto args = IsolatedDevmgr::DefaultArgs();

  args.root_device_driver = "/boot/meta/test-parent-sys.cm";
  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kFailDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kFailDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

// Test the flag using bind success as a proxy for "the unit test didn't run".
TEST_F(DeviceControllerIntegrationTest, SpecificTestDisabledBind) {
  auto args = IsolatedDevmgr::DefaultArgs();

  args.root_device_driver = "/boot/meta/test-parent-sys.cm";
  args.driver_tests_enable_all = true;
  args.driver_tests_disable.push_back("unit_test_fail");

  zx::result devmgr = IsolatedDevmgr::Create(std::move(args), loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  fidl::ClientEnd<fuchsia_device::Controller> controller_channel;
  fidl::ClientEnd<fuchsia_device_test::Device> dev_channel;
  CreateTestDevice(devmgr.value(), kFailDriverName, &controller_channel, &dev_channel);

  zx_status_t call_status = ZX_OK;
  auto resp =
      fidl::WireCall(controller_channel)->Bind(fidl::StringView::FromExternal(kFailDriverName));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);
  ASSERT_OK(fidl::WireCall(dev_channel)->Destroy());
}

TEST_F(DeviceControllerIntegrationTest, TestRebindWithInit_Success) {
  using driver_integration_test::IsolatedDevmgr;
  driver_integration_test::IsolatedDevmgr::Args args;
  driver_integration_test::IsolatedDevmgr devmgr;

  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata = {
      .init_reply_success = true,
  };
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_DEVHOST_TEST;
  dev.did = 0;
  args.device_list.push_back(dev);

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform/11:0e:0")
                .status_value());
  zx::result channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(channel.status_value());
  fidl::ClientEnd<fuchsia_device::Controller> parent_channel{std::move(channel.value())};

  zx_status_t call_status = ZX_OK;
  auto resp = fidl::WireCall(parent_channel)->Rebind(fidl::StringView(""));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_OK(call_status);

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                 "sys/platform/11:0e:0/devhost-test-parent")
                .status_value());
  ASSERT_OK(
      device_watcher::RecursiveWaitForFile(
          devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent/devhost-test-child")
          .status_value());
}

TEST_F(DeviceControllerIntegrationTest, TestRebindWithInit_Failure) {
  using driver_integration_test::IsolatedDevmgr;
  driver_integration_test::IsolatedDevmgr::Args args;
  driver_integration_test::IsolatedDevmgr devmgr;

  board_test::DeviceEntry dev = {};
  struct devhost_test_metadata test_metadata = {
      .init_reply_success = false,
  };
  dev.metadata = reinterpret_cast<const uint8_t*>(&test_metadata);
  dev.metadata_size = sizeof(test_metadata);
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_DEVHOST_TEST;
  dev.did = 0;
  args.device_list.push_back(dev);

  ASSERT_OK(IsolatedDevmgr::Create(&args, &devmgr));

  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), "sys/platform/11:0e:0")
                .status_value());
  zx::result channel = device_watcher::RecursiveWaitForFile(
      devmgr.devfs_root().get(), "sys/platform/11:0e:0/devhost-test-parent");
  ASSERT_OK(channel.status_value());
  fidl::ClientEnd<fuchsia_device::Controller> parent_channel{std::move(channel.value())};

  zx_status_t call_status = ZX_OK;
  auto resp = fidl::WireCall(parent_channel)->Rebind(fidl::StringView(""));
  ASSERT_OK(resp.status());
  if (resp.value().is_error()) {
    call_status = resp.value().error_value();
  }
  ASSERT_STATUS(ZX_ERR_IO, call_status);
  ASSERT_OK(device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                 "sys/platform/11:0e:0/devhost-test-parent")
                .status_value());
}

}  // namespace
