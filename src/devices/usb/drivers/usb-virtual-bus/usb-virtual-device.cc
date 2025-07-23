// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-device.h"

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-bus.h"

namespace usb_virtual_bus {

void UsbVirtualDevice::UsbDciRequestQueue(usb_request_t* usb_request,
                                          const usb_request_complete_callback_t* complete_cb) {
  Request request(usb_request, *complete_cb, sizeof(usb_request_t));

  uint8_t index = EpAddressToIndex(request.request()->header.ep_address);
  if (index >= USB_MAX_EPS) {
    FDF_LOG(ERROR, "usb_virtual_bus_device_queue bad endpoint %u\n",
            request.request()->header.ep_address);
    request.Complete(ZX_ERR_INVALID_ARGS, 0);
    return;
  }

  bus_->ep(index).device_.QueueRequest(std::move(request));
}

zx_status_t UsbVirtualDevice::UsbDciSetInterface(const usb_dci_interface_protocol_t* interface) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbVirtualDevice::UsbDciConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                                             const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
  uint8_t index = EpAddressToIndex(usb_ep_num(ep_desc));
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }
  bus_->ep(index).max_packet_size_ = usb_ep_max_packet(ep_desc);
  return ZX_OK;
}

zx_status_t UsbVirtualDevice::UsbDciDisableEp(uint8_t ep_address) { return ZX_OK; }

zx_status_t UsbVirtualDevice::UsbDciEpSetStall(uint8_t ep_address) {
  uint8_t index = EpAddressToIndex(ep_address);
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }
  return bus_->ep(index).SetStall(true).status_value();
}

zx_status_t UsbVirtualDevice::UsbDciEpClearStall(uint8_t ep_address) {
  uint8_t index = EpAddressToIndex(ep_address);
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }
  return bus_->ep(index).SetStall(false).status_value();
}

zx_status_t UsbVirtualDevice::UsbDciCancelAll(uint8_t endpoint) {
  uint8_t index = EpAddressToIndex(endpoint);
  if (index >= USB_MAX_EPS) {
    return ZX_ERR_INVALID_ARGS;
  }

  bus_->ep(index).device_.CommonCancelAll();
  return ZX_OK;
}

size_t UsbVirtualDevice::UsbDciGetRequestSize() {
  return Request::RequestSize(Request::RequestSize(sizeof(usb_request_t)));
}

void UsbVirtualDevice::ConnectToEndpoint(ConnectToEndpointRequest& request,
                                         ConnectToEndpointCompleter::Sync& completer) {
  uint8_t index = EpAddressToIndex(request.ep_addr());
  if (index >= USB_MAX_EPS) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  bus_->ep(index).device_.Connect(std::move(request.ep()));
  completer.Reply(zx::ok());
}

void UsbVirtualDevice::SetInterface(SetInterfaceRequest& request,
                                    SetInterfaceCompleter::Sync& completer) {
  completer.Reply(bus_->SetDciInterface(std::move(request.interface())));
}

void UsbVirtualDevice::StartController(StartControllerCompleter::Sync& completer) {
  bus_->FinishConnect();
  completer.Reply(zx::ok());
}

void UsbVirtualDevice::StopController(StopControllerCompleter::Sync& completer) {
  bus_->SetConnected(false);
  completer.Reply(zx::ok());
}

void UsbVirtualDevice::ConfigureEndpoint(ConfigureEndpointRequest& request,
                                         ConfigureEndpointCompleter::Sync& completer) {
  uint8_t index = EpAddressToIndex(request.ep_descriptor().b_endpoint_address());
  if (index >= USB_MAX_EPS) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  bus_->ep(index).max_packet_size_ = usb_ep_max_packet2(request.ep_descriptor());
  completer.Reply(zx::ok());
}

void UsbVirtualDevice::DisableEndpoint(DisableEndpointRequest& request,
                                       DisableEndpointCompleter::Sync& completer) {
  zx_status_t status = UsbDciDisableEp(request.ep_address());
  if (status != ZX_OK) {
    completer.Reply(zx::error(status));
    return;
  }
  completer.Reply(zx::ok());
}

void UsbVirtualDevice::EndpointSetStall(EndpointSetStallRequest& request,
                                        EndpointSetStallCompleter::Sync& completer) {
  zx_status_t status = UsbDciEpSetStall(request.ep_address());
  if (status != ZX_OK) {
    completer.Reply(zx::error(status));
    return;
  }
  completer.Reply(zx::ok());
}

void UsbVirtualDevice::EndpointClearStall(EndpointClearStallRequest& request,
                                          EndpointClearStallCompleter::Sync& completer) {
  zx_status_t status = UsbDciEpClearStall(request.ep_address());
  if (status != ZX_OK) {
    completer.Reply(zx::error(status));
    return;
  }
  completer.Reply(zx::ok());
}

void UsbVirtualDevice::CancelAll(CancelAllRequest& request, CancelAllCompleter::Sync& completer) {
  zx_status_t status = UsbDciCancelAll(request.ep_address());
  if (status != ZX_OK) {
    completer.Reply(zx::error(status));
    return;
  }
  completer.Reply(zx::ok());
}

}  // namespace usb_virtual_bus
