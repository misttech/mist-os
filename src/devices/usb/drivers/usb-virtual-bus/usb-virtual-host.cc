// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-host.h"

#include <assert.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <fbl/auto_lock.h>
#include <usb/request-fidl.h>
#include <usb/usb-request.h>

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-bus.h"

namespace usb_virtual_bus {

void UsbVirtualHost::DdkRelease() { delete this; }

void UsbVirtualHost::UsbHciRequestQueue(usb_request_t* req,
                                        const usb_request_complete_callback_t* complete_cb) {
  return bus_->UsbHciRequestQueue(req, complete_cb);
}

void UsbVirtualHost::UsbHciSetBusInterface(const usb_bus_interface_protocol_t* bus_intf) {
  return bus_->UsbHciSetBusInterface(bus_intf);
}

size_t UsbVirtualHost::UsbHciGetMaxDeviceCount() { return bus_->UsbHciGetMaxDeviceCount(); }

zx_status_t UsbVirtualHost::UsbHciEnableEndpoint(uint32_t device_id,
                                                 const usb_endpoint_descriptor_t* ep_desc,
                                                 const usb_ss_ep_comp_descriptor_t* ss_com_desc,
                                                 bool enable) {
  return bus_->UsbHciEnableEndpoint(device_id, ep_desc, ss_com_desc, enable);
}

uint64_t UsbVirtualHost::UsbHciGetCurrentFrame() { return bus_->UsbHciGetCurrentFrame(); }

zx_status_t UsbVirtualHost::UsbHciConfigureHub(uint32_t device_id, usb_speed_t speed,
                                               const usb_hub_descriptor_t* desc, bool multi_tt) {
  return bus_->UsbHciConfigureHub(device_id, speed, desc, multi_tt);
}

zx_status_t UsbVirtualHost::UsbHciHubDeviceAdded(uint32_t device_id, uint32_t port,
                                                 usb_speed_t speed) {
  return bus_->UsbHciHubDeviceAdded(device_id, port, speed);
}

zx_status_t UsbVirtualHost::UsbHciHubDeviceRemoved(uint32_t device_id, uint32_t port) {
  return bus_->UsbHciHubDeviceRemoved(device_id, port);
}

zx_status_t UsbVirtualHost::UsbHciHubDeviceReset(uint32_t device_id, uint32_t port) {
  return bus_->UsbHciHubDeviceReset(device_id, port);
}

zx_status_t UsbVirtualHost::UsbHciResetEndpoint(uint32_t device_id, uint8_t ep_address) {
  return bus_->UsbHciResetEndpoint(device_id, ep_address);
}

zx_status_t UsbVirtualHost::UsbHciResetDevice(uint32_t hub_address, uint32_t device_id) {
  return bus_->UsbHciResetDevice(hub_address, device_id);
}

size_t UsbVirtualHost::UsbHciGetMaxTransferSize(uint32_t device_id, uint8_t ep_address) {
  return bus_->UsbHciGetMaxTransferSize(device_id, ep_address);
}

zx_status_t UsbVirtualHost::UsbHciCancelAll(uint32_t device_id, uint8_t ep_address) {
  return bus_->UsbHciCancelAll(device_id, ep_address);
}

size_t UsbVirtualHost::UsbHciGetRequestSize() { return bus_->UsbHciGetRequestSize(); }

void UsbVirtualHost::ConnectToEndpoint(ConnectToEndpointRequest& request,
                                       ConnectToEndpointCompleter::Sync& completer) {
  uint8_t index = EpAddressToIndex(request.ep_addr());
  if (index >= USB_MAX_EPS) {
    printf("usb_virtual_bus_host_queue bad endpoint %u\n", request.ep_addr());
    return completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
  }

  if (eps_[index].has_value()) {
    return completer.Reply(zx::error(ZX_ERR_ALREADY_BOUND));
  }

  async::PostTask(bus_->device_dispatcher(), [this, index, req = std::move(request),
                                              cmp = completer.ToAsync()]() mutable {
    eps_[index].emplace(bus_->ep(index), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                        std::move(req.ep()));
    cmp.Reply(zx::ok());
  });
}

UsbVirtualHost::~UsbVirtualHost() {
  libsync::Completion wait;
  async::PostTask(bus_->device_dispatcher(), [this, &wait]() mutable {
    for (auto& ep : eps_) {
      ep.reset();
    }
    wait.Signal();
  });
  wait.Wait();
}

void UsbVirtualHost::UsbEpServer::OnFidlClosed(fidl::UnbindInfo) {
  for (const auto& [_, registered_vmo] : registered_vmos_) {
    auto status = zx::vmar::root_self()->unmap(registered_vmo.addr, registered_vmo.size);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to unmap VMO %d", status);
      continue;
    }
  }
}

