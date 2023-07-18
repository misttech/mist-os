// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.hardware.hidctl/cpp/wire.h>
#include <fidl/fuchsia.hardware.input/cpp/wire.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <fuchsia/driver/test/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fd.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/syscalls.h>

#include <fbl/unique_fd.h>
#include <hid/boot.h>
#include <zxtest/zxtest.h>

namespace {

class HidDriverTest : public zxtest::Test {
 protected:
  void SetUp() override {
    // Create and build the realm.
    auto realm_builder = component_testing::RealmBuilder::Create();
    driver_test_realm::Setup(realm_builder);
    realm_ =
        std::make_unique<component_testing::RealmRoot>(realm_builder.Build(loop_.dispatcher()));

    // Start DriverTestRealm.
    ASSERT_OK(realm_->component().Connect(driver_test_realm.NewRequest()));
    fuchsia::driver::test::Realm_Start_Result realm_result;

    auto args = fuchsia::driver::test::RealmArgs();
#ifdef DFV2
    args.set_use_driver_framework_v2(true);
#else
    args.set_use_driver_framework_v2(false);
#endif

    ASSERT_OK(driver_test_realm->Start(std::move(args), &realm_result));
    ASSERT_FALSE(realm_result.is_err());

    // Connect to dev.
    fidl::InterfaceHandle<fuchsia::io::Node> dev;
    ASSERT_OK(realm_->component().Connect("dev-topological", dev.NewRequest().TakeChannel()));
    ASSERT_OK(fdio_fd_create(dev.TakeChannel().release(), dev_fd_.reset_and_get_address()));

    // Wait for HidCtl to be created.
    zx::result hidctl_channel =
        device_watcher::RecursiveWaitForFile(dev_fd_.get(), "sys/test/hidctl");
    ASSERT_OK(hidctl_channel.status_value());

    fidl::ClientEnd<fuchsia_hardware_hidctl::Device> client_end(std::move(hidctl_channel.value()));
    hidctl_client_.Bind(std::move(client_end));
  }

  template <typename Protocol>
  zx::result<fidl::ClientEnd<Protocol>> WaitForFirstDevice(const std::string& path) {
    fdio_cpp::UnownedFdioCaller caller(dev_fd_);
    zx::result directory_result =
        component::ConnectAt<fuchsia_io::Directory>(caller.directory(), path);
    if (directory_result.is_error()) {
      return directory_result.take_error();
    }
    auto& directory = directory_result.value();
    zx::result watch_result =
        device_watcher::WatchDirectoryForItems<zx::result<fidl::ClientEnd<Protocol>>>(
            directory, [&directory](std::string_view devpath) {
              return component::ConnectAt<Protocol>(directory, devpath);
            });
    if (watch_result.is_error()) {
      return watch_result.take_error();
    }
    return std::move(watch_result.value());
  }

