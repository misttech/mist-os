// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "network_device.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>

namespace network {

NetworkDevice::~NetworkDevice() = default;

zx_status_t NetworkDevice::Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher) {
  fbl::AllocChecker ac;
  std::unique_ptr netdev = fbl::make_unique_checked<NetworkDevice>(&ac, parent, dispatcher);
  if (!ac.check()) {
    zxlogf(ERROR, "no memory");
    return ZX_ERR_NO_MEMORY;
  }

  ddk::NetworkDeviceImplProtocolClient netdevice_impl(parent);
  if (!netdevice_impl.is_valid()) {
    zxlogf(ERROR, "bind failed, protocol not available");
    return ZX_ERR_NOT_FOUND;
  }

  zx::result device =
      NetworkDeviceInterface::Create(netdev->dispatcher_, netdevice_impl, netdev.get());
  if (device.is_error()) {
    zxlogf(ERROR, "failed to create inner device %s", device.status_string());
    return device.status_value();
  }
  netdev->device_ = std::move(device.value());

  if (zx_status_t status = netdev->DdkAdd(
          ddk::DeviceAddArgs("network-device").set_proto_id(ZX_PROTOCOL_NETWORK_DEVICE));
      status != ZX_OK) {
    zxlogf(ERROR, "failed to bind %s", zx_status_get_string(status));
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

void NetworkDevice::NotifyThread(zx::unowned_thread thread, ThreadType type) {
  const std::string_view role = [type]() {
    switch (type) {
      case ThreadType::Tx:
        return "fuchsia.devices.network.core.tx";
      case ThreadType::Rx:
        return "fuchsia.devices.network.core.rx";
    }
  }();

  if (!thread->is_valid()) {
    zxlogf(INFO, "thread not present, scheduler role '%.*s' will not be applied",
           static_cast<int>(role.size()), role.data());
    return;
  }

  if (zx_status_t status =
          device_set_profile_by_role(parent(), thread->get(), role.data(), role.size());
      status != ZX_OK) {
    zxlogf(WARNING, "failed to set scheduler role '%.*s': %s", static_cast<int>(role.size()),
           role.data(), zx_status_get_string(status));
  }
}

static constexpr zx_driver_ops_t network_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind =
        +[](void* ctx, zx_device_t* parent) {
          return NetworkDevice::Create(ctx, parent,
                                       fdf::Dispatcher::GetCurrent()->async_dispatcher());
        },
};

}  // namespace network

ZIRCON_DRIVER(network, network::network_driver_ops, "zircon", "0.1");
