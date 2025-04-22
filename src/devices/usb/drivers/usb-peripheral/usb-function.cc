// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"

#include <fidl/fuchsia.hardware.usb.endpoint/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/usb/cpp/bind.h>
#include <fbl/auto_lock.h>

#include "src/devices/usb/drivers/usb-peripheral/usb-peripheral.h"

namespace usb_peripheral {

namespace fdescriptor = fuchsia_hardware_usb_descriptor;

zx_status_t UsbFunction::AddDevice(const std::string& name) {
  if (dev_added_) {
    return ZX_ERR_ALREADY_BOUND;
  }

  auto& desc = GetFunctionDescriptor();

  zx_device_str_prop_t props[] = {
      ddk::MakeStrProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_usb::BIND_PROTOCOL_FUNCTION),
      ddk::MakeStrProperty(bind_fuchsia::USB_CLASS, static_cast<uint32_t>(desc.interface_class)),
      ddk::MakeStrProperty(bind_fuchsia::USB_SUBCLASS,
                           static_cast<uint32_t>(desc.interface_subclass)),
      ddk::MakeStrProperty(bind_fuchsia::USB_PROTOCOL,
                           static_cast<uint32_t>(desc.interface_protocol)),
      ddk::MakeStrProperty(bind_fuchsia::USB_VID,
                           static_cast<uint32_t>(peripheral_->device_desc().id_vendor)),
      ddk::MakeStrProperty(bind_fuchsia::USB_PID,
                           static_cast<uint32_t>(peripheral_->device_desc().id_product)),
  };

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }
  zx_status_t status = AddService(std::move(endpoints->server));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not add service %s", zx_status_get_string(status));
    return status;
  }

  std::array offers = {
      fuchsia_hardware_usb_function::UsbFunctionService::Name,
      ddk::MetadataServer<fuchsia_boot_metadata::MacAddressMetadata>::kFidlServiceName,
  };
  status = DdkAdd(ddk::DeviceAddArgs(name.c_str())
                      .set_str_props(props)
                      // TODO(b/407987472): Don't forward DEVICE_METADATA_SERIAL_NUMBER once no
                      // longer retrieved.
                      .forward_metadata(peripheral_->parent(), DEVICE_METADATA_SERIAL_NUMBER)
                      .set_fidl_service_offers(offers)
                      .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb_dev_bind_functions add_device failed %s", zx_status_get_string(status));
    return status;
  }
  dev_added_ = true;
  // Hold a reference while devmgr has a pointer to the function.
  AddRef();
  return ZX_OK;
}

void UsbFunction::DdkRelease() {
  peripheral_->FunctionCleared();
  // Release the reference now that devmgr no longer has a pointer to the function.
  if (Release()) {
    delete this;
  }
}

// UsbFunctionProtocol implementation.
zx_status_t UsbFunction::UsbFunctionSetInterface(
    const usb_function_interface_protocol_t* function_intf) {
  auto func_intf = ddk::UsbFunctionInterfaceProtocolClient(function_intf);
  if (!func_intf.is_valid()) {
    bool was_valid = function_intf_.is_valid();
    function_intf_.clear();
    zxlogf(INFO, "Taking peripheral device offline until ready");
    return was_valid ? peripheral_->DeviceStateChanged() : ZX_OK;
  }
  if (function_intf_.is_valid()) {
    zxlogf(ERROR, "Function interface already bound");
    return ZX_ERR_ALREADY_BOUND;
  }

  function_intf_ = func_intf;

  size_t length = function_intf_.GetDescriptorsSize();
  fbl::AllocChecker ac;
  auto* descriptors = new (&ac) uint8_t[length];
  if (!ac.check()) {
    zxlogf(ERROR, "UsbFunctionSetInterface failed due to no memory.");
    return ZX_ERR_NO_MEMORY;
  }

  size_t actual;
  function_intf_.GetDescriptors(descriptors, length, &actual);
  if (actual != length) {
    zxlogf(ERROR, "UsbFunctionInterfaceClient::GetDescriptors() failed");
    delete[] descriptors;
    return ZX_ERR_INTERNAL;
  }
  num_interfaces_ = 0;

  auto status = peripheral_->ValidateFunction(fbl::RefPtr<UsbFunction>(this), descriptors, length,
                                              &num_interfaces_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "UsbFunctionInterfaceClient::ValidateFunction() failed - %s",
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
  return peripheral_->AllocInterface(fbl::RefPtr<UsbFunction>(this), out_intf_num);
}

zx_status_t UsbFunction::UsbFunctionAllocEp(uint8_t direction, uint8_t* out_address) {
  return peripheral_->AllocEndpoint(fbl::RefPtr<UsbFunction>(this), direction, out_address);
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
      zxlogf(DEBUG, "(framework) ConfigureEndpoint(): %s", result.status_string());
    } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
      zxlogf(DEBUG, "ConfigureEndpoint(): %s", result.status_string());
    } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
      return result->error_value();
    } else {
      return ZX_OK;
    }
  }

  zxlogf(DEBUG, "could not ConfigureEndpoint() over FIDL, falling back to banjo");
  return peripheral_->dci().ConfigEp(ep_desc, ss_comp_desc);
}

