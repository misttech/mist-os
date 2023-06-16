// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/usb-virtual-bus-launcher/usb-virtual-bus-launcher.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/usb-peripheral-utils/event-watcher.h>
#include <lib/usb-virtual-bus-launcher-helper/usb-virtual-bus-launcher-helper.h>
#include <lib/zx/channel.h>
#include <zircon/status.h>

#include <iostream>

#include <fbl/string.h>
#include <fbl/unique_fd.h>
#include <usb/usb.h>

namespace usb_virtual {

using driver_integration_test::IsolatedDevmgr;

zx::result<BusLauncher> BusLauncher::Create(IsolatedDevmgr::Args args) {
  args.disable_block_watcher = true;

  board_test::DeviceEntry dev = {};
  dev.did = 0;
  dev.vid = PDEV_VID_TEST;
  dev.pid = PDEV_PID_USB_VBUS_TEST;
  args.device_list.push_back(dev);

  BusLauncher launcher;

  if (zx_status_t status = IsolatedDevmgr::Create(&args, &launcher.devmgr_); status != ZX_OK) {
    std::cout << "IsolatedDevmgr::Create(): " << zx_status_get_string(status) << '\n';
    return zx::error(status);
  }

  zx::result channel = device_watcher::RecursiveWaitForFile(launcher.devmgr_.devfs_root().get(),
                                                            "sys/platform/11:03:0/usb-virtual-bus");
  if (channel.is_error()) {
    std::cout << "Failed to wait for usb-virtual-bus: " << channel.status_string() << '\n';
    return channel.take_error();
  }
  launcher.virtual_bus_.Bind(
      fidl::ClientEnd<fuchsia_hardware_usb_virtual_bus::Bus>{std::move(channel.value())});

  const fidl::WireResult enable_result = launcher.virtual_bus_->Enable();
  if (!enable_result.ok()) {
    std::cout << "virtual_bus_->Enable(): " << enable_result.FormatDescription() << '\n';
    return zx::error(enable_result.status());
  }
  const fidl::WireResponse enable_response = enable_result.value();
  if (zx_status_t status = enable_response.status; status != ZX_OK) {
    std::cout << "virtual_bus_->Enable(): " << zx_status_get_string(status) << '\n';
    return zx::error(status);
  }

  fdio_cpp::UnownedFdioCaller caller(launcher.devmgr_.devfs_root());

  zx::result directory_result =
      component::ConnectAt<fuchsia_io::Directory>(caller.directory(), "class/usb-peripheral");
  if (directory_result.is_error()) {
    std::cout << "component::ConnectAt(): " << directory_result.status_string() << '\n';
    return directory_result.take_error();
  }
  auto& directory = directory_result.value();
  zx::result watch_result = device_watcher::WatchDirectoryForItems<
      zx::result<fidl::ClientEnd<fuchsia_hardware_usb_peripheral::Device>>>(
      directory, [&directory](std::string_view devpath) {
        return component::ConnectAt<fuchsia_hardware_usb_peripheral::Device>(directory, devpath);
      });
  if (watch_result.is_error()) {
    std::cout << "WatchDirectoryForItems(): " << watch_result.status_string() << '\n';
    return watch_result.take_error();
  }
  auto& peripheral = watch_result.value();
  if (peripheral.is_error()) {
    std::cout << "Failed to get USB peripheral service: " << peripheral.status_string() << '\n';
    return peripheral.take_error();
  }
  launcher.peripheral_.Bind(std::move(peripheral.value()));

  if (zx_status_t status = launcher.ClearPeripheralDeviceFunctions(); status != ZX_OK) {
    std::cout << "launcher.ClearPeripheralDeviceFunctions(): " << zx_status_get_string(status)
              << '\n';
    return zx::error(status);
  }

  return zx::ok(std::move(launcher));
}

zx_status_t BusLauncher::SetupPeripheralDevice(DeviceDescriptor&& device_desc,
                                               std::vector<ConfigurationDescriptor> config_descs) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_usb_peripheral::Events>();
  if (endpoints.is_error()) {
    std::cout << "Failed to create endpoints: " << endpoints.status_string() << '\n';
    return endpoints.error_value();
  }
  auto& [client, server] = endpoints.value();

