// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-bus.h"

#include <assert.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/sync/cpp/completion.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <variant>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/test/platform/cpp/bind.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <usb/usb.h>

namespace usb_virtual_bus {

#define DEVICE_SLOT_ID 0
#define DEVICE_HUB_ID 0
#define DEVICE_SPEED USB_SPEED_HIGH

template <typename T>
zx::result<std::unique_ptr<T>> UsbVirtualBus::CreateChild() {
  fbl::AllocChecker ac;
  auto child = fbl::make_unique_checked<T>(&ac, this);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  {
    zx::result result = outgoing()->AddService<typename T::Service>(child->GetInstanceHandler());
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add service %s", result.status_string());
      return result.take_error();
    }
  }

  {
    zx::result result =
        child->compat_server().Initialize(incoming(), outgoing(), node_name(), T::kName.c_str(),
                                          compat::ForwardMetadata::None(), child->GetBanjoConfig());
    if (result.is_error()) {
      return result.take_error();
    }
  }

  std::vector offers = child->compat_server().CreateOffers2();
  offers.push_back(fdf::MakeOffer2<typename T::Service>());
  auto properties = T::GetProperties();
  properties.push_back(fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST));
  properties.push_back(fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                                         bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_USB));

  {
    zx::result result = AddChild(T::kName.c_str(), properties, offers);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
      return result.take_error();
    }
    child->controller().Bind(std::move(*result));
  }
  return zx::ok(std::move(child));
}

template <typename T>
zx::result<> UsbVirtualBus::RemoveChild(std::unique_ptr<T>& child) {
  if (!child) {
    return zx::ok();
  }

  zx::result<> return_value = zx::ok();
  if (child->controller()) {
    auto result = child->controller()->Remove();
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to remove child: %s", result.FormatDescription().c_str());
      // Continue despite failure.
      return_value = zx::error(result.status());
    }
  }
  child->compat_server().reset();
  {
    zx::result result = outgoing()->RemoveService<typename T::Service>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to remove device service: %s", result.status_string());
      // Continue despite failure.
      return_value = result;
    }
  }
  child.reset();
  return return_value;
}

void UsbVirtualBus::Serve(fidl::ServerEnd<fuchsia_hardware_usb_virtual_bus::Bus> request) {
  bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request), this,
                       fidl::kIgnoreBindingClosure);
}

zx::result<> UsbVirtualBus::Start() {
  auto dispatcher = fdf::SynchronizedDispatcher::Create({}, "usb-virtual-bus-bus-intf",
                                                        [&](fdf_dispatcher_t*) {});
  if (dispatcher.is_error()) {
    FDF_LOG(ERROR, "Failed to create dispatcher %s", dispatcher.status_string());
    return dispatcher.take_error();
  }
  bus_intf_dispatcher_ = std::move(*dispatcher);

  zx::result connector = devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Error creating devfs node");
    return connector.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{
      {.connector = std::move(connector.value()),
       .class_name = "usb-virtual-bus",
       .connector_supports = fuchsia_device_fs::ConnectionType::kController}};

  zx::result child = AddOwnedChild(kName, devfs_args);
  if (child.is_error()) {
    FDF_LOG(ERROR, "Failed to add child %s", child.status_string());
    return child.take_error();
  }
  child_ = std::move(*child);
  return zx::ok();
}

void UsbVirtualBus::SetConnected(bool connected) {
  if (connected && (connected_ == ConnectedState::kDisconnected)) {
    if (dci_intf_.is_valid()) {
      connected_ = ConnectedState::kConnecting;
      dci_intf_->SetConnected(true).Then(
          [](fidl::Result<fuchsia_hardware_usb_dci::UsbDciInterface::SetConnected>& result) {
            if (result.is_error()) {
              FDF_LOG(ERROR, "Failed to set connected");
            }
          });
    }
  } else if (!connected && (connected_ != ConnectedState::kDisconnected)) {
    connected_ = ConnectedState::kDisconnected;
    for (auto& ep : eps_) {
      ep.host_.CommonCancelAll();
      ep.device_.CommonCancelAll();
    }
    async::PostTask(bus_intf_dispatcher_.async_dispatcher(), [this]() {
      if (!bus_intf_.is_valid()) {
        return;
      }
      bus_intf_.RemoveDevice(DEVICE_SLOT_ID);
    });
    if (dci_intf_.is_valid()) {
      dci_intf_->SetConnected(false).Then(
          [](fidl::Result<fuchsia_hardware_usb_dci::UsbDciInterface::SetConnected>& result) {
            if (result.is_error()) {
              FDF_LOG(ERROR, "Failed to set connected");
            }
          });
    }
  }
}