void UsbVirtualHost::UsbEpServer::RegisterVmos(RegisterVmosRequest& request,
                                               RegisterVmosCompleter::Sync& completer) {
  std::vector<fuchsia_hardware_usb_endpoint::VmoHandle> vmos;
  for (const auto& info : request.vmo_ids()) {
    ZX_ASSERT(info.id());
    ZX_ASSERT(info.size());
    auto id = *info.id();
    auto size = *info.size();

    if (registered_vmos_.find(id) != registered_vmos_.end()) {
      zxlogf(ERROR, "VMO ID %lu already registered", id);
      continue;
    }

    zx::vmo vmo;
    auto status = zx::vmo::create(size, 0, &vmo);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to pin registered VMO %d", status);
      continue;
    }

    // Map VMO.
    zx_vaddr_t mapped_addr;
    status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, size,
                                        &mapped_addr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to map the vmo: %d", status);
      // Try for the next one.
      continue;
    }

    // Save
    vmos.emplace_back(
        std::move(fuchsia_hardware_usb_endpoint::VmoHandle().id(id).vmo(std::move(vmo))));
    registered_vmos_.emplace(id, usb::MappedVmo{mapped_addr, size});
  }

  completer.Reply({std::move(vmos)});
}

void UsbVirtualHost::UsbEpServer::UnregisterVmos(UnregisterVmosRequest& request,
                                                 UnregisterVmosCompleter::Sync& completer) {
  std::vector<zx_status_t> errors;
  std::vector<uint64_t> failed_vmo_ids;
  for (const auto& id : request.vmo_ids()) {
    auto registered_vmo = registered_vmos_.extract(id);
    if (registered_vmo.empty()) {
      failed_vmo_ids.emplace_back(id);
      errors.emplace_back(ZX_ERR_NOT_FOUND);
      continue;
    }

    auto status =
        zx::vmar::root_self()->unmap(registered_vmo.mapped().addr, registered_vmo.mapped().size);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to unmap VMO %d", status);
      failed_vmo_ids.emplace_back(id);
      errors.emplace_back(status);
      continue;
    }
  }
  completer.Reply({std::move(failed_vmo_ids), std::move(errors)});
}

void UsbVirtualHost::UsbEpServer::QueueRequests(QueueRequestsRequest& request,
                                                QueueRequestsCompleter::Sync& completer) {
  for (auto& req : request.req()) {
    ep_->QueueRequest(usb::FidlRequest{std::move(req)});
  }
}

void UsbVirtualHost::UsbEpServer::CancelAll(CancelAllCompleter::Sync& completer) {
  completer.Reply(ep_->CancelAll());
}

void UsbVirtualHost::UsbEpServer::RequestComplete(zx_status_t status, size_t actual,
                                                  usb::FidlRequest request) {
  auto defer_completion = *request->defer_completion();
  completions_.emplace_back(std::move(fuchsia_hardware_usb_endpoint::Completion()
                                          .request(request.take_request())
                                          .status(status)
                                          .transfer_size(actual)));
  if (defer_completion && status == ZX_OK) {
    return;
  }
  std::vector<fuchsia_hardware_usb_endpoint::Completion> completions;
  completions.swap(completions_);

  auto result = fidl::SendEvent(binding_)->OnCompletion(std::move(completions));
  if (result.is_error()) {
    zxlogf(ERROR, "Error sending event: %s", result.error_value().status_string());
  }
}

}  // namespace usb_virtual_bus
