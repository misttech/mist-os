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
  bus_->UsbHciSetBusInterface(bus_intf);
}

size_t UsbVirtualHost::UsbHciGetMaxDeviceCount() { return 1; }

zx_status_t UsbVirtualHost::UsbHciEnableEndpoint(uint32_t device_id,
                                                 const usb_endpoint_descriptor_t* ep_desc,
                                                 const usb_ss_ep_comp_descriptor_t* ss_com_desc,
                                                 bool enable) {
  return ZX_OK;
}

uint64_t UsbVirtualHost::UsbHciGetCurrentFrame() { return 0; }

zx_status_t UsbVirtualHost::UsbHciConfigureHub(uint32_t device_id, usb_speed_t speed,
                                               const usb_hub_descriptor_t* desc, bool multi_tt) {
  return ZX_OK;
}

zx_status_t UsbVirtualHost::UsbHciHubDeviceAdded(uint32_t device_id, uint32_t port,
                                                 usb_speed_t speed) {
  return ZX_OK;
}

zx_status_t UsbVirtualHost::UsbHciHubDeviceRemoved(uint32_t device_id, uint32_t port) {
  return ZX_OK;
}

zx_status_t UsbVirtualHost::UsbHciHubDeviceReset(uint32_t device_id, uint32_t port) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbVirtualHost::UsbHciResetEndpoint(uint32_t device_id, uint8_t ep_address) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbVirtualHost::UsbHciResetDevice(uint32_t hub_address, uint32_t device_id) {
  return ZX_ERR_NOT_SUPPORTED;
}

size_t UsbVirtualHost::UsbHciGetMaxTransferSize(uint32_t device_id, uint8_t ep_address) {
  return 65536;
}

zx_status_t UsbVirtualHost::UsbHciCancelAll(uint32_t device_id, uint8_t ep_address) {
  return bus_->UsbHciCancelAll(device_id, ep_address);
}

size_t UsbVirtualHost::UsbHciGetRequestSize() {
  return Request::RequestSize(sizeof(usb_request_t));
}

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

}  // namespace usb_virtual_bus
