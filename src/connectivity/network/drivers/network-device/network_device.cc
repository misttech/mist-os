// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "network_device.h"

#include <lib/driver/compat/cpp/banjo_client.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fdf/cpp/env.h>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "device/network_device_shim.h"

namespace network {
namespace {

constexpr char kDriverName[] = "network-device";
constexpr char kDevFsClassName[] = "network";
constexpr char kDevFsChildNodeName[] = "network-device";

}  // namespace

NetworkDevice::NetworkDevice(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

NetworkDevice::~NetworkDevice() {
  if (dispatchers_) {
    dispatchers_->ShutdownSync();
  }
  if (shim_dispatchers_) {
    shim_dispatchers_->ShutdownSync();
  }
}

void NetworkDevice::Start(fdf::StartCompleter completer) {
  zx::result<> result = [&]() -> zx::result<> {
    zx::result dispatchers = OwnedDeviceInterfaceDispatchers::Create();
    if (dispatchers.is_error()) {
      FDF_LOG(ERROR, "failed to create owned dispatchers: %s", dispatchers.status_string());
      return dispatchers.take_error();
    }
    dispatchers_ = std::move(dispatchers.value());

    zx::result<std::unique_ptr<NetworkDeviceImplBinder>> binder = CreateImplBinder();
    if (binder.is_error()) {
      FDF_LOG(ERROR, "failed to create network device binder: %s", binder.status_string());
      return binder.take_error();
    }

    zx::result device =
        NetworkDeviceInterface::Create(dispatchers_->Unowned(), std::move(binder.value()));
    if (device.is_error()) {
      FDF_LOG(ERROR, "failed to create inner device %s", device.status_string());
      return device.take_error();
    }
    device_ = std::move(device.value());

    // Create a devfs connector and child node for netcfg to discover and connect to.
    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      FDF_LOG(ERROR, "failed to bind devfs connector: %s", connector.status_string());
      return connector.take_error();
    }

    fuchsia_driver_framework::DevfsAddArgs devfs_args;
    devfs_args.connector(std::move(connector.value()))
        .class_name(kDevFsClassName)
        .connector_supports(fuchsia_device_fs::ConnectionType::kController);

    // Use AddOwnedChild to prevent other drivers from binding to the node. The node only exists for
    // netcfg to discover and connect to, no other drivers are involved.
    zx::result child = AddOwnedChild(kDevFsChildNodeName, devfs_args);
    if (child.is_error()) {
      FDF_LOG(ERROR, "failed to add child node: %s", child.status_string());
      return child.take_error();
    }

    child_node_.Bind(std::move(child->node_));
    return zx::ok();
  }();

  if (result.is_error() && device_) {
    // Start failed but got to the point where the device was created. We must tear it down.
    // PrepareStop will not be called if Start fails.
    device_->Teardown([result, completer = std::move(completer)]() mutable { completer(result); });
  } else {
    completer(result);
  }
}

void NetworkDevice::PrepareStop(fdf::PrepareStopCompleter completer) {
  FDF_LOG(INFO, "%p PrepareStop", this);
  device_->Teardown([completer = std::move(completer), this]() mutable {
    FDF_LOG(INFO, "%p PrepareStop Done", this);
    completer(zx::ok());
  });
}

void NetworkDevice::GetDevice(GetDeviceRequestView request, GetDeviceCompleter::Sync& _completer) {
  ZX_ASSERT_MSG(device_, "can't serve device if not bound to parent implementation");
  device_->Bind(std::move(request->device));
}

void NetworkDevice::Connect(fidl::ServerEnd<fuchsia_hardware_network::DeviceInstance> request) {
  bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

zx::result<std::unique_ptr<NetworkDeviceImplBinder>> NetworkDevice::CreateImplBinder() {
  fbl::AllocChecker ac;

  // If the `parent` is Banjo based, then we must use "shims" to translate between Banjo and FIDL in
  // order to leverage the netdevice core library.
  zx::result banjo_client = compat::ConnectBanjo<ddk::NetworkDeviceImplProtocolClient>(incoming());
  if (banjo_client.is_ok() && banjo_client->is_valid()) {
    // These dispatchers are only needed for Banjo parents.
    zx::result shim_dispatchers = OwnedShimDispatchers::Create();
    if (shim_dispatchers.is_error()) {
      FDF_LOG(ERROR, "failed to create owned shim dispatchers: %s",
              shim_dispatchers.status_string());
      return shim_dispatchers.take_error();
    }
    shim_dispatchers_ = std::move(shim_dispatchers.value());

    auto shim = fbl::make_unique_checked<NetworkDeviceShim>(&ac, banjo_client.value(),
                                                            shim_dispatchers_->Unowned());
    if (!ac.check()) {
      FDF_LOG(ERROR, "no memory");
      return zx::error(ZX_ERR_NO_MEMORY);
    }
    return zx::ok(std::move(shim));
  }

  // If the `parent` is FIDL based, then return a binder that connects to the device with no extra
  // translation layer.
  std::unique_ptr fidl = fbl::make_unique_checked<FidlNetworkDeviceImplBinder>(&ac, incoming());
  if (!ac.check()) {
    FDF_LOG(ERROR, "no memory");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(fidl));
}

zx::result<fdf::ClientEnd<fuchsia_hardware_network_driver::NetworkDeviceImpl>>
FidlNetworkDeviceImplBinder::Bind() {
  std::shared_ptr incoming = incoming_.lock();
  if (!incoming) {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }
  zx::result client_end =
      incoming->Connect<fuchsia_hardware_network_driver::Service::NetworkDeviceImpl>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "failed to connect to parent device: %s", client_end.status_string());
    return client_end;
  }
  return client_end;
}

}  // namespace network

FUCHSIA_DRIVER_EXPORT(::network::NetworkDevice);
