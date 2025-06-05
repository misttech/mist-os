// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral/usb-peripheral.h"

#include <assert.h>
#include <fidl/fuchsia.hardware.usb.phy/cpp/fidl.h>
#include <fuchsia/hardware/usb/dci/c/banjo.h>
#include <fuchsia/hardware/usb/function/c/banjo.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/metadata/cpp/metadata.h>
#include <lib/fit/defer.h>
#include <lib/stdcompat/span.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <zircon/errors.h>
#include <zircon/listnode.h>
#include <zircon/types.h>

#include <algorithm>
#include <memory>
#include <string>
#include <vector>

#include <fbl/auto_lock.h>
#include <usb/cdc.h>
#include <usb/peripheral.h>
#include <usb/usb.h>

#include "src/devices/usb/drivers/usb-peripheral/config-parser.h"
#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"

namespace usb_peripheral {

zx_status_t UsbPeripheral::UsbDciCancelAll(uint8_t ep_address) {
  fidl::Arena arena;
  auto result = dci_new_.buffer(arena)->CancelAll(ep_address);

  if (!result.ok()) {
    fdf::debug("Failed to send CancelAll request: {}", result.status_string());
  } else if (result->is_error() && result->error_value() == ZX_ERR_NOT_SUPPORTED) {
    fdf::debug("Failed to cancel all: {}", zx_status_get_string(result->error_value()));
  } else if (result->is_error() && result->error_value() != ZX_ERR_NOT_SUPPORTED) {
    return result->error_value();
  } else {
    return ZX_OK;
  }

  fdf::debug("could not CancelAll() over FIDL, falling back to banjo");
  return dci_.CancelAll(ep_address);
}

void UsbPeripheral::RequestComplete(usb_request_t* req) {
  fbl::AutoLock l(&pending_requests_lock_);
  usb::BorrowedRequest<void> request(req, dci_.GetRequestSize());

  pending_requests_.erase(&request);
  l.release();
  request.Complete(request.request()->response.status, request.request()->response.actual);
  usb_monitor_.AddRecord(req);
}

void UsbPeripheral::UsbPeripheralRequestQueue(usb_request_t* usb_request,
                                              const usb_request_complete_callback_t* complete_cb) {
  if (shutting_down_) {
    usb_request_complete(usb_request, ZX_ERR_IO_NOT_PRESENT, 0, complete_cb);
    return;
  }
  fbl::AutoLock l(&pending_requests_lock_);
  usb::BorrowedRequest<void> request(usb_request, *complete_cb, dci_.GetRequestSize());
  [[maybe_unused]] usb_request_complete_callback_t completion;
  completion.ctx = this;
  completion.callback = [](void* ctx, usb_request_t* req) {
    reinterpret_cast<UsbPeripheral*>(ctx)->RequestComplete(req);
  };
  pending_requests_.push_back(&request);
  l.release();
  usb_monitor_.AddRecord(usb_request);
  dci_.RequestQueue(request.take(), &completion);
}

zx::result<> UsbPeripheral::Start() {
  zx::result client = incoming()->Connect<fuchsia_hardware_usb_dci::UsbDciService::Device>();
  if (client.is_error()) {
    fdf::error("Failed to connect dci fidl protocol: {}", client);
    return client.take_error();
  }
  dci_new_.Bind(std::move(*client));

  zx::result dci_banjo = compat::ConnectBanjo<ddk::UsbDciProtocolClient>(incoming());
  if (dci_banjo.is_error()) {
    fdf::info("Failed to connect to dci banjo protocol: {}", dci_banjo);
  } else {
    dci_ = dci_banjo.value();
  }

  if (!dci_.is_valid() && !dci_new_.is_valid()) {
    fdf::error("No banjo/FIDL UsbDci protocol served by parent");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  // Starting USB mode is determined from device metadata.
  // We read initial value and store it in dev->usb_mode, but do not actually
  // enable it until after all of our functions have bound.
  zx::result metadata =
      fdf_metadata::GetMetadataIfExists<fuchsia_hardware_usb_phy::Metadata>(*incoming());
  if (metadata.is_error()) {
    fdf::error("Failed to get metadata: {}", metadata);
    return metadata.take_error();
  }
  if (!metadata.value().has_value()) {
    fbl::AutoLock lock(&lock_);
    // Assume peripheral mode by default.
    parent_usb_mode_ = USB_MODE_PERIPHERAL;
  }

  if (dci_.is_valid()) {
    // This field is only applicable to the banjo protocol.
    parent_request_size_ = usb::BorrowedRequest<void>::RequestSize(dci_.GetRequestSize());
  }

  // Create child.
  zx::result<fidl::ClientEnd<fuchsia_device_fs::Connector>> bind_devfs_connector_result =
      devfs_connector_.Bind(dispatcher());
  if (bind_devfs_connector_result.is_error()) {
    fdf::error("Failed to bind devfs connector: {}", bind_devfs_connector_result);
    return bind_devfs_connector_result.take_error();
  }
  fuchsia_driver_framework::DevfsAddArgs devfs_args{{
      .connector = std::move(bind_devfs_connector_result).value(),
      .connector_supports = fuchsia_device_fs::ConnectionType::kDevice,
  }};
  zx::result child = AddOwnedChild(kChildNodeName, devfs_args);
  if (child.is_error()) {
    fdf::error("Failed to add child: {}", child);
    return child.take_error();
  }
  child_ = std::move(child.value());

  // Advertise a service:
  fuchsia_hardware_usb_peripheral::Service::InstanceHandler handler({
      .device = bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
  });
  zx::result add_result =
      outgoing()->AddService<fuchsia_hardware_usb_peripheral::Service>(std::move(handler));
  if (add_result.is_error()) {
    fdf::error("Failed to add service: {}", add_result);
    return add_result.take_error();
  }

  auto config = take_config<usb_peripheral_config::Config>();
  PeripheralConfigParser peripheral_config = {};
  zx_status_t status = peripheral_config.AddFunctions(config.functions());
  if (status != ZX_OK) {
    fdf::error("Failed to add usb functions from structured config: {}",
               zx_status_get_string(status));
    return zx::error(status);
  }

  device_desc_.id_vendor = peripheral_config.vid();
  device_desc_.id_product = peripheral_config.pid();

  status = AllocStringDesc(peripheral_config.manufacturer(), &device_desc_.i_manufacturer);
  if (status != ZX_OK) {
    fdf::error("Failed to allocate manufacturer string descriptor: {}",
               zx_status_get_string(status));
    return zx::error(status);
  }

  status = AllocStringDesc(peripheral_config.product(), &device_desc_.i_product);
  if (status != ZX_OK) {
    fdf::error("Failed to allocate product string descriptor: {}", zx_status_get_string(status));
    return zx::error(status);
  }

  zx::result serial = GetSerialNumber();
  if (serial.is_error()) {
    fdf::error("Failed to get serial number: {}", serial);
    return serial.take_error();
  }
  status = AllocStringDesc(std::move(serial.value()), &device_desc_.i_serial_number);
  if (status != ZX_OK) {
    fdf::error("Failed to add serial number descriptor: {}", zx_status_get_string(status));
    return zx::error(status);
  }

  status = SetDefaultConfig(peripheral_config.functions());
  if (status != ZX_OK) {
    fdf::error("Failed to set default config: {}", zx_status_get_string(status));
    return zx::error(status);
  }

  usb_monitor_.Start();

  // Try to set DciIntf over FIDL. We don't do this for Banjo because SetInterface is used to
  // indicate that all functions have been attached/un-attached. This is a separate method in
  // FIDL, so we set DciIntf here for FIDL.
  fidl::Arena arena;
  auto client_end = intf_srv_.AddBinding();
  auto result = dci_new_.buffer(arena)->SetInterface(std::move(client_end));
  // DCI FIDL is not available. Return OK because we could be using Banjo DCI instead.
  // In the future when we remove banjo. This should be an error.
  if (!result.ok()) {
    fdf::debug("Failed to send SetInterface request: {}", result.status_string());
    return zx::ok();
  }
  if (result->is_error()) {
    if (result->error_value() == ZX_ERR_NOT_SUPPORTED) {
      return zx::ok();
    }
    fdf::error("Failed to set interface: {}", zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }
  dci_new_valid_ = true;

  return zx::ok();
}

zx::result<std::string> UsbPeripheral::GetSerialNumber() {
  zx::result serial_number_result =
      fdf_metadata::GetMetadataIfExists<fuchsia_boot_metadata::SerialNumberMetadata>(*incoming());
  if (serial_number_result.is_error()) {
    fdf::error("Failed to get serial number metadata: {}", serial_number_result);
    return serial_number_result.take_error();
  }
  // Return serial number from metadata if present.
  if (serial_number_result.value().has_value()) {
    auto& metadata = serial_number_result.value().value();
    if (!metadata.serial_number().has_value()) {
      fdf::error("Serial number metadata missing serial_number field");
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok(std::move(metadata.serial_number().value()));
  }

  // Use MAC address as the next option.
  zx::result mac_address_result =
      fdf_metadata::GetMetadataIfExists<fuchsia_boot_metadata::MacAddressMetadata>(*incoming());
  if (mac_address_result.is_error()) {
    fdf::error("Failed to get MAC address metadata: {}", mac_address_result);
    return mac_address_result.take_error();
  }
  if (mac_address_result.value().has_value()) {
    const auto& metadata = mac_address_result.value().value();
    if (!metadata.mac_address().has_value()) {
      fdf::error("MAC address metadata missing mac_address field");
      return zx::error(ZX_ERR_INTERNAL);
    }
    const auto& octets = metadata.mac_address().value().octets();
    char buffer[13];
    snprintf(buffer, sizeof(buffer), "%02X%02X%02X%02X%02X%02X", octets[0], octets[1], octets[2],
             octets[3], octets[4], octets[5]);
    return zx::ok(std::string{buffer});
  }

  fdf::info("Serial number/MAC address not found. Using generic (non-unique) serial number.\n");

  return zx::ok(std::string{kDefaultSerialNumber});
}

zx_status_t UsbPeripheral::AllocStringDesc(std::string desc, uint8_t* out_index) {
  fbl::AutoLock lock(&lock_);

  if (strings_.size() >= MAX_STRINGS) {
    fdf::error("String descriptor limit reached");
    return ZX_ERR_NO_RESOURCES;
  }
  desc.resize(MAX_STRING_LENGTH);
  strings_.push_back(std::move(desc));

  // String indices are 1-based.
  *out_index = static_cast<uint8_t>(strings_.size());
  return ZX_OK;
}

zx_status_t UsbPeripheral::ValidateFunction(size_t function_index, void* descriptors, size_t length,
                                            uint8_t* out_num_interfaces) {
  auto* intf_desc = static_cast<usb_interface_descriptor_t*>(descriptors);
  if (intf_desc->b_descriptor_type == USB_DT_INTERFACE) {
    if (intf_desc->b_length != sizeof(usb_interface_descriptor_t)) {
      fdf::error("{}: interface descriptor is invalid", __func__);
      return ZX_ERR_INVALID_ARGS;
    }
  } else if (intf_desc->b_descriptor_type == USB_DT_INTERFACE_ASSOCIATION) {
    if (intf_desc->b_length != sizeof(usb_interface_assoc_descriptor_t)) {
      fdf::error("{}: interface association descriptor is invalid", __func__);
      return ZX_ERR_INVALID_ARGS;
    }
  } else {
    fdf::error("{}: first descriptor not an interface descriptor", __func__);
    return ZX_ERR_INVALID_ARGS;
  }

  auto* end =
      reinterpret_cast<const usb_descriptor_header_t*>(static_cast<uint8_t*>(descriptors) + length);
  auto* header = reinterpret_cast<const usb_descriptor_header_t*>(descriptors);

  while (header < end) {
    if (header->b_descriptor_type == USB_DT_INTERFACE) {
      auto* desc = reinterpret_cast<const usb_interface_descriptor_t*>(header);
      auto& function = GetFunction(function_index);
      ZX_ASSERT(function.configuration() < configurations_.size());
      const auto& configuration = configurations_[function.configuration()];
      const auto& interface_map = configuration.interface_map;
      if (desc->b_interface_number >= std::size(interface_map) ||
          interface_map[desc->b_interface_number] != function_index) {
        fdf::error("Function does not exist at interface number {}", desc->b_interface_number);
        return ZX_ERR_INVALID_ARGS;
      }
      if (desc->b_alternate_setting == 0) {
        if (*out_num_interfaces == UINT8_MAX) {
          return ZX_ERR_INVALID_ARGS;
        }
        (*out_num_interfaces)++;
      }
    } else if (header->b_descriptor_type == USB_DT_ENDPOINT) {
      auto* desc = reinterpret_cast<const usb_endpoint_descriptor_t*>(header);
      auto index = EpAddressToIndex(desc->b_endpoint_address);
      if (index == 0 || index >= std::size(endpoint_map_) ||
          endpoint_map_[index] != function_index) {
        fdf::error("Bad endpoint address {:#x}", desc->b_endpoint_address);
        return ZX_ERR_INVALID_ARGS;
      }
    }

    if (header->b_length == 0) {
      fdf::error("usb_func_set_interface: zero length descriptor");
      return ZX_ERR_INVALID_ARGS;
    }
    header = reinterpret_cast<const usb_descriptor_header_t*>(
        reinterpret_cast<const uint8_t*>(header) + header->b_length);
  }

  return ZX_OK;
}

bool UsbPeripheral::AllFunctionsRegistered() const {
  for (const auto& config : configurations_) {
    for (auto function_index : config.functions) {
      const auto& function = GetFunction(function_index);
      if (!function.registered()) {
        return false;
      }
    }
  }
  return true;
}

zx_status_t UsbPeripheral::FunctionRegistered() {
  fbl::AutoLock lock(&lock_);
  // Check to see if we have all our functions registered.
  // If so, we can build our configuration descriptor and tell the DCI driver we are ready.
  if (!AllFunctionsRegistered()) {
    // Need to wait for more functions to register.
    return ZX_OK;
  }
  size_t config_idx = 0;
  // build our configuration descriptor
  for (auto& config : configurations_) {
    std::vector<uint8_t> config_desc_bytes(sizeof(usb_configuration_descriptor_t));
    {
      auto* config_desc =
          reinterpret_cast<usb_configuration_descriptor_t*>(config_desc_bytes.data());

      config_desc->b_length = sizeof(*config_desc);
      config_desc->b_descriptor_type = USB_DT_CONFIG;
      config_desc->b_num_interfaces = 0;
      config_desc->b_configuration_value = static_cast<uint8_t>(1 + config_idx);
      config_desc->i_configuration = 0;
      // TODO(voydanoff) add a way to configure bm_attributes and bMaxPower
      config_desc->bm_attributes = USB_CONFIGURATION_SELF_POWERED | USB_CONFIGURATION_RESERVED_7;
      config_desc->b_max_power = 0;
    }

    for (auto function_index : config.functions) {
      auto& function = GetFunction(function_index);
      size_t descriptors_length;
      auto* descriptors = function.GetDescriptors(&descriptors_length);
      auto old_size = config_desc_bytes.size();
      config_desc_bytes.resize(old_size + descriptors_length);
      memcpy(config_desc_bytes.data() + old_size, descriptors, descriptors_length);
      reinterpret_cast<usb_configuration_descriptor_t*>(config_desc_bytes.data())
          ->b_num_interfaces += function.GetNumInterfaces();
    }
    reinterpret_cast<usb_configuration_descriptor_t*>(config_desc_bytes.data())->w_total_length =
        htole16(config_desc_bytes.size());
    config.config_desc = std::move(config_desc_bytes);
    config_idx++;
  }

  auto status = DeviceStateChangedLocked();
  if (status != ZX_OK) {
    fdf::error("DeviceStateChangedLocked failed: {}", status);
    return status;
  }
  lock.release();

  if (listener_.is_valid()) {
    if (fidl::Status status = fidl::WireCall(listener_)->FunctionRegistered(); !status.ok()) {
      // If you expected a call here, the listener_ might have been closed before it got called.
      // This shouldn't crash the driver though.
      fdf::debug("Failed to send FunctionRegistered request: {}", status);
    }
  }
  return ZX_OK;
}

zx_status_t UsbPeripheral::StartController() {
  if (dci_new_valid_) {
    fidl::Arena arena;
    auto result = dci_new_.buffer(arena)->StartController();
    if (!result.ok()) {
      fdf::debug("Failed to send StartController request: {}", result.status_string());
      return ZX_ERR_INTERNAL;
    }
    if (result->is_error()) {
      fdf::debug("Failed to start controller: {}", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
    return ZX_OK;
  }

  // Not all DCI drivers have the new FIDL protocols plumbed through. For drivers still in
  // migration, fall back to banjo if FIDL fails.
  fdf::warn("could not StartController over FIDL, falling back to banjo");
  // TODO(b/356940744): Move all DCI drivers over to the UsbDci FIDL protocol.
  zx_status_t status = dci_.SetInterface(this, &usb_dci_interface_protocol_ops_);
  if (status != ZX_OK) {
    fdf::error("SetInterface failed: {}", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t UsbPeripheral::StopController() {
  CommonSetConnected(false);

  if (dci_new_valid_) {
    fidl::Arena arena;
    auto result = dci_new_.buffer(arena)->StopController();
    if (!result.ok()) {
      fdf::debug("Failed to send StopController request: {}", result.status_string());
      return ZX_ERR_INTERNAL;
    }
    if (result->is_error()) {
      fdf::debug("Failed to stop controller: {}", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
    return ZX_OK;
  }

  // Not all DCI drivers have the new FIDL protocols plumbed through. For drivers still in
  // migration, fall back to banjo if FIDL fails.
  fdf::warn("could not StopController over FIDL, falling back to banjo");
  // TODO(b/356940744): Move all DCI drivers over to the UsbDci FIDL protocol.
  zx_status_t status = dci_.SetInterface(nullptr, nullptr);
  if (status != ZX_OK) {
    fdf::error("SetInterface failed: {}", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t UsbPeripheral::AllocInterface(size_t function_index, uint8_t* out_intf_num) {
  fbl::AutoLock lock(&lock_);
  auto& function = GetFunction(function_index);
  ZX_ASSERT(function.configuration() < configurations_.size());
  auto& configuration = configurations_[function.configuration()];
  auto& interface_map = configuration.interface_map;
  for (uint8_t i = 0; i < std::size(interface_map); i++) {
    if (!interface_map[i].has_value()) {
      interface_map[i] = function_index;
      *out_intf_num = i;
      return ZX_OK;
    }
  }

  return ZX_ERR_NO_RESOURCES;
}

zx_status_t UsbPeripheral::AllocEndpoint(size_t function_index, uint8_t direction,
                                         uint8_t* out_address) {
  uint8_t start, end;

  if (direction == USB_DIR_OUT) {
    start = OUT_EP_START;
    end = OUT_EP_END;
  } else if (direction == USB_DIR_IN) {
    start = IN_EP_START;
    end = IN_EP_END;
  } else {
    fdf::error("Invalid direction.");
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AutoLock lock(&lock_);
  for (uint8_t endpoint_index = start; endpoint_index <= end; endpoint_index++) {
    if (!endpoint_map_[endpoint_index].has_value()) {
      endpoint_map_[endpoint_index] = function_index;
      *out_address = EpIndexToAddress(endpoint_index);
      return ZX_OK;
    }
  }

  fdf::error("Exceeded maximum supported endpoints.");
  return ZX_ERR_NO_RESOURCES;
}

zx_status_t UsbPeripheral::GetDescriptor(uint8_t request_type, uint16_t value, uint16_t index,
                                         void* buffer, size_t length, size_t* out_actual) {
  uint8_t type = request_type & USB_TYPE_MASK;

  if (type != USB_TYPE_STANDARD) {
    fdf::debug("Unsupported request type: {}", request_type);
    return ZX_ERR_NOT_SUPPORTED;
  }

  fbl::AutoLock lock(&lock_);

  auto desc_type = static_cast<uint8_t>(value >> 8);
  if (desc_type == USB_DT_DEVICE && index == 0) {
    if (device_desc_.b_length == 0) {
      fdf::error("Device descriptor not set");
      return ZX_ERR_INTERNAL;
    }
    length = std::min(length, sizeof(device_desc_));
    memcpy(buffer, &device_desc_, length);
    *out_actual = length;
    return ZX_OK;
  } else if (desc_type == USB_DT_CONFIG && index == 0) {
    index = value & 0xff;
    if (index >= configurations_.size()) {
      fdf::error("Invalid configuration index: {}", index);
      return ZX_ERR_INVALID_ARGS;
    }
    auto& config_desc = configurations_[index].config_desc;
    if (config_desc.size() == 0) {
      fdf::error("Configuration descriptor not set");
      return ZX_ERR_INTERNAL;
    }
    auto desc_length = config_desc.size();
    length = std::min(length, desc_length);
    memcpy(buffer, config_desc.data(), length);
    *out_actual = length;
    return ZX_OK;
  } else if (desc_type == USB_DT_STRING) {
    uint8_t desc[255];
    auto* header = reinterpret_cast<usb_descriptor_header_t*>(desc);
    header->b_descriptor_type = USB_DT_STRING;

    auto string_index = static_cast<uint8_t>(value & 0xFF);
    if (string_index == 0) {
      // special case - return language list
      header->b_length = 4;
      desc[2] = 0x09;  // language ID
      desc[3] = 0x04;
    } else {
      // String indices are 1-based.
      string_index--;
      if (string_index >= strings_.size()) {
        fdf::error("Invalid string index: {}", string_index);
        return ZX_ERR_INVALID_ARGS;
      }
      const char* string = strings_[string_index].c_str();
      unsigned index = 2;

      // convert ASCII to UTF16
      if (string) {
        while (*string && index < sizeof(desc) - 2) {
          desc[index++] = *string++;
          desc[index++] = 0;
        }
      }
      header->b_length = static_cast<uint8_t>(index);
    }

    length = std::min<size_t>(header->b_length, length);
    memcpy(buffer, desc, length);
    *out_actual = length;
    return ZX_OK;
  } else if (desc_type == USB_DT_DEVICE_QUALIFIER) {
    if (device_desc_.b_length == 0) {
      fdf::error("Device descriptor not set");
      return ZX_ERR_INTERNAL;
    }
    length = std::min(length, sizeof(usb_device_qualifier_descriptor_t));
    memcpy(buffer, &device_desc_, length);
    auto* qualifier = static_cast<usb_device_qualifier_descriptor_t*>(buffer);
    qualifier->b_descriptor_type = USB_DT_DEVICE_QUALIFIER;
    qualifier->b_num_configurations = 0;
    qualifier->b_reserved = 0;
    *out_actual = length;
    return ZX_OK;
  } else if (desc_type == USB_DT_BOS) {
    usb_bos_descriptor_t bos{
      .b_length = sizeof(usb_bos_descriptor_t),
      .b_descriptor_type = USB_DT_BOS,
      .w_total_length = sizeof(usb_bos_descriptor_t),
      .b_num_device_caps = 0,  // No device capabilities.
    };
    length = std::min(length, sizeof(usb_bos_descriptor_t));
    memcpy(buffer, &bos, length);
    *out_actual = length;
    return ZX_OK;
  }

  fdf::error("Unsupported value: {:#x} index: {}", value, index);
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbPeripheral::SetConfiguration(uint8_t configuration) {
  bool configured = configuration > 0;
  // TODO(b/355271738): Logs added to debug b/355271738. Remove when fixed.
  fdf::info("Configuration {}", configuration);

  fbl::AutoLock lock(&lock_);
  for (const auto& config : configurations_) {
    for (auto function_index : config.functions) {
      auto& function = GetFunction(function_index);
      auto status = function.SetConfigured(function.configuration() == (configuration - 1), speed_);
      if (status != ZX_OK && configured) {
        return status;
      }
    }
  }

  configuration_ = configuration;

  return ZX_OK;
}

zx_status_t UsbPeripheral::SetInterface(uint8_t interface, uint8_t alt_setting) {
  const auto& configuration = configurations_[configuration_ - 1];
  if (interface >= std::size(configuration.interface_map)) {
    fdf::error("Invalid interface index: {}", interface);
    return ZX_ERR_OUT_OF_RANGE;
  }

  auto function_index = configuration.interface_map[interface];
  if (function_index.has_value()) {
    auto& function = GetFunction(function_index.value());
    return function.SetInterface(interface, alt_setting);
  }

  fdf::error("Function does not exist");
  return ZX_ERR_NOT_SUPPORTED;
}

zx::result<size_t> UsbPeripheral::AddFunction(UsbConfiguration& config, FunctionDescriptor desc) {
  fbl::AutoLock lock(&lock_);
  if (lock_functions_) {
    fdf::error("Functions are already bound");
    return zx::error(ZX_ERR_BAD_STATE);
  }

  auto function_index = functions_.size();
  auto function = std::make_unique<UsbFunction>(function_index, this, desc, config.index,
                                                fdf::Dispatcher::GetCurrent()->async_dispatcher());
  functions_.emplace_back(std::move(function));

  config.functions.push_back(function_index);
  return zx::ok(function_index);
}

void UsbPeripheral::ClearFunctions() {
  fdf::debug("{}", __func__);
  {
    fbl::AutoLock lock(&lock_);
    if (shutting_down_) {
      fdf::info("Already in process of clearing the functions");
      return;
    }
    shutting_down_ = true;
  }
  auto status = StopController();
  if (status != ZX_OK) {
    fdf::info("Failed to stop controller: {}", zx_status_get_string(status));
    return;
  }
  for (size_t i = 0; i < 256; i++) {
    UsbDciCancelAll(static_cast<uint8_t>(i));
  }
  {
    fbl::AutoLock lock(&lock_);
    shutting_down_ = false;
    configurations_.clear();
    lock_functions_ = false;
    for (size_t i = 0; i < std::size(endpoint_map_); i++) {
      endpoint_map_[i].reset();
    }
    strings_.clear();
    functions_.clear();

    DeviceStateChangedLocked();

    if (listener_.is_valid()) {
      if (fidl::Status status = fidl::WireCall(listener_)->FunctionsCleared(); !status.ok()) {
        fdf::error("Failed to send FunctionsCleared request: {}", status);
      }
    }
  }
}

zx_status_t UsbPeripheral::AddFunctionDevices() {
  fdf::debug("{}", __func__);
  int func_index = 0;
  for (const auto& configuration : configurations_) {
    for (auto function_index : configuration.functions) {
      char name[16];
      snprintf(name, sizeof(name), "function-%03u", func_index);

      auto& function = GetFunction(function_index);
      zx::result result = function.AddChild(child_.node_, name, incoming(), outgoing());
      if (result.is_error() && result.status_value() != ZX_ERR_ALREADY_BOUND) {
        fdf::error("Failed to add child {}: {}; Continuing on to next.", name, result);
      }

      func_index++;
    }
  }

  return ZX_OK;
}

zx_status_t UsbPeripheral::DeviceStateChanged() {
  fbl::AutoLock _(&lock_);
  return DeviceStateChangedLocked();
}

zx_status_t UsbPeripheral::DeviceStateChangedLocked() {
  fdf::info("cur_usb_mode={}, parent_usb_mode={}", cur_usb_mode_, parent_usb_mode_);

  std::optional<bool> stop = std::nullopt;
  usb_mode_t new_dci_usb_mode = cur_usb_mode_;
  if (parent_usb_mode_ == USB_MODE_PERIPHERAL) {
    if (lock_functions_) {
      // publish child devices
      auto status = AddFunctionDevices();
      if (status != ZX_OK) {
        return status;
      }
    }

    if (AllFunctionsRegistered()) {
      // switch DCI to device mode
      new_dci_usb_mode = USB_MODE_PERIPHERAL;
      stop = false;
    } else {
      new_dci_usb_mode = USB_MODE_NONE;
      stop = true;
    }
  } else {
    new_dci_usb_mode = parent_usb_mode_;
  }

  if (cur_usb_mode_ != new_dci_usb_mode) {
    fdf::info("Set DCI mode to {}", new_dci_usb_mode);
    cur_usb_mode_ = new_dci_usb_mode;

    if (stop.has_value()) {
      lock_.Release();
      auto status = *stop ? StopController() : StartController();
      lock_.Acquire();
      if (status != ZX_OK) {
        return status;
      }
    }
  }

  return ZX_OK;
}

zx_status_t UsbPeripheral::CommonControl(const usb_setup_t* setup, const uint8_t* write_buffer,
                                         size_t write_size, uint8_t* read_buffer, size_t read_size,
                                         size_t* out_read_actual) {
  uint8_t request_type = setup->bm_request_type;
  uint8_t direction = request_type & USB_DIR_MASK;
  uint8_t request = setup->b_request;
  uint16_t value = le16toh(setup->w_value);
  uint16_t index = le16toh(setup->w_index);
  uint16_t length = le16toh(setup->w_length);

  if (direction == USB_DIR_IN && length > read_size) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  } else if (direction == USB_DIR_OUT && length > write_size) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  if ((write_size > 0 && write_buffer == NULL) || (read_size > 0 && read_buffer == NULL)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fdf::debug("usb_dev_control type={:#02X}, req={}, value={}, index={}, length={}", request_type,
             request, value, index, length);

  switch (request_type & USB_RECIP_MASK) {
    case USB_RECIP_DEVICE:
      // handle standard device requests
      if ((request_type & (USB_DIR_MASK | USB_TYPE_MASK)) == (USB_DIR_IN | USB_TYPE_STANDARD) &&
          request == USB_REQ_GET_DESCRIPTOR) {
        return GetDescriptor(request_type, value, index, read_buffer, length, out_read_actual);
      } else if (request_type == (USB_DIR_OUT | USB_TYPE_STANDARD | USB_RECIP_DEVICE) &&
                 request == USB_REQ_SET_CONFIGURATION && length == 0) {
        return SetConfiguration(static_cast<uint8_t>(value));
      } else if (request_type == (USB_DIR_IN | USB_TYPE_STANDARD | USB_RECIP_DEVICE) &&
                 request == USB_REQ_GET_CONFIGURATION && length > 0) {
        *read_buffer = configuration_;
        *out_read_actual = sizeof(uint8_t);
        return ZX_OK;
      } else if (request_type == (USB_DIR_IN | USB_TYPE_STANDARD | USB_RECIP_DEVICE) &&
                 request == USB_REQ_GET_STATUS && length == 2) {
        read_buffer[1] = 1 << USB_DEVICE_SELF_POWERED;
        *out_read_actual = read_size;
        return ZX_OK;
      } else {
        // Delegate to one of the function drivers.
        // USB_RECIP_DEVICE should only be used when there is a single active interface.
        // But just to be conservative, try all the available interfaces.
        if (configuration_ == 0) {
          return ZX_ERR_BAD_STATE;
        }
        ZX_ASSERT(configuration_ <= configurations_.size());
        const auto& configuration = configurations_[configuration_ - 1];
        const auto& interface_map = configuration.interface_map;
        for (size_t i = 0; i < std::size(interface_map); i++) {
          auto function_index = interface_map[i];
          if (function_index.has_value()) {
            auto& function = GetFunction(function_index.value());
            auto status = function.Control(setup, write_buffer, write_size, read_buffer, read_size,
                                           out_read_actual);
            if (status == ZX_OK) {
              return ZX_OK;
            }
          }
        }
      }
      break;
    case USB_RECIP_INTERFACE: {
      if (request_type == (USB_DIR_OUT | USB_TYPE_STANDARD | USB_RECIP_INTERFACE) &&
          request == USB_REQ_SET_INTERFACE && length == 0) {
        return SetInterface(static_cast<uint8_t>(index), static_cast<uint8_t>(value));
      } else {
        const auto& configuration = configurations_[configuration_ - 1];
        const auto& interface_map = configuration.interface_map;
        if (index >= std::size(interface_map)) {
          return ZX_ERR_OUT_OF_RANGE;
        }
        // delegate to the function driver for the interface
        auto function_index = interface_map[index];
        if (function_index.has_value()) {
          auto& function = GetFunction(function_index.value());
          return function.Control(setup, write_buffer, write_size, read_buffer, read_size,
                                  out_read_actual);
        }
      }
      break;
    }
    case USB_RECIP_ENDPOINT: {
      // delegate to the function driver for the endpoint
      index = EpAddressToIndex(static_cast<uint8_t>(index));
      if (index == 0 || index >= USB_MAX_EPS) {
        return ZX_ERR_INVALID_ARGS;
      }
      if (index >= std::size(endpoint_map_)) {
        return ZX_ERR_OUT_OF_RANGE;
      }
      auto function_index = endpoint_map_[index];
      if (function_index.has_value()) {
        auto& function = GetFunction(function_index.value());
        return function.Control(setup, write_buffer, write_size, read_buffer, read_size,
                                out_read_actual);
      }
      break;
    }
    case USB_RECIP_OTHER:
      // TODO(voydanoff) - how to handle this?
    default:
      break;
  }

  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UsbPeripheral::UsbDciInterfaceControl(const usb_setup_t* setup,
                                                  const uint8_t* write_buffer, size_t write_size,
                                                  uint8_t* read_buffer, size_t read_size,
                                                  size_t* out_read_actual) {
  return CommonControl(setup, write_buffer, write_size, read_buffer, read_size, out_read_actual);
}

void UsbPeripheral::CommonSetConnected(bool connected) {
  bool was_connected = connected;
  {
    fbl::AutoLock lock(&lock_);
    std::swap(connected_, was_connected);
  }

  // TODO(b/355271738): Logs added to debug b/355271738. Remove when fixed.
  fdf::info("connected={}, was_connected={}", connected, was_connected);

  if (was_connected != connected) {
    if (!connected) {
      for (const auto& configuration : configurations_) {
        for (const auto function_index : configuration.functions) {
          auto& function = GetFunction(function_index);
          function.SetConfigured(false, USB_SPEED_UNDEFINED);
        }
      }
    }
  }
}

void UsbPeripheral::UsbDciInterfaceSetConnected(bool connected) { CommonSetConnected(connected); }

void UsbPeripheral::UsbDciInterfaceSetSpeed(usb_speed_t speed) { speed_ = speed; }

void UsbPeripheral::SetConfiguration(SetConfigurationRequestView request,
                                     SetConfigurationCompleter::Sync& completer) {
  {
    fbl::AutoLock _(&lock_);
    if (lock_functions_) {
      completer.ReplyError(ZX_ERR_ALREADY_BOUND);
      return;
    }
  }

  fdf::debug("{}", __func__);
  ZX_ASSERT(!request->config_descriptors.empty());
  uint8_t index = 0;
  for (auto& func_descs : request->config_descriptors) {
    auto& descriptor = configurations_.emplace_back(index);
    {
      fbl::AutoLock lock(&lock_);
      if (shutting_down_) {
        fdf::error("Cannot set configuration while clearing functions");
        completer.ReplyError(ZX_ERR_BAD_STATE);
        return;
      }
    }

    if (func_descs.count() == 0) {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }

    zx_status_t status = SetDeviceDescriptor(std::move(request->device_desc));
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    for (auto func_desc : func_descs) {
      auto result = AddFunction(descriptor, func_desc);
      if (result.is_error()) {
        fdf::error("Failed to add function: {}", result);
      }
    }
    index++;
  }
  {
    fbl::AutoLock _(&lock_);
    lock_functions_ = true;
    DeviceStateChangedLocked();
  }
  completer.ReplySuccess();
}

zx_status_t UsbPeripheral::SetDeviceDescriptor(DeviceDescriptor desc) {
  if (desc.b_num_configurations == 0) {
    fdf::error("bNumConfigurations must be non-zero");
    return ZX_ERR_INVALID_ARGS;
  } else {
    device_desc_.b_length = sizeof(usb_device_descriptor_t);
    device_desc_.b_descriptor_type = USB_DT_DEVICE;
    device_desc_.bcd_usb = desc.bcd_usb;
    device_desc_.b_device_class = desc.b_device_class;
    device_desc_.b_device_sub_class = desc.b_device_sub_class;
    device_desc_.b_device_protocol = desc.b_device_protocol;
    device_desc_.b_max_packet_size0 = desc.b_max_packet_size0;
    device_desc_.id_vendor = desc.id_vendor;
    device_desc_.id_product = desc.id_product;
    device_desc_.bcd_device = desc.bcd_device;
    zx_status_t status =
        AllocStringDesc(std::string(desc.manufacturer.data(), desc.manufacturer.size()),
                        &device_desc_.i_manufacturer);
    if (status != ZX_OK) {
      return status;
    }
    status = AllocStringDesc(std::string(desc.product.data(), desc.product.size()),
                             &device_desc_.i_product);
    if (status != ZX_OK) {
      return status;
    }
    status = AllocStringDesc(std::string(desc.serial.data(), desc.serial.size()),
                             &device_desc_.i_serial_number);
    if (status != ZX_OK) {
      return status;
    }
    device_desc_.b_num_configurations = desc.b_num_configurations;
    return ZX_OK;
  }
}

void UsbPeripheral::ClearFunctions(ClearFunctionsCompleter::Sync& completer) {
  fdf::debug("{}", __func__);
  ClearFunctions();
  completer.Reply();
}

int UsbPeripheral::ListenerCleanupThread() {
  zx_signals_t observed = 0;
  listener_.channel().wait_one(ZX_CHANNEL_PEER_CLOSED | __ZX_OBJECT_HANDLE_CLOSED,
                               zx::time::infinite(), &observed);
  fbl::AutoLock l(&lock_);
  listener_.reset();

  if (prepare_stop_completer_.has_value()) {
    prepare_stop_completer_.value()(zx::ok());
    prepare_stop_completer_.reset();
  }
  return 0;
}

void UsbPeripheral::SetStateChangeListener(SetStateChangeListenerRequestView request,
                                           SetStateChangeListenerCompleter::Sync& completer) {
  // This code is wrapped in a loop
  // to prevent a race condition in the event that multiple
  // clients try to set the handle at once.
  while (1) {
    fbl::AutoLock lock(&lock_);
    if (listener_.is_valid() && thread_) {
      thrd_t thread = thread_;
      thread_ = 0;
      lock.release();
      int output;
      thrd_join(thread, &output);
      continue;
    }
    if (listener_.is_valid()) {
      completer.Close(ZX_ERR_BAD_STATE);
      return;
    }
    if (thread_) {
      int output;
      thrd_t thread = thread_;
      thread_ = 0;
      lock.release();
      // We now own the thread, but not the listener.
      thrd_join(thread, &output);
      // Go back and try to re-set the listener_.
      // another caller may have tried to do this while we were blocked on thrd_join.
      continue;
    }
    listener_ = std::move(request->listener);
    if (thrd_create(
            &thread_,
            [](void* arg) -> int {
              return reinterpret_cast<UsbPeripheral*>(arg)->ListenerCleanupThread();
            },
            reinterpret_cast<void*>(this)) != thrd_success) {
      listener_.reset();
      completer.Close(ZX_ERR_INTERNAL);
      return;
    }
    return;
  }
}
void UsbPeripheral::PrepareStop(fdf::PrepareStopCompleter completer) {
  if (thread_) {
    prepare_stop_completer_.emplace(std::move(completer));
  } else {
    completer(zx::ok());
  }

  {
    fbl::AutoLock l(&lock_);
    if (listener_) {
      listener_.reset();
    }
  }

  ClearFunctions();
  usb_monitor_.Stop();
}

void UsbPeripheral::Stop() {
  if (thread_) {
    int output;
    thrd_join(thread_, &output);
    thread_ = 0;
  }
}

zx_status_t UsbPeripheral::SetDefaultConfig(std::vector<FunctionDescriptor>& functions) {
  {
    fbl::AutoLock _(&lock_);
    if (lock_functions_) {
      return ZX_ERR_ALREADY_BOUND;
    }
  }

  auto& descriptor = configurations_.emplace_back(static_cast<uint8_t>(0));
  device_desc_.b_length = sizeof(usb_device_descriptor_t),
  device_desc_.b_descriptor_type = USB_DT_DEVICE;
  device_desc_.bcd_usb = htole16(0x0200);
  device_desc_.b_device_class = 0;
  device_desc_.b_device_sub_class = 0;
  device_desc_.b_device_protocol = 0;
  device_desc_.b_max_packet_size0 = 64;
  device_desc_.bcd_device = htole16(0x0100);
  device_desc_.b_num_configurations = 1;

  for (auto function : functions) {
    auto result = AddFunction(descriptor, function);
    if (result.is_error()) {
      fdf::error("Failed to add function: ({}:{}:{}) status: {}", function.interface_class,
                 function.interface_subclass, function.interface_protocol, result);
      return result.status_value();
    }
  }

  {
    fbl::AutoLock _(&lock_);
    lock_functions_ = true;
    DeviceStateChangedLocked();
  }
  return ZX_OK;
}

UsbFunction& UsbPeripheral::GetFunction(size_t index) {
  ZX_ASSERT_MSG(index < functions_.size(), "Function %lu does not exist", index);
  auto& function = functions_[index];
  ZX_ASSERT(function != nullptr);
  return *function;
}

const UsbFunction& UsbPeripheral::GetFunction(size_t index) const {
  ZX_ASSERT_MSG(index < functions_.size(), "Function %lu does not exist", index);
  const auto& function = functions_[index];
  ZX_ASSERT(function != nullptr);
  return *function;
}

}  // namespace usb_peripheral
FUCHSIA_DRIVER_EXPORT(usb_peripheral::UsbPeripheral);