void UsbVirtualBus::FinishConnect() {
  connected_ = ConnectedState::kConnected;
  async::PostTask(bus_intf_dispatcher_.async_dispatcher(), [this]() {
    if (!bus_intf_.is_valid()) {
      return;
    }
    bus_intf_.AddDevice(DEVICE_SLOT_ID, DEVICE_HUB_ID, DEVICE_SPEED);
  });
}

void UsbVirtualBus::PrepareStop(fdf::PrepareStopCompleter completer) {
  zx_status_t status = Disable();
  if (status != ZX_OK) {
    completer(zx::error(status));
    return;
  }
  completer(zx::ok());
}

void UsbVirtualBus::SetBusInterface(const usb_bus_interface_protocol_t* bus_intf) {
  libsync::Completion wait;
  async::PostTask(bus_intf_dispatcher_.async_dispatcher(), [this, &bus_intf, &wait]() {
    if (!bus_intf) {
      // Now that we've finished using bus_intf, signal the main thread.
      wait.Signal();
      bus_intf_.clear();
      return;
    }

    bus_intf_ = ddk::UsbBusInterfaceProtocolClient(bus_intf);
    // Now that we've finished using bus_intf, signal the main thread.
    wait.Signal();

    if (connected_ == ConnectedState::kConnected) {
      bus_intf_.AddDevice(DEVICE_SLOT_ID, DEVICE_HUB_ID, DEVICE_SPEED);
    }
  });
  wait.Wait();
}

zx::result<> UsbVirtualBus::SetDciInterface(
    fidl::ClientEnd<fuchsia_hardware_usb_dci::UsbDciInterface> client_end) {
  if (dci_intf_.is_valid()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }

  dci_intf_.Bind(std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());
  return zx::ok();
}

void UsbVirtualBus::Enable(EnableCompleter::Sync& completer) {
  if (host_ == nullptr) {
    zx::result result = CreateChild<UsbVirtualHost>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create host %s", result.status_string());
      completer.Reply(result.error_value());
      return;
    }
    host_ = std::move(*result);
  }
  if (device_ == nullptr) {
    zx::result result = CreateChild<UsbVirtualDevice>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create device %s", result.status_string());
      completer.Reply(result.error_value());
      return;
    }
    device_ = std::move(*result);
  }

  completer.Reply(ZX_OK);
}

void UsbVirtualBus::Disable(DisableCompleter::Sync& completer) { completer.Reply(Disable()); }

zx_status_t UsbVirtualBus::Disable() {
  SetConnected(false);
  bus_intf_.clear();
  dci_intf_ = {};
  zx::result host_result = RemoveChild(host_);
  if (host_result.is_error()) {
    FDF_LOG(ERROR, "Failed to remove host %s", host_result.status_string());
    // Continue despite failure.
  }
  zx::result device_result = RemoveChild(device_);
  if (host_result.is_error()) {
    FDF_LOG(ERROR, "Failed to remove device %s", device_result.status_string());
    // Continue despite failure.
  }
  return host_result.is_error() ? host_result.error_value() : device_result.status_value();
}

void UsbVirtualBus::Connect(ConnectCompleter::Sync& completer) {
  if (host_ == nullptr || device_ == nullptr) {
    completer.Reply(ZX_ERR_BAD_STATE);
    return;
  }

  SetConnected(true);
  completer.Reply(ZX_OK);
}

void UsbVirtualBus::Disconnect(DisconnectCompleter::Sync& completer) {
  if (host_ == nullptr || device_ == nullptr) {
    completer.Reply(ZX_ERR_BAD_STATE);
    return;
  }

  SetConnected(false);
  completer.Reply(ZX_OK);
}

}  // namespace usb_virtual_bus

FUCHSIA_DRIVER_EXPORT(usb_virtual_bus::UsbVirtualBus);
