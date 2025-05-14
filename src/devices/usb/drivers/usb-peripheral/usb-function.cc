// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"

#include <fidl/fuchsia.hardware.usb.endpoint/cpp/wire.h>
#include <lib/ddk/metadata.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/usb/cpp/bind.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/devices/usb/drivers/usb-peripheral/usb-peripheral.h"

namespace usb_peripheral {

namespace fdescriptor = fuchsia_hardware_usb_descriptor;

zx::result<> UsbFunction::AddChild(fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent,
                                   const std::string& child_node_name,
                                   const std::shared_ptr<fdf::Namespace>& incoming,
                                   const std::shared_ptr<fdf::OutgoingDirectory>& outgoing) {
  if (child_.is_valid()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }

  {
    compat::DeviceServer::BanjoConfig banjo_config;
    banjo_config.callbacks[ZX_PROTOCOL_USB_FUNCTION] = banjo_server_.callback();
    zx::result result = compat_server_.Initialize(
        incoming, outgoing, std::string{UsbPeripheral::kChildNodeName}, child_node_name,
        compat::ForwardMetadata::None(), std::move(banjo_config));
    if (result.is_error()) {
      fdf::error("Failed to initialize compat server: {}", result);
      return result.take_error();
    }
  }

  auto& mac_address_metadata_server = mac_address_metadata_server_.emplace(child_node_name);
  if (zx::result result = mac_address_metadata_server.ForwardMetadataIfExists(incoming);
      result.is_error()) {
    fdf::error("Failed to forward mac address metadata: {}", result.status_string());
    return result.take_error();
  }
  if (zx::result result = mac_address_metadata_server.Serve(*outgoing, dispatcher_);
      result.is_error()) {
    fdf::error("Failed to serve mac address metadata: {}", result);
    return result.take_error();
  }

  auto& serial_number_metadata_server = serial_number_metadata_server_.emplace(child_node_name);
  if (zx::result result = serial_number_metadata_server.ForwardMetadataIfExists(incoming);
      result.is_error()) {
    fdf::error("Failed to forward serial number metadata: {}", result.status_string());
    return result.take_error();
  }
  if (zx::result result = serial_number_metadata_server.Serve(*outgoing, dispatcher_);
      result.is_error()) {
    fdf::error("Failed to serve serial number metadata: {}", result);
    return result.take_error();
  }

  zx::result result = outgoing->AddService<fuchsia_hardware_usb_function::UsbFunctionService>(
      fuchsia_hardware_usb_function::UsbFunctionService::InstanceHandler({
          .device = bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure),
      }),
      child_node_name);
  if (result.is_error()) {
    fdf::error("Failed to add usb-function service: {}", result);
    return result.take_error();
  }

  auto& desc = GetFunctionDescriptor();

  std::vector props = {
      fdf::MakeProperty2(bind_fuchsia::PROTOCOL, bind_fuchsia_usb::BIND_PROTOCOL_FUNCTION),
      fdf::MakeProperty2(bind_fuchsia::USB_CLASS, static_cast<uint32_t>(desc.interface_class)),
      fdf::MakeProperty2(bind_fuchsia::USB_SUBCLASS,
                         static_cast<uint32_t>(desc.interface_subclass)),
      fdf::MakeProperty2(bind_fuchsia::USB_PROTOCOL,
                         static_cast<uint32_t>(desc.interface_protocol)),
      fdf::MakeProperty2(bind_fuchsia::USB_VID,
                         static_cast<uint32_t>(peripheral_->device_desc().id_vendor)),
      fdf::MakeProperty2(bind_fuchsia::USB_PID,
                         static_cast<uint32_t>(peripheral_->device_desc().id_product)),
  };

  std::vector offers = compat_server_.CreateOffers2();
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_usb_function::UsbFunctionService>(child_node_name));
  offers.push_back(mac_address_metadata_server.MakeOffer());
  offers.push_back(serial_number_metadata_server.MakeOffer());

  zx::result child =
      fdf::AddChild(parent, *fdf::Logger::GlobalInstance(), child_node_name, props, offers);
  if (child.is_error()) {
    fdf::error("Failed to add child: {}", child);
    return child.take_error();
  }
  child_ = std::move(child.value());
  return zx::ok();
}

