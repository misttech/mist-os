// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral/config-parser.h"

#include <lib/ddk/debug.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <string>
#include <vector>

namespace usb_peripheral {

namespace {

struct FunctionDefinition {
  uint16_t product_id;
  fuchsia_hardware_usb_peripheral::wire::FunctionDescriptor descriptor;
  std::string_view description;
};

const std::map<std::string_view, FunctionDefinition> all_functions = {
    {"cdc",
     {
         .product_id = GOOGLE_USB_CDC_PID,
         .descriptor = kCDCFunctionDescriptor,
         .description = kCDCProductDescription,
     }},
    {"ums",
     {
         .product_id = GOOGLE_USB_UMS_PID,
         .descriptor = kUMSFunctionDescriptor,
         .description = kUMSProductDescription,
     }},
    {"rndis",
     {
         .product_id = GOOGLE_USB_RNDIS_PID,
         .descriptor = kRNDISFunctionDescriptor,
         .description = kRNDISProductDescription,
     }},
    {"adb",
     {
         .product_id = GOOGLE_USB_ADB_PID,
         .descriptor = kADBFunctionDescriptor,
         .description = kADBProductDescription,
     }},
    {"fastboot",
     {
         .product_id = GOOGLE_USB_FASTBOOT_PID,
         .descriptor = kFastbootFunctionDescriptor,
         .description = kFastbootProductDescription,
     }},
    {"test",
     {
         .product_id = GOOGLE_USB_FUNCTION_TEST_PID,
         .descriptor = kTestFunctionDescriptor,
         .description = kTestProductDescription,
     }},
    {"vsock_bridge",
     {
         .product_id = GOOGLE_USB_VSOCK_BRIDGE_PID,
         .descriptor = kFfxFunctionDescriptor,
         .description = kVsockBridgeProductDescription,
     }},
};

}  // namespace

zx_status_t PeripheralConfigParser::AddFunctions(const std::vector<std::string>& functions) {
  if (functions.empty()) {
    zxlogf(INFO, "No functions found");
    return ZX_OK;
  }

  // resolve and then sort the functions by their product ids so that they are added and combined in
  // a predictable order.
  std::vector<FunctionDefinition> function_defs;
  for (const auto& function : functions) {
    auto function_def = all_functions.find(function);
    if (function_def != all_functions.end()) {
      function_defs.push_back(function_def->second);

    } else {
      zxlogf(ERROR, "Function not supported: %s", function.c_str());
      return ZX_ERR_INVALID_ARGS;
    }
  }
  std::ranges::sort(function_defs,
                    [](auto& left, auto& right) { return left.product_id < right.product_id; });

  zx_status_t status = ZX_OK;
  for (const auto& function : function_defs) {
    function_configs_.push_back(function.descriptor);
    status = SetCompositeProductDescription(function.product_id, function.description);

    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to set product description for 0x%x", function.product_id);
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t PeripheralConfigParser::SetCompositeProductDescription(uint16_t pid,
                                                                   const std::string_view& desc) {
  if (pid_ == 0) {
    product_desc_ = desc;
    pid_ = pid;
  } else {
    if (pid_ == GOOGLE_USB_CDC_PID && pid == GOOGLE_USB_FUNCTION_TEST_PID) {
      pid_ = GOOGLE_USB_CDC_AND_FUNCTION_TEST_PID;
    } else if (pid_ == GOOGLE_USB_CDC_PID && pid == GOOGLE_USB_ADB_PID) {
      pid_ = GOOGLE_USB_CDC_AND_ADB_PID;
    } else if (pid_ == GOOGLE_USB_ADB_PID && pid == GOOGLE_USB_VSOCK_BRIDGE_PID) {
      pid_ = GOOGLE_USB_ADB_AND_VSOCK_BRIDGE_PID;
    } else if (pid_ == GOOGLE_USB_CDC_PID && pid == GOOGLE_USB_VSOCK_BRIDGE_PID) {
      pid_ = GOOGLE_USB_CDC_AND_VSOCK_BRIDGE_PID;
    } else if (pid_ == GOOGLE_USB_CDC_AND_ADB_PID && pid == GOOGLE_USB_VSOCK_BRIDGE_PID) {
      pid_ = GOOGLE_USB_CDC_AND_ADB_AND_VSOCK_BRIDGE_PID;
    } else if (pid_ == GOOGLE_USB_CDC_PID && pid == GOOGLE_USB_FASTBOOT_PID) {
      pid_ = GOOGLE_USB_CDC_AND_FASTBOOT_PID;
    } else {
      zxlogf(ERROR, "No matching pid for this combination: 0x%x + 0x%x", pid_, pid);
      return ZX_ERR_WRONG_TYPE;
    }
    product_desc_ += kCompositeDeviceConnector;
    product_desc_ += desc;
  }
  return ZX_OK;
}

}  // namespace usb_peripheral