  async::Loop loop_ = async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fbl::unique_fd dev_fd_;
  fidl::WireSyncClient<fuchsia_hardware_hidctl::Device> hidctl_client_;
  std::unique_ptr<component_testing::RealmRoot> realm_;
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
};

const uint8_t kBootMouseReportDesc[50] = {
    0x05, 0x01,  // Usage Page (Generic Desktop Ctrls)
    0x09, 0x02,  // Usage (Mouse)
    0xA1, 0x01,  // Collection (Application)
    0x09, 0x01,  //   Usage (Pointer)
    0xA1, 0x00,  //   Collection (Physical)
    0x05, 0x09,  //     Usage Page (Button)
    0x19, 0x01,  //     Usage Minimum (0x01)
    0x29, 0x03,  //     Usage Maximum (0x03)
    0x15, 0x00,  //     Logical Minimum (0)
    0x25, 0x01,  //     Logical Maximum (1)
    0x95, 0x03,  //     Report Count (3)
    0x75, 0x01,  //     Report Size (1)
    0x81, 0x02,  //     Input (Data,Var,Abs,No Wrap,Linear,No Null Position)
    0x95, 0x01,  //     Report Count (1)
    0x75, 0x05,  //     Report Size (5)
    0x81, 0x03,  //     Input (Const,Var,Abs,No Wrap,Linear,No Null Position)
    0x05, 0x01,  //     Usage Page (Generic Desktop Ctrls)
    0x09, 0x30,  //     Usage (X)
    0x09, 0x31,  //     Usage (Y)
    0x15, 0x81,  //     Logical Minimum (-127)
    0x25, 0x7F,  //     Logical Maximum (127)
    0x75, 0x08,  //     Report Size (8)
    0x95, 0x02,  //     Report Count (2)
    0x81, 0x06,  //     Input (Data,Var,Rel,No Wrap,Linear,No Null Position)
    0xC0,        //   End Collection
    0xC0,        // End Collection
};

TEST_F(HidDriverTest, BootMouseTest) {
  // Create a fake mouse device
  fuchsia_hardware_hidctl::wire::HidCtlConfig config = {};
  config.dev_num = 5;
  config.boot_device = false;
  config.dev_class = 0;
  auto result = hidctl_client_->MakeHidDevice(
      config, fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kBootMouseReportDesc),
                                                      std::size(kBootMouseReportDesc)));
  ASSERT_OK(result.status());
  zx::socket hidctl_socket = std::move(result.value().report_socket);

  fbl::unique_fd fd_device;

  // Wait for input-report driver to bind to avoid binding to a device which is being torn down.
  ASSERT_OK(
      WaitForFirstDevice<fuchsia_input_report::InputDevice>("class/input-report").status_value());
  // Open the /dev/class/input/ device created by MakeHidDevice.
  zx::result input_client = WaitForFirstDevice<fuchsia_hardware_input::Controller>("class/input");
  ASSERT_OK(input_client.status_value());

  // Open a FIDL channel to the HID device
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_input::Device>();
  ASSERT_OK(endpoints);
  auto& [device, server] = endpoints.value();

  ASSERT_OK(fidl::WireCall(input_client.value())->OpenSession(std::move(server)));

  fidl::WireSyncClient client(std::move(device));
  // Make a synchronous FIDL call to ensure the session connection is alive.
  ASSERT_OK(client->GetBootProtocol());

  // Send a single mouse report
  hid_boot_mouse_report_t mouse_report = {
      .rel_x = 50,
      .rel_y = 100,
  };
  ASSERT_OK(hidctl_socket.write(0, &mouse_report, sizeof(mouse_report), nullptr));

  // Get the report event.
  zx::event report_event;
  {
    auto result = client->GetReportsEvent();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    report_event = std::move(result.value().event);
  }

  // Check that the report comes through
  {
    report_event.wait_one(ZX_USER_SIGNAL_0, zx::time::infinite(), nullptr);

    hid_boot_mouse_report_t test_report = {};

    auto response = client->ReadReport();
    ASSERT_OK(response.status());
    ASSERT_OK(response.value().status);
    ASSERT_EQ(response.value().data.count(), sizeof(test_report));

    memcpy(&test_report, response.value().data.data(), sizeof(test_report));
    ASSERT_EQ(mouse_report.rel_x, test_report.rel_x);
    ASSERT_EQ(mouse_report.rel_y, test_report.rel_y);
  }

  // Check that report descriptors match completely
  {
    auto response = client->GetReportDesc();
    ASSERT_OK(response.status());
    ASSERT_EQ(response.value().desc.count(), sizeof(kBootMouseReportDesc));
    for (size_t i = 0; i < sizeof(kBootMouseReportDesc); i++) {
      if (kBootMouseReportDesc[i] != response.value().desc[i]) {
        printf("Index %ld of the report descriptor doesn't match\n", i);
      }
      EXPECT_EQ(kBootMouseReportDesc[i], response.value().desc[i]);
    }
  }
}

TEST_F(HidDriverTest, BootMouseTestInputReport) {
  // Create a fake mouse device
  fuchsia_hardware_hidctl::wire::HidCtlConfig config;
  config.dev_num = 5;
  config.boot_device = false;
  config.dev_class = 0;
  auto result = hidctl_client_->MakeHidDevice(
      config, fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kBootMouseReportDesc),
                                                      std::size(kBootMouseReportDesc)));
  ASSERT_OK(result.status());
  zx::socket hidctl_socket = std::move(result.value().report_socket);

  // Open the /dev/class/input/ device created by MakeHidDevice.
  zx::result input_client =
      WaitForFirstDevice<fuchsia_input_report::InputDevice>("class/input-report");
  ASSERT_OK(input_client.status_value());
  fidl::WireSyncClient client(std::move(input_client.value()));

  auto reader_endpoints = fidl::CreateEndpoints<fuchsia_input_report::InputReportsReader>();
  ASSERT_OK(reader_endpoints.status_value());
  auto reader = fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(
      std::move(reader_endpoints->client));
  ASSERT_OK(client->GetInputReportsReader(std::move(reader_endpoints->server)).status());

  // Check the Descriptor.
  {
    auto response = client->GetDescriptor();
    ASSERT_OK(response.status());
    ASSERT_TRUE(response.value().descriptor.has_mouse());
    ASSERT_TRUE(response.value().descriptor.mouse().has_input());
    ASSERT_TRUE(response.value().descriptor.mouse().input().has_movement_x());
    ASSERT_TRUE(response.value().descriptor.mouse().input().has_movement_y());
  }

  // Send a single mouse report
  hid_boot_mouse_report_t mouse_report = {};
  mouse_report.rel_x = 50;
  mouse_report.rel_y = 100;
  ASSERT_OK(hidctl_socket.write(0, &mouse_report, sizeof(mouse_report), nullptr));

  // Get the mouse InputReport.
  auto response = reader->ReadInputReports();
  ASSERT_OK(response.status());
  ASSERT_OK(response.status());
  ASSERT_EQ(1, response->value()->reports.count());
  ASSERT_EQ(50, response->value()->reports[0].mouse().movement_x());
  ASSERT_EQ(100, response->value()->reports[0].mouse().movement_y());
}

}  // namespace