// UsbFunctionProtocol implementation.
zx_status_t UsbFunction::UsbFunctionSetInterface(
    const usb_function_interface_protocol_t* function_intf) {
  auto func_intf = ddk::UsbFunctionInterfaceProtocolClient(function_intf);
  if (!func_intf.is_valid()) {
    bool was_valid = function_intf_.is_valid();
    function_intf_.clear();
    fdf::info("Taking peripheral device offline until ready");
    return was_valid ? peripheral_->DeviceStateChanged() : ZX_OK;
  }
  if (function_intf_.is_valid()) {
    fdf::error("Function interface already bound");
    return ZX_ERR_ALREADY_BOUND;
  }

  function_intf_ = func_intf;

  size_t length = function_intf_.GetDescriptorsSize();
  fbl::AllocChecker ac;
  auto* descriptors = new (&ac) uint8_t[length];
  if (!ac.check()) {
    fdf::error("UsbFunctionSetInterface failed due to no memory.");
    return ZX_ERR_NO_MEMORY;
  }

  size_t actual;
  function_intf_.GetDescriptors(descriptors, length, &actual);
  if (actual != length) {
    fdf::error("UsbFunctionInterfaceClient::GetDescriptors() failed");
    delete[] descriptors;
    return ZX_ERR_INTERNAL;
  }
  num_interfaces_ = 0;

  auto status = peripheral_->ValidateFunction(index_, descriptors, length, &num_interfaces_);
  if (status != ZX_OK) {
    fdf::error("UsbFunctionInterfaceClient::ValidateFunction() failed: {}",
               zx_status_get_string(status));
    delete[] descriptors;
    return status;
  }

  descriptors_.reset(descriptors, length);
  return peripheral_->FunctionRegistered();
}

zx_status_t UsbFunction::UsbFunctionCancelAll(uint8_t ep_address) {
  return peripheral_->UsbDciCancelAll(ep_address);
}

zx_status_t UsbFunction::UsbFunctionAllocInterface(uint8_t* out_intf_num) {
  return peripheral_->AllocInterface(index_, out_intf_num);
}

zx_status_t UsbFunction::UsbFunctionAllocEp(uint8_t direction, uint8_t* out_address) {
  return peripheral_->AllocEndpoint(index_, direction, out_address);
}

zx_status_t UsbFunction::UsbFunctionConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                                             const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
  fidl::Arena arena;

  fdescriptor::wire::UsbEndpointDescriptor fep_desc;
  fep_desc.b_length = ep_desc->b_length;
  fep_desc.b_descriptor_type = ep_desc->b_descriptor_type;
  fep_desc.b_endpoint_address = ep_desc->b_endpoint_address;
  fep_desc.bm_attributes = ep_desc->bm_attributes;
  fep_desc.w_max_packet_size = ep_desc->w_max_packet_size;
  fep_desc.b_interval = ep_desc->b_interval;

  fdescriptor::wire::UsbSsEpCompDescriptor fss_comp_desc;
  if (ss_comp_desc != nullptr) {  // Only applies to 3.x devices.
    fss_comp_desc.b_length = ss_comp_desc->b_length;
    fss_comp_desc.b_descriptor_type = ss_comp_desc->b_descriptor_type;
    fss_comp_desc.b_max_burst = ss_comp_desc->b_max_burst;
    fss_comp_desc.bm_attributes = ss_comp_desc->bm_attributes;
    fss_comp_desc.w_bytes_per_interval = ss_comp_desc->w_bytes_per_interval;
  }

  if (peripheral_->dci_new_valid()) {
    auto result = peripheral_->dci_new().buffer(arena)->ConfigureEndpoint(fep_desc, fss_comp_desc);

    if (!result.ok()) {
      fdf::debug("Failed to send ConfigureEndpoint request: {}", result.status_string());
    } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
      fdf::debug("Failed to configure endpoint: {}", result.status_string());
    } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
      return result->error_value();
    } else {
      return ZX_OK;
    }
  }

  fdf::debug("could not ConfigureEndpoint() over FIDL, falling back to banjo");
  return peripheral_->dci().ConfigEp(ep_desc, ss_comp_desc);
}