zx_status_t UsbFunction::UsbFunctionDisableEp(uint8_t address) {
  fidl::Arena arena;
  auto result = peripheral_->dci_new().buffer(arena)->DisableEndpoint(address);

  if (!result.ok()) {
    zxlogf(DEBUG, "(framework) DisableEndpoint(): %s", result.status_string());
  } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
    zxlogf(DEBUG, "DisableEndpoint(): %s", result.status_string());
  } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
    return result->error_value();
  } else {
    return ZX_OK;
  }

  zxlogf(DEBUG, "could not DisableEndpoint() over FIDL, falling back to banjo");
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
    zxlogf(DEBUG, "(framework) EndpointSetStall(): %s", result.status_string());
  } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
    zxlogf(DEBUG, "EndpointSetStall(): %s", result.status_string());
  } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
    return result->error_value();
  } else {
    return ZX_OK;
  }

  zxlogf(DEBUG, "could not EndointSetStall() over FIDL, falling back to banjo");
  return peripheral_->dci().EpSetStall(ep_address);
}

zx_status_t UsbFunction::UsbFunctionEpClearStall(uint8_t ep_address) {
  fidl::Arena arena;
  auto result = peripheral_->dci_new().buffer(arena)->EndpointClearStall(ep_address);

  if (!result.ok()) {
    zxlogf(DEBUG, "(framework) EndpointClearStall(): %s", result.status_string());
  } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
    zxlogf(DEBUG, "EndpointClearStall(): %s", result.status_string());
  } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
    return result->error_value();
  } else {
    return ZX_OK;
  }

  zxlogf(DEBUG, "could not EndointClearStall() over FIDL, falling back to banjo");
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
  } else {
    zxlogf(ERROR, "SetConfigured failed as the interface is invalid.");
    return ZX_ERR_BAD_STATE;
  }
}

zx_status_t UsbFunction::SetInterface(uint8_t interface, uint8_t alt_setting) {
  if (function_intf_.is_valid()) {
    return function_intf_.SetInterface(interface, alt_setting);
  } else {
    zxlogf(ERROR, "SetInterface failed as the interface is invalid.");
    return ZX_ERR_BAD_STATE;
  }
}

zx_status_t UsbFunction::Control(const usb_setup_t* setup, const void* write_buffer,
                                 size_t write_size, void* read_buffer, size_t read_size,
                                 size_t* out_read_actual) {
  if (function_intf_.is_valid()) {
    return function_intf_.Control(setup, reinterpret_cast<const uint8_t*>(write_buffer), write_size,
                                  reinterpret_cast<uint8_t*>(read_buffer), read_size,
                                  out_read_actual);
  } else {
    zxlogf(ERROR, "Control failed as the interface is invalid.");
    return ZX_ERR_BAD_STATE;
  }
}

zx::result<> UsbFunction::Init() {
  if (zx::result result = mac_address_metadata_server_.ForwardMetadataIfExists(parent_);
      result.is_error()) {
    zxlogf(ERROR, "Failed to forward mac address metadata: %s", result.status_string());
    return result.take_error();
  }
  if (zx_status_t status = mac_address_metadata_server_.Serve(
          outgoing_, fdf::Dispatcher::GetCurrent()->async_dispatcher());
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to serve mac address metadata: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  if (zx::result result = serial_number_metadata_server_.ForwardMetadataIfExists(parent_);
      result.is_error()) {
    zxlogf(ERROR, "Failed to forward serial number metadata: %s", result.status_string());
    return result.take_error();
  }
  if (zx_status_t status = serial_number_metadata_server_.Serve(
          outgoing_, fdf::Dispatcher::GetCurrent()->async_dispatcher());
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to serve serial number metadata: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

}  // namespace usb_peripheral
