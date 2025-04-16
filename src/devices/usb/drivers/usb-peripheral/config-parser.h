// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_CONFIG_PARSER_H_
#define SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_CONFIG_PARSER_H_

#include <fidl/fuchsia.hardware.usb.peripheral/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.peripheral/cpp/wire_types.h>
#include <lib/ddk/device.h>
#include <stdint.h>

#include <cstdint>
#include <string_view>
#include <vector>

#include <usb/cdc.h>
#include <usb/peripheral.h>
#include <usb/usb.h>

namespace usb_peripheral {

namespace peripheral = fuchsia_hardware_usb_peripheral;

constexpr std::string_view kDefaultSerialNumber = "0123456789ABCDEF";
constexpr std::string_view kManufacturer = "Zircon";
constexpr std::string_view kCompositeDeviceConnector = " & ";
constexpr std::string_view kCDCProductDescription = "CDC Ethernet";
constexpr std::string_view kUMSProductDescription = "USB Mass Storage";
constexpr std::string_view kRNDISProductDescription = "RNDIS Ethernet";
constexpr std::string_view kTestProductDescription = "USB Function Test";
constexpr std::string_view kADBProductDescription = "ADB";
constexpr std::string_view kVsockBridgeProductDescription = "VSOCK Bridge";
constexpr std::string_view kFastbootProductDescription = "Fastboot";

constexpr peripheral::wire::FunctionDescriptor kCDCFunctionDescriptor = {
    .interface_class = USB_CLASS_COMM,
    .interface_subclass = USB_CDC_SUBCLASS_ETHERNET,
    .interface_protocol = 0,
};

constexpr peripheral::wire::FunctionDescriptor kUMSFunctionDescriptor = {
    .interface_class = USB_CLASS_MSC,
    .interface_subclass = USB_SUBCLASS_MSC_SCSI,
    .interface_protocol = USB_PROTOCOL_MSC_BULK_ONLY,
};

constexpr peripheral::wire::FunctionDescriptor kRNDISFunctionDescriptor = {
    .interface_class = USB_CLASS_MISC,
    .interface_subclass = USB_SUBCLASS_MSC_RNDIS,
    .interface_protocol = USB_PROTOCOL_MSC_RNDIS_ETHERNET,
};

constexpr peripheral::wire::FunctionDescriptor kADBFunctionDescriptor = {
    .interface_class = USB_CLASS_VENDOR,
    .interface_subclass = USB_SUBCLASS_ADB,
    .interface_protocol = USB_PROTOCOL_ADB,
};

constexpr peripheral::wire::FunctionDescriptor kFfxFunctionDescriptor = {
    .interface_class = USB_CLASS_VENDOR,
    .interface_subclass = USB_SUBCLASS_VSOCK_BRIDGE,
    .interface_protocol = USB_PROTOCOL_VSOCK_BRIDGE,
};

constexpr peripheral::wire::FunctionDescriptor kFastbootFunctionDescriptor = {
    .interface_class = USB_CLASS_VENDOR,
    .interface_subclass = USB_SUBCLASS_FASTBOOT,
    .interface_protocol = USB_PROTOCOL_FASTBOOT,
};

constexpr peripheral::wire::FunctionDescriptor kTestFunctionDescriptor = {
    .interface_class = USB_CLASS_VENDOR,
    .interface_subclass = 0,
    .interface_protocol = 0,
};

// Class for generating USB peripheral config struct.
// Currently supports getting a CDC Ethernet config by default, or parse the boot args
// `driver.usb.peripheral` string to compose different functionality.
class PeripheralConfigParser {
 public:
  zx_status_t AddFunctions(const std::vector<std::string>& functions);

  uint16_t vid() const { return GOOGLE_USB_VID; }
  uint16_t pid() const { return pid_; }
  std::string manufacturer() const { return std::string(kManufacturer); }
  std::string product() const { return product_desc_; }

  std::vector<fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor>& functions() {
    return function_configs_;
  }

 private:
  // Helper function for determining the pid and product description.
  zx_status_t SetCompositeProductDescription(uint16_t pid, const std::string_view& desc);

  uint16_t pid_ = 0;
  std::string product_desc_;
  std::vector<fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor> function_configs_;
};

}  // namespace usb_peripheral

#endif  // SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_CONFIG_PARSER_H_
