// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/usb-virtual-bus-launcher/usb-virtual-bus-launcher.h"

#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/watcher.h>
#include <lib/fidl-async/cpp/bind.h>
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
    std::cout << "IsolatedDevmgr::Create(): " << zx_status_get_string(status) << std::endl;
    return zx::error(status);
  }

  zx::result channel = device_watcher::RecursiveWaitForFile(launcher.devmgr_.devfs_root().get(),
                                                            "sys/platform/11:03:0/usb-virtual-bus");
  if (channel.is_error()) {
    std::cout << "Failed to wait for usb-virtual-bus: " << channel.status_string() << std::endl;
    return channel.take_error();
  }
  launcher.virtual_bus_.Bind(
      fidl::ClientEnd<fuchsia_hardware_usb_virtual_bus::Bus>{std::move(channel.value())});

  const fidl::WireResult enable_result = launcher.virtual_bus_->Enable();
  if (!enable_result.ok()) {
    std::cout << "virtual_bus_->Enable(): " << enable_result.FormatDescription() << std::endl;
    return zx::error(enable_result.status());
  }
  const fidl::WireResponse enable_response = enable_result.value();
  if (zx_status_t status = enable_response.status; status != ZX_OK) {
    std::cout << "virtual_bus_->Enable(): " << zx_status_get_string(status) << std::endl;
    return zx::error(status);
  }

  const fbl::unique_fd fd{
      openat(launcher.devmgr_.devfs_root().get(), "class/usb-peripheral", O_RDONLY)};
  fbl::String devpath;
  if (zx_status_t status = fdio_watch_directory(fd.get(), usb_virtual_bus::WaitForAnyFile,
                                                ZX_TIME_INFINITE, &devpath);
      status != ZX_ERR_STOP) {
    std::cout << "fdio_watch_directory(): " << zx_status_get_string(status) << std::endl;
    return zx::error(status);
  }
  fdio_cpp::UnownedFdioCaller caller(fd);

  zx::result peripheral =
      component::ConnectAt<fuchsia_hardware_usb_peripheral::Device>(caller.directory(), devpath);
  if (peripheral.is_error()) {
    std::cout << "Failed to get USB peripheral service: " << peripheral.status_string()
              << std::endl;
    return peripheral.take_error();
  }
  launcher.peripheral_.Bind(std::move(peripheral.value()));

  if (zx_status_t status = launcher.ClearPeripheralDeviceFunctions(); status != ZX_OK) {
    std::cout << "launcher.ClearPeripheralDeviceFunctions(): " << zx_status_get_string(status)
              << std::endl;
    return zx::error(status);
  }

  return zx::ok(std::move(launcher));
}

zx_status_t BusLauncher::SetupPeripheralDevice(DeviceDescriptor&& device_desc,
                                               std::vector<ConfigurationDescriptor> config_descs) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_usb_peripheral::Events>();
  if (endpoints.is_error()) {
    std::cout << "Failed to create endpoints: " << endpoints.status_string() << std::endl;
    return endpoints.error_value();
  }
  auto& [client, server] = endpoints.value();

  if (const fidl::Status result = peripheral_->SetStateChangeListener(std::move(client));
      !result.ok()) {
    std::cout << "peripheral_->SetStateChangeListener(): " << result.FormatDescription()
              << std::endl;
    return result.status();
  }

  const fidl::WireResult result = peripheral_->SetConfiguration(
      device_desc, fidl::VectorView<ConfigurationDescriptor>::FromExternal(config_descs));
  if (!result.ok()) {
    std::cout << "peripheral_->SetConfiguration(): " << result.FormatDescription() << std::endl;
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    std::cout << "peripheral_->SetConfiguration(): " << zx_status_get_string(response.error_value())
              << std::endl;
    return response.error_value();
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  usb_peripheral_utils::EventWatcher watcher(&loop, std::move(server), 1);

  if (zx_status_t status = loop.Run(); status != ZX_ERR_CANCELED) {
    std::cout << "loop.Run(): " << zx_status_get_string(status) << std::endl;
    return status;
  }
  if (!watcher.all_functions_registered()) {
    std::cout << "watcher.all_functions_registered() returned false" << std::endl;
    return ZX_ERR_INTERNAL;
  }

  auto connect_result = virtual_bus_->Connect();
  if (connect_result.status() != ZX_OK) {
    std::cout << "virtual_bus_->Connect(): " << zx_status_get_string(connect_result.status())
              << std::endl;
    return connect_result.status();
  }
  if (connect_result.value().status != ZX_OK) {
    std::cout << "virtual_bus_->Connect() returned status: "
              << zx_status_get_string(connect_result.value().status) << std::endl;
    return connect_result.value().status;
  }
  return ZX_OK;
}

zx_status_t BusLauncher::ClearPeripheralDeviceFunctions() {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_usb_peripheral::Events>();
  if (endpoints.is_error()) {
    std::cout << "Failed to create endpoints: " << endpoints.status_string() << std::endl;
    return endpoints.error_value();
  }
  auto& [client, server] = endpoints.value();

  if (const fidl::Status result = peripheral_->SetStateChangeListener(std::move(client));
      !result.ok()) {
    std::cout << "peripheral_->SetStateChangeListener(): " << result.FormatDescription()
              << std::endl;
    return result.status();
  }

  auto clear_functions = peripheral_->ClearFunctions();
  if (clear_functions.status() != ZX_OK) {
    std::cout << "peripheral_->ClearFunctions(): " << zx_status_get_string(clear_functions.status())
              << std::endl;
    return clear_functions.status();
  }
  const fidl::WireResult result = peripheral_->ClearFunctions();
  if (!result.ok()) {
    std::cout << "peripheral_->ClearFunctions(): " << result.FormatDescription() << std::endl;
    return result.status();
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  usb_peripheral_utils::EventWatcher watcher(&loop, std::move(server), 1);

  if (zx_status_t status = loop.Run(); status != ZX_ERR_CANCELED) {
    std::cout << "loop.Run(): " << zx_status_get_string(status) << std::endl;
    return status;
  }
  if (!watcher.all_functions_cleared()) {
    std::cout << "watcher.all_functions_cleared() returned false" << std::endl;
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

int BusLauncher::GetRootFd() { return devmgr_.devfs_root().get(); }

zx_status_t BusLauncher::Disable() {
  auto result = virtual_bus_->Disable();
  if (result.status() != ZX_OK) {
    std::cout << "virtual_bus_->Disable(): " << zx_status_get_string(result.status()) << std::endl;
    return result.status();
  }
  if (result.value().status != ZX_OK) {
    std::cout << "virtual_bus_->Disable() returned status: "
              << zx_status_get_string(result.value().status) << std::endl;
    return result.value().status;
  }
  return result.value().status;
}

zx_status_t BusLauncher::Disconnect() {
  auto result = virtual_bus_->Disconnect();
  if (result.status() != ZX_OK) {
    std::cout << "virtual_bus_->Disconnect(): " << zx_status_get_string(result.status())
              << std::endl;
    return result.status();
  }
  if (result.value().status != ZX_OK) {
    std::cout << "virtual_bus_->Disconnect() returned status: "
              << zx_status_get_string(result.value().status) << std::endl;
    return result.value().status;
  }
  return ZX_OK;
}

}  // namespace usb_virtual