zx_status_t UsbFunction::UsbFunctionDisableEp(uint8_t address) {
  fidl::Arena arena;
  auto result = peripheral_->dci_new().buffer(arena)->DisableEndpoint(address);

  if (!result.ok()) {
    fdf::debug("Failed to send DisableEndpoint request: {}", result.status_string());
  } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
    fdf::debug("Failed to disable endpoint: {}", result.status_string());
  } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
    return result->error_value();
  } else {
    return ZX_OK;
  }

  fdf::debug("could not DisableEndpoint() over FIDL, falling back to banjo");
  return peripheral_->dci().DisableEp(address);
}

zx_status_t UsbFunction::UsbFunctionAllocStringDesc(const char* str, uint8_t* out_index) {
  return peripheral_->AllocStringDesc(str, out_index);
}

void UsbFunction::UsbFunctionRequestQueue(usb_request_t* usb_request,
                                          const usb_request_complete_callback_t* complete_cb) {
  peripheral_->UsbPeripheralRequestQueue(usb_request, complete_cb);
}

zx_status_t UsbFunction::UsbFunctionEpSetStall(uint8_t ep_address) {
  fidl::Arena arena;
  auto result = peripheral_->dci_new().buffer(arena)->EndpointSetStall(ep_address);

  if (!result.ok()) {
    fdf::debug("Failed to send EndpointSetStall request: {}", result.status_string());
  } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
    fdf::debug("Failed to set stall: {}", result.status_string());
  } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
    return result->error_value();
  } else {
    return ZX_OK;
  }

  fdf::debug("could not EndointSetStall() over FIDL, falling back to banjo");
  return peripheral_->dci().EpSetStall(ep_address);
}

zx_status_t UsbFunction::UsbFunctionEpClearStall(uint8_t ep_address) {
  fidl::Arena arena;
  auto result = peripheral_->dci_new().buffer(arena)->EndpointClearStall(ep_address);

  if (!result.ok()) {
    fdf::debug("Failed to send EndpointClearStall request: {}", result.status_string());
  } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
    fdf::debug("Failed to clear stall): {}", result.status_string());
  } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
    return result->error_value();
  } else {
    return ZX_OK;
  }

  fdf::debug("could not EndointClearStall() over FIDL, falling back to banjo");
  return peripheral_->dci().EpClearStall(ep_address);
}

size_t UsbFunction::UsbFunctionGetRequestSize() { return peripheral_->ParentRequestSize(); }

void UsbFunction::ConnectToEndpoint(ConnectToEndpointRequest& request,
                                    ConnectToEndpointCompleter::Sync& completer) {
  auto status = peripheral_->ConnectToEndpoint(request.ep_addr(), std::move(request.ep()));
  if (status != ZX_OK) {
    completer.Reply(fit::as_error(status));
    return;
  }
  completer.Reply(fit::ok());
}

zx_status_t UsbFunction::SetConfigured(bool configured, usb_speed_t speed) {
  if (function_intf_.is_valid()) {
    return function_intf_.SetConfigured(configured, speed);
  }
  fdf::error("SetConfigured failed as the interface is invalid.");
  return ZX_ERR_BAD_STATE;
}

zx_status_t UsbFunction::SetInterface(uint8_t interface, uint8_t alt_setting) {
  if (function_intf_.is_valid()) {
    return function_intf_.SetInterface(interface, alt_setting);
  }
  fdf::error("SetInterface failed as the interface is invalid.");
  return ZX_ERR_BAD_STATE;
}

zx_status_t UsbFunction::Control(const usb_setup_t* setup, const void* write_buffer,
                                 size_t write_size, void* read_buffer, size_t read_size,
                                 size_t* out_read_actual) {
  if (function_intf_.is_valid()) {
    return function_intf_.Control(setup, reinterpret_cast<const uint8_t*>(write_buffer), write_size,
                                  reinterpret_cast<uint8_t*>(read_buffer), read_size,
                                  out_read_actual);
  }
  fdf::error("Control failed as the interface is invalid.");
  return ZX_ERR_BAD_STATE;
}

}  // namespace usb_peripheral
