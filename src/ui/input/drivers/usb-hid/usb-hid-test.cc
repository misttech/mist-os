// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <endian.h>
#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.input/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.peripheral/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.virtual.bus/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/watcher.h>
#include <lib/fit/function.h>
#include <lib/usb-virtual-bus-launcher/usb-virtual-bus-launcher.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <fbl/string.h>
#include <hid/boot.h>
#include <usb/usb.h>
#include <zxtest/zxtest.h>

namespace usb_virtual_bus {
namespace {

using usb_virtual::BusLauncher;

std::string GetDeviceControllerPath(std::string_view dev_path) {
  auto dev_path_modified = std::string(dev_path);
  return dev_path_modified.append("/device_controller");
}

class UsbHidTest : public zxtest::Test {
 public:
  void SetUp() override {
    auto bus = BusLauncher::Create();
    ASSERT_OK(bus.status_value());
    bus_ = std::move(bus.value());

    auto usb_hid_function_desc = GetConfigDescriptor();
    ASSERT_NO_FATAL_FAILURE(InitUsbHid(&devpath_, usb_hid_function_desc));

    fdio_cpp::UnownedFdioCaller caller(bus_->GetRootFd());
    zx::result controller =
        component::ConnectAt<fuchsia_hardware_input::Controller>(caller.directory(), devpath_);
    ASSERT_OK(controller);
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_input::Device>();
    ASSERT_OK(endpoints);
    auto& [device, server] = endpoints.value();
    ASSERT_OK(fidl::WireCall(controller.value())->OpenSession(std::move(server)));

    sync_client_ = fidl::WireSyncClient<fuchsia_hardware_input::Device>(std::move(device));
  }

  void TearDown() override {
    ASSERT_OK(bus_->ClearPeripheralDeviceFunctions());
    ASSERT_OK(bus_->Disable());
  }

 protected:
  virtual fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor GetConfigDescriptor() = 0;

  // Initialize a Usb HID device. Asserts on failure.
  void InitUsbHid(fbl::String* devpath,
                  fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor desc) {
    namespace usb_peripheral = fuchsia_hardware_usb_peripheral;
    std::vector<usb_peripheral::wire::FunctionDescriptor> function_descs = {desc};
    ASSERT_OK(bus_->SetupPeripheralDevice(
        {
            .bcd_usb = htole16(0x0200),
            .b_max_packet_size0 = 64,
            .id_vendor = htole16(0x18d1),
            .id_product = htole16(0xaf10),
            .bcd_device = htole16(0x0100),
            .b_num_configurations = 1,
        },
        {fidl::VectorView<usb_peripheral::wire::FunctionDescriptor>::FromExternal(
            function_descs)}));

    fbl::unique_fd fd(openat(bus_->GetRootFd(), "class/input", O_RDONLY));
    while (fdio_watch_directory(fd.get(), WaitForAnyFile, ZX_TIME_INFINITE, devpath) !=
           ZX_ERR_STOP) {
    }
    *devpath = fbl::String::Concat({fbl::String("class/input/"), *devpath});
  }

  // Unbinds Usb HID driver from host.
  void Unbind(const fbl::String& devpath) {
    fdio_cpp::UnownedFdioCaller caller(bus_->GetRootFd());

    zx::result input_controller = component::ConnectAt<fuchsia_device::Controller>(
        caller.directory(), GetDeviceControllerPath(devpath_.data()));
    ASSERT_OK(input_controller);
    const fidl::WireResult result = fidl::WireCall(input_controller.value())->GetTopologicalPath();
    ASSERT_OK(result);
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    const std::string_view hid_device_abspath = response->path.get();
    constexpr std::string_view kDev = "/dev/";
    ASSERT_TRUE(cpp20::starts_with(hid_device_abspath, kDev));
    const std::string_view hid_device_relpath = hid_device_abspath.substr(kDev.size());
    const std::string_view usb_hid_relpath =
        hid_device_relpath.substr(0, hid_device_relpath.find_last_of('/'));

    zx::result usb_hid_controller = component::ConnectAt<fuchsia_device::Controller>(
        caller.directory(), GetDeviceControllerPath(usb_hid_relpath));
    ASSERT_OK(usb_hid_controller);
    const size_t last_slash = usb_hid_relpath.find_last_of('/');
    const std::string_view suffix = usb_hid_relpath.substr(last_slash + 1);
    std::string ifc_path{usb_hid_relpath.substr(0, last_slash)};
    fbl::unique_fd fd_usb_hid_parent(
        openat(bus_->GetRootFd(), ifc_path.c_str(), O_DIRECTORY | O_RDONLY));
    ASSERT_TRUE(fd_usb_hid_parent, "openat(_, %s, _): %s", ifc_path.c_str(), strerror(errno));
    std::unique_ptr<device_watcher::DirWatcher> watcher;

    ASSERT_OK(device_watcher::DirWatcher::Create(fd_usb_hid_parent.get(), &watcher));
    {
      const fidl::WireResult result = fidl::WireCall(usb_hid_controller.value())->ScheduleUnbind();
      ASSERT_OK(result.status());
      const fit::result response = result.value();
      ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
    }
    ASSERT_OK(watcher->WaitForRemoval(suffix, zx::duration::infinite()));
  }

