// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "network_device.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/fdf/cpp/env.h>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "device/network_device_shim.h"

namespace network {
namespace {

// Creates a `NetworkDeviceImplBinder` based on the `parent` device type.
zx::result<std::unique_ptr<NetworkDeviceImplBinder>> CreateImplBinder(
    ddk::NetworkDeviceImplProtocolClient netdevice_impl, NetworkDevice* device,
    const ShimDispatchers& dispatchers) {
  fbl::AllocChecker ac;

  // If the `parent` is Banjo based, then we must use "shims" to translate between Banjo and FIDL in
  // order to leverage the netdevice core library.
  if (netdevice_impl.is_valid()) {
    auto shim = fbl::make_unique_checked<NetworkDeviceShim>(&ac, netdevice_impl, dispatchers);
    if (!ac.check()) {
      zxlogf(ERROR, "no memory");
      return zx::error(ZX_ERR_NO_MEMORY);
    }

    return zx::ok(std::move(shim));
  }

  // If the `parent` is FIDL based, then return a binder that connects to the device with no extra
  // translation layer.
  std::unique_ptr fidl = fbl::make_unique_checked<FidlNetworkDeviceImplBinder>(&ac, device);
  if (!ac.check()) {
    zxlogf(ERROR, "no memory");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(fidl));
}

}  // namespace

NetworkDevice::~NetworkDevice() {
  if (dispatchers_) {
    dispatchers_->ShutdownSync();
  }
  if (shim_dispatchers_) {
    shim_dispatchers_->ShutdownSync();
  }
}

zx_status_t NetworkDevice::Create(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  std::unique_ptr netdev = fbl::make_unique_checked<NetworkDevice>(&ac, parent);
  if (!ac.check()) {
    zxlogf(ERROR, "no memory");
    return ZX_ERR_NO_MEMORY;
  }

  zx::result dispatchers = OwnedDeviceInterfaceDispatchers::Create();
  if (dispatchers.is_error()) {
    zxlogf(ERROR, "failed to create owned dispatchers: %s", dispatchers.status_string());
    return dispatchers.status_value();
  }
  netdev->dispatchers_ = std::move(dispatchers.value());

  ddk::NetworkDeviceImplProtocolClient netdevice_impl(parent);
  ShimDispatchers unowned_shim_dispatchers;
  if (netdevice_impl.is_valid()) {
    // We only need these dispatchers for Banjo parents.
    zx::result shim_dispatchers = OwnedShimDispatchers::Create();
    if (shim_dispatchers.is_error()) {
      zxlogf(ERROR, "failed to create owned shim dispatchers: %s",
             shim_dispatchers.status_string());
      return shim_dispatchers.status_value();
    }
    netdev->shim_dispatchers_ = std::move(shim_dispatchers.value());
    unowned_shim_dispatchers = netdev->shim_dispatchers_->Unowned();
  }

  zx::result<std::unique_ptr<NetworkDeviceImplBinder>> binder =
      CreateImplBinder(netdevice_impl, netdev.get(), unowned_shim_dispatchers);
  if (binder.is_error()) {
    zxlogf(ERROR, "failed to create network device binder: %s", binder.status_string());
    return binder.status_value();
  }

  zx::result device =
      NetworkDeviceInterface::Create(netdev->dispatchers_->Unowned(), std::move(binder.value()));

  if (device.is_error()) {
    zxlogf(ERROR, "failed to create inner device %s", device.status_string());
    return device.status_value();
  }
  netdev->device_ = std::move(device.value());

  if (zx_status_t status = netdev->DdkAdd(
          ddk::DeviceAddArgs("network-device").set_proto_id(ZX_PROTOCOL_NETWORK_DEVICE));
      status != ZX_OK) {
    zxlogf(ERROR, "failed to bind device: %s", zx_status_get_string(status));
    return status;
  }

  // On successful Add, Devmgr takes ownership (relinquished on DdkRelease),
  // so transfer our ownership to a local var, and let it go out of scope.
  [[maybe_unused]] auto temp_ref = netdev.release();
  return ZX_OK;
}

void NetworkDevice::DdkUnbind(ddk::UnbindTxn unbindTxn) {
  zxlogf(INFO, "%p DdkUnbind", zxdev());
  device_->Teardown([txn = std::move(unbindTxn), this]() mutable {
    zxlogf(INFO, "%p DdkUnbind Done", zxdev());
    txn.Reply();
  });
}

void NetworkDevice::DdkRelease() {
  zxlogf(INFO, "%p DdkRelease", zxdev());
  delete this;
}

void NetworkDevice::GetDevice(GetDeviceRequestView request, GetDeviceCompleter::Sync& _completer) {
  ZX_ASSERT_MSG(device_, "can't serve device if not bound to parent implementation");
  device_->Bind(std::move(request->device));
}

static constexpr zx_driver_ops_t network_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) -> zx_status_t {
      return NetworkDevice::Create(ctx, parent);
    },
};

zx::result<fdf::ClientEnd<fuchsia_hardware_network_driver::NetworkDeviceImpl>>
FidlNetworkDeviceImplBinder::Bind() {
  zx::result<fdf::ClientEnd<fuchsia_hardware_network_driver::NetworkDeviceImpl>> client_end =
      parent_->DdkConnectRuntimeProtocol<
          fuchsia_hardware_network_driver::Service::NetworkDeviceImpl>();
  if (client_end.is_error()) {
    zxlogf(ERROR, "failed to connect to parent device: %s", client_end.status_string());
    return client_end;
  }
  return client_end;
}

}  // namespace network

ZIRCON_DRIVER(network, network::network_driver_ops, "zircon", "0.1");
