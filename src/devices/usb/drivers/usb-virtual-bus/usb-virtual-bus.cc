// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-bus.h"

#include <assert.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/sync/cpp/completion.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <variant>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/test/platform/cpp/bind.h>
#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <usb/usb.h>

namespace usb_virtual_bus {

#define DEVICE_SLOT_ID 0
#define DEVICE_HUB_ID 0
#define DEVICE_SPEED USB_SPEED_HIGH

zx_status_t UsbVirtualBus::Create(zx_device_t* parent) {
  fbl::AllocChecker ac;
  auto bus = fbl::make_unique_checked<UsbVirtualBus>(
      &ac, parent, fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto status = bus->Init();
  if (status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dummy = bus.release();
  return ZX_OK;
}

zx_status_t UsbVirtualBus::CreateDevice() {
  fbl::AllocChecker ac;
  device_ = fbl::make_unique_checked<UsbVirtualDevice>(&ac, zxdev(), this);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    device_ = nullptr;
    return endpoints.status_value();
  }
  zx::result result = outgoing_.AddService<fuchsia_hardware_usb_dci::UsbDciService>(
      fuchsia_hardware_usb_dci::UsbDciService::InstanceHandler({
          .device =
              dci_bindings_.CreateHandler(device_.get(), dispatcher_, fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service");
    device_ = nullptr;
    return result.status_value();
  }
  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to service the outgoing directory");
    device_ = nullptr;
    return result.status_value();
  }

  const zx_device_str_prop_t props[] = {
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID,
                           bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID,
                           bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_USB),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID,
                           bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_VIRTUAL_DEVICE),
  };

  std::array offers = {
      fuchsia_hardware_usb_dci::UsbDciService::Name,
  };
  auto status = device_->DdkAdd(ddk::DeviceAddArgs("usb-virtual-device")
                                    .set_str_props(props)
                                    .set_fidl_service_offers(offers)
                                    .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    device_ = nullptr;
    return status;
  }

  return ZX_OK;
}

zx_status_t UsbVirtualBus::CreateHost() {
  fbl::AllocChecker ac;
  host_ = fbl::make_unique_checked<UsbVirtualHost>(&ac, zxdev(), this);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    host_ = nullptr;
    return endpoints.status_value();
  }
  zx::result result = outgoing_.AddService<fuchsia_hardware_usb_hci::UsbHciService>(
      fuchsia_hardware_usb_hci::UsbHciService::InstanceHandler({
          .device =
              hci_bindings_.CreateHandler(host_.get(), dispatcher_, fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service");
    host_ = nullptr;
    return result.status_value();
  }
  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to service the outgoing directory");
    host_ = nullptr;
    return result.status_value();
  }

  const zx_device_str_prop_t props[] = {
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID,
                           bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID,
                           bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_USB),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID,
                           bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_VIRTUAL_BUS),
  };

  std::array offers = {
      fuchsia_hardware_usb_hci::UsbHciService::Name,
  };
  auto status = host_->DdkAdd(ddk::DeviceAddArgs("usb-virtual-host")
                                  .set_str_props(props)
                                  .set_fidl_service_offers(offers)
                                  .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    host_ = nullptr;
    return status;
  }

  return ZX_OK;
}

zx_status_t UsbVirtualBus::Init() { return DdkAdd("usb-virtual-bus", DEVICE_ADD_NON_BINDABLE); }

void UsbVirtualBus::DdkInit(ddk::InitTxn txn) {
  auto result = fdf::SynchronizedDispatcher::Create(
      {}, "usb-virtual-bus-device-thread",
      [this](fdf_dispatcher_t*) { device_dispatcher_shutdown_wait_.Signal(); });
  if (!result.is_ok()) {
    return txn.Reply(result.status_value());
  }
  device_dispatcher_ = std::move(*result);
  return txn.Reply(ZX_OK);
}

void UsbVirtualBus::SetConnected(bool connected) {
  bool was_connected = connected;
  {
    fbl::AutoLock lock(&connection_lock_);
    std::swap(connected_, was_connected);
  }
  if (connected && !was_connected) {
    if (bus_intf_.is_valid()) {
      bus_intf_.AddDevice(DEVICE_SLOT_ID, DEVICE_HUB_ID, DEVICE_SPEED);
    }
    if (dci_intf_.is_valid()) {
      dci_intf_.SetConnected(true);
    }
  } else if (!connected && was_connected) {
    for (auto& ep : eps_) {
      std::queue<RequestVariant> queue;
      {
        fbl::AutoLock l(&lock_);
        fbl::AutoLock l2(&device_lock_);
        queue = std::move(ep.host_reqs);
        for (auto req = ep.device_reqs.pop(); req; req = ep.device_reqs.pop()) {
          queue.push(std::move(*req));
        }
      }
      while (!queue.empty()) {
        ep.RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(queue.front()));
        queue.pop();
      }
    }
    if (bus_intf_.is_valid()) {
      bus_intf_.RemoveDevice(DEVICE_SLOT_ID);
    }
    if (dci_intf_.is_valid()) {
      dci_intf_.SetConnected(false);
    }
  }
}