  if (const fidl::Status result = peripheral_->SetStateChangeListener(std::move(client));
      !result.ok()) {
    std::cout << "peripheral_->SetStateChangeListener(): " << result.FormatDescription() << '\n';
    return result.status();
  }

  const fidl::WireResult result = peripheral_->SetConfiguration(
      device_desc, fidl::VectorView<ConfigurationDescriptor>::FromExternal(config_descs));
  if (!result.ok()) {
    std::cout << "peripheral_->SetConfiguration(): " << result.FormatDescription() << '\n';
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    std::cout << "peripheral_->SetConfiguration(): " << zx_status_get_string(response.error_value())
              << '\n';
    return response.error_value();
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  usb_peripheral_utils::EventWatcher watcher(loop, std::move(server), 1);

  if (zx_status_t status = loop.Run(); status != ZX_ERR_CANCELED) {
    std::cout << "loop.Run(): " << zx_status_get_string(status) << '\n';
    return status;
  }
  if (!watcher.all_functions_registered()) {
    std::cout << "watcher.all_functions_registered() returned false" << '\n';
    return ZX_ERR_INTERNAL;
  }

  auto connect_result = virtual_bus_->Connect();
  if (connect_result.status() != ZX_OK) {
    std::cout << "virtual_bus_->Connect(): " << zx_status_get_string(connect_result.status())
              << '\n';
    return connect_result.status();
  }
  if (connect_result.value().status != ZX_OK) {
    std::cout << "virtual_bus_->Connect() returned status: "
              << zx_status_get_string(connect_result.value().status) << '\n';
    return connect_result.value().status;
  }
  return ZX_OK;
}

zx_status_t BusLauncher::ClearPeripheralDeviceFunctions() {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_usb_peripheral::Events>();
  if (endpoints.is_error()) {
    std::cout << "Failed to create endpoints: " << endpoints.status_string() << '\n';
    return endpoints.error_value();
  }
  auto& [client, server] = endpoints.value();

  if (const fidl::Status result = peripheral_->SetStateChangeListener(std::move(client));
      !result.ok()) {
    std::cout << "peripheral_->SetStateChangeListener(): " << result.FormatDescription() << '\n';
    return result.status();
  }

  const fidl::WireResult result = peripheral_->ClearFunctions();
  if (!result.ok()) {
    std::cout << "peripheral_->ClearFunctions(): " << result.FormatDescription() << '\n';
    return result.status();
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  usb_peripheral_utils::EventWatcher watcher(loop, std::move(server), 1);

  if (zx_status_t status = loop.Run(); status != ZX_ERR_CANCELED) {
    std::cout << "loop.Run(): " << zx_status_get_string(status) << '\n';
    return status;
  }
  if (!watcher.all_functions_cleared()) {
    std::cout << "watcher.all_functions_cleared() returned false" << '\n';
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

int BusLauncher::GetRootFd() { return devmgr_.devfs_root().get(); }

zx_status_t BusLauncher::Disable() {
  auto result = virtual_bus_->Disable();
  if (result.status() != ZX_OK) {
    std::cout << "virtual_bus_->Disable(): " << zx_status_get_string(result.status()) << '\n';
    return result.status();
  }
  if (result.value().status != ZX_OK) {
    std::cout << "virtual_bus_->Disable() returned status: "
              << zx_status_get_string(result.value().status) << '\n';
    return result.value().status;
  }
  return result.value().status;
}

zx_status_t BusLauncher::Disconnect() {
  auto result = virtual_bus_->Disconnect();
  if (result.status() != ZX_OK) {
    std::cout << "virtual_bus_->Disconnect(): " << zx_status_get_string(result.status()) << '\n';
    return result.status();
  }
  if (result.value().status != ZX_OK) {
    std::cout << "virtual_bus_->Disconnect() returned status: "
              << zx_status_get_string(result.value().status) << '\n';
    return result.value().status;
  }
  return ZX_OK;
}

}  // namespace usb_virtual