  std::optional<BusLauncher> bus_;
  fbl::String devpath_;
  fidl::WireSyncClient<fuchsia_hardware_input::Device> sync_client_;
};

class UsbOneEndpointTest : public UsbHidTest {
 protected:
  fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor GetConfigDescriptor() override {
    return fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor{
        .interface_class = USB_CLASS_HID,
        .interface_subclass = 0,
        .interface_protocol = USB_PROTOCOL_TEST_HID_ONE_ENDPOINT,
    };
  }
};

TEST_F(UsbOneEndpointTest, GetDeviceIdsVidPid) {
  // Check USB device descriptor VID/PID plumbing.
  auto result = sync_client_->GetDeviceIds();
  ASSERT_OK(result.status());
  EXPECT_EQ(0x18d1, result.value().ids.vendor_id);
  EXPECT_EQ(0xaf10, result.value().ids.product_id);
}

TEST_F(UsbOneEndpointTest, SetAndGetReport) {
  uint8_t buf[sizeof(hid_boot_mouse_report_t)] = {0xab, 0xbc, 0xde};

  auto set_result = sync_client_->SetReport(fuchsia_hardware_input::wire::ReportType::kInput, 0,
                                            fidl::VectorView<uint8_t>::FromExternal(buf));
  auto get_result = sync_client_->GetReport(fuchsia_hardware_input::wire::ReportType::kInput, 0);

  ASSERT_OK(set_result.status());
  ASSERT_OK(set_result.value().status);

  ASSERT_OK(get_result.status());
  ASSERT_OK(get_result.value().status);

  ASSERT_EQ(get_result.value().report.count(), sizeof(hid_boot_mouse_report_t));
  ASSERT_EQ(0xab, get_result.value().report[0]);
  ASSERT_EQ(0xbc, get_result.value().report[1]);
  ASSERT_EQ(0xde, get_result.value().report[2]);
}

TEST_F(UsbOneEndpointTest, UnBind) { ASSERT_NO_FATAL_FAILURE(Unbind(devpath_)); }

class UsbTwoEndpointTest : public UsbHidTest {
 protected:
  fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor GetConfigDescriptor() override {
    return fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor{
        .interface_class = USB_CLASS_HID,
        .interface_subclass = 0,
        .interface_protocol = USB_PROTOCOL_TEST_HID_TWO_ENDPOINT,
    };
  }
};

TEST_F(UsbTwoEndpointTest, SetAndGetReport) {
  uint8_t buf[sizeof(hid_boot_mouse_report_t)] = {0xab, 0xbc, 0xde};

  auto set_result = sync_client_->SetReport(fuchsia_hardware_input::wire::ReportType::kInput, 0,
                                            fidl::VectorView<uint8_t>::FromExternal(buf));
  auto get_result = sync_client_->GetReport(fuchsia_hardware_input::wire::ReportType::kInput, 0);

  ASSERT_OK(set_result.status());
  ASSERT_OK(set_result.value().status);

  ASSERT_OK(get_result.status());
  ASSERT_OK(get_result.value().status);

  ASSERT_EQ(get_result.value().report.count(), sizeof(hid_boot_mouse_report_t));
  ASSERT_EQ(0xab, get_result.value().report[0]);
  ASSERT_EQ(0xbc, get_result.value().report[1]);
  ASSERT_EQ(0xde, get_result.value().report[2]);
}

TEST_F(UsbTwoEndpointTest, UnBind) { ASSERT_NO_FATAL_FAILURE(Unbind(devpath_)); }

}  // namespace
}  // namespace usb_virtual_bus