void UsbVirtualBus::DdkUnbind(ddk::UnbindTxn txn) {
  {
    fbl::AutoLock lock(&lock_);
    fbl::AutoLock lock2(&device_lock_);
    unbinding_ = true;
    // The device thread will reply to the unbind txn when ready.
    unbind_txn_ = std::move(txn);
    process_requests_.Post(device_dispatcher_.async_dispatcher());
  }
  auto* host = host_.release();
  if (host) {
    host->DdkAsyncRemove();
  }
  auto* device = device_.release();
  if (device) {
    device->DdkAsyncRemove();
  }
}

void UsbVirtualBus::DdkChildPreRelease(void* child_ctx) {
  if (host_.get() == child_ctx) {
    [[maybe_unused]] auto* host = host_.release();
    return;
  }

  if (device_.get() == child_ctx) {
    [[maybe_unused]] auto* device = device_.release();
    return;
  }
}

void UsbVirtualBus::DdkRelease() {
  device_dispatcher_.ShutdownAsync();
  if (disconnect_completer_) {
    thrd_join(disconnect_thread_, nullptr);
  }
  if (disable_completer_) {
    thrd_join(disable_thread_, nullptr);
  }
  device_dispatcher_shutdown_wait_.Wait();
  delete this;
}

zx_status_t UsbVirtualBus::UsbDciCancelAll(uint8_t endpoint) {
  uint8_t index = EpAddressToIndex(endpoint);
  if (index == 0 || index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }
  RequestQueue q;
  {
    fbl::AutoLock l(&device_lock_);
    q = std::move(eps_[index].device_reqs);
  }
  for (auto req = q.pop(); req; req = q.pop()) {
    req->Complete(ZX_ERR_IO_NOT_PRESENT, 0);
  }
  return ZX_OK;
}

void UsbVirtualBus::UsbDciRequestQueue(usb_request_t* req,
                                       const usb_request_complete_callback_t* complete_cb) {
  Request request(req, *complete_cb, sizeof(usb_request_t));

  uint8_t index = EpAddressToIndex(request.request()->header.ep_address);
  if (index == 0 || index >= USB_MAX_EPS) {
    printf("%s: bad endpoint %u\n", __func__, request.request()->header.ep_address);
    request.Complete(ZX_ERR_INVALID_ARGS, 0);
    return;
  }
  // NOTE: Don't check if we're connected here, because the DCI interface
  // may come up before the virtual cable is connected.
  // The functions have no way of knowing if the cable is connected
  // so we need to allow them to queue up requeste here in case
  // we're in the bringup phase, and the request is queued before the cable is connected.
  // (otherwise requests will never be completed).
  // The same is not true for the host side, which is why these are different.

  fbl::AutoLock lock(&device_lock_);
  if (unbinding_) {
    lock.release();
    request.Complete(ZX_ERR_IO_REFUSED, 0);
    return;
  }
  eps_[index].device_reqs.push(std::move(request));

  process_requests_.Post(device_dispatcher_.async_dispatcher());
}

zx_status_t UsbVirtualBus::UsbDciSetInterface(const usb_dci_interface_protocol_t* dci_intf) {
  if (dci_intf) {
    dci_intf_ = ddk::UsbDciInterfaceProtocolClient(dci_intf);
  } else {
    dci_intf_.clear();
  }
  return ZX_OK;
}

zx_status_t UsbVirtualBus::UsbDciConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                                          const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
  uint8_t index = EpAddressToIndex(ep_desc->b_endpoint_address);
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }

  UsbVirtualEp& ep = eps_[index];
  ep.max_packet_size = usb_ep_max_packet(ep_desc);
  return ZX_OK;
}

zx_status_t UsbVirtualBus::UsbDciEpSetStall(uint8_t ep_address) {
  return SetStall(ep_address, true);
}

zx_status_t UsbVirtualBus::UsbDciEpClearStall(uint8_t ep_address) {
  return SetStall(ep_address, false);
}

void UsbVirtualBus::UsbHciRequestQueue(usb_request_t* req,
                                       const usb_request_complete_callback_t* complete_cb) {
  Request request(req, *complete_cb, sizeof(usb_request_t));

  uint8_t index = EpAddressToIndex(request.request()->header.ep_address);
  if (index >= USB_MAX_EPS) {
    printf("usb_virtual_bus_host_queue bad endpoint %u\n", request.request()->header.ep_address);
    request.Complete(ZX_ERR_INVALID_ARGS, 0);
    return;
  }

  eps_[index].QueueRequest(std::move(request));
}

void UsbVirtualBus::UsbHciSetBusInterface(const usb_bus_interface_protocol_t* bus_intf) {
  if (bus_intf) {
    bus_intf_ = ddk::UsbBusInterfaceProtocolClient(bus_intf);

    bool connected;
    {
      fbl::AutoLock al(&connection_lock_);
      connected = connected_;
    }

    if (connected && bus_intf_.is_valid()) {
      bus_intf_.AddDevice(DEVICE_SLOT_ID, DEVICE_HUB_ID, DEVICE_SPEED);
    }
  } else {
    bus_intf_.clear();
  }
}

zx_status_t UsbVirtualBus::UsbHciCancelAll(uint32_t device_id, uint8_t ep_address) {
  uint8_t index = EpAddressToIndex(ep_address);
  return eps_[index].CancelAll().status_value();
}

void UsbVirtualBus::Enable(EnableCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);

  zx_status_t status = ZX_OK;

  if (host_ == nullptr) {
    status = CreateHost();
  }
  if (status == ZX_OK && device_ == nullptr) {
    status = CreateDevice();
  }

  completer.Reply(status);
}

void UsbVirtualBus::Disable(DisableCompleter::Sync& completer) {
  if (disable_completer_) {
    completer.Reply(ZX_ERR_BAD_STATE);
    return;
  }

  disable_completer_ = completer.ToAsync();

  int rc = thrd_create_with_name(
      &disable_thread_,
      [](void* arg) -> int {
        auto virtual_bus = reinterpret_cast<UsbVirtualBus*>(arg);
        virtual_bus->SetConnected(false);
        UsbVirtualHost* host;
        UsbVirtualDevice* device;
        {
          fbl::AutoLock lock(&virtual_bus->lock_);
          // Use release() here to avoid double free of these objects.
          // devmgr will handle freeing them.
          host = virtual_bus->host_.release();
          device = virtual_bus->device_.release();
        }
        if (host) {
          host->DdkAsyncRemove();
        }
        if (device) {
          device->DdkAsyncRemove();
        }
        virtual_bus->disable_completer_->Reply(ZX_OK);
        virtual_bus->disable_completer_.reset();
        return 0;
      },
      reinterpret_cast<void*>(this), "usb-virtual-bus-disable-thread");

  if (rc != thrd_success) {
    disable_completer_->Reply(ZX_ERR_INTERNAL);
    return;
  }
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
  if (host_ == nullptr || device_ == nullptr || disconnect_completer_) {
    completer.Reply(ZX_ERR_BAD_STATE);
    return;
  }

  disconnect_completer_ = completer.ToAsync();

  int rc = thrd_create_with_name(
      &disconnect_thread_,
      [](void* arg) -> int {
        auto virtual_bus = reinterpret_cast<UsbVirtualBus*>(arg);
        virtual_bus->SetConnected(false);
        virtual_bus->disconnect_completer_->Reply(ZX_OK);
        virtual_bus->disconnect_completer_.reset();
        return 0;
      },
      reinterpret_cast<void*>(this), "usb-virtual-bus-disconnect-thread");

  if (rc != thrd_success) {
    disconnect_completer_->Reply(ZX_ERR_INTERNAL);
    return;
  }
}

static zx_status_t usb_virtual_bus_bind(void* ctx, zx_device_t* parent) {
  return usb_virtual_bus::UsbVirtualBus::Create(parent);
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = usb_virtual_bus_bind;
  return ops;
}();

}  // namespace usb_virtual_bus

ZIRCON_DRIVER(usb_virtual_bus, usb_virtual_bus::driver_ops, "zircon", "0.1");
