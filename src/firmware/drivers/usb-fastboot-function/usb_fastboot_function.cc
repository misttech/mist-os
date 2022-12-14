// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/firmware/drivers/usb-fastboot-function/usb_fastboot_function.h"

#include <lib/ddk/debug.h>
#include <lib/zircon-internal/align.h>

#include "src/firmware/drivers/usb-fastboot-function/usb_fastboot_function-bind.h"

namespace usb_fastboot_function {

void UsbFastbootFunction::TxComplete(usb_request_t* req) {
  // TODO(b/260651258): To implement
}

void UsbFastbootFunction::Send(::fuchsia_hardware_fastboot::wire::FastbootImplSendRequest* request,
                               SendCompleter::Sync& completer) {
  // TODO(b/260651258): To implement
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void UsbFastbootFunction::RxComplete(usb_request_t* req) {
  // TODO(b/260651258): To implement
}

void UsbFastbootFunction::Receive(
    ::fuchsia_hardware_fastboot::wire::FastbootImplReceiveRequest* request,
    ReceiveCompleter::Sync& completer) {
  // TODO(b/260651258): To implement
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

size_t UsbFastbootFunction::UsbFunctionInterfaceGetDescriptorsSize() {
  return sizeof(descriptors_);
}

void UsbFastbootFunction::UsbFunctionInterfaceGetDescriptors(uint8_t* buffer, size_t buffer_size,
                                                             size_t* out_actual) {
  const size_t length = std::min(sizeof(descriptors_), buffer_size);
  std::memcpy(buffer, &descriptors_, length);
  *out_actual = length;
}

zx_status_t UsbFastbootFunction::UsbFunctionInterfaceControl(
    const usb_setup_t* setup, const uint8_t* write_buffer, size_t write_size,
    uint8_t* out_read_buffer, size_t read_size, size_t* out_read_actual) {
  if (out_read_actual != NULL) {
    *out_read_actual = 0;
  }
  return ZX_OK;
}

zx_status_t UsbFastbootFunction::ConfigureEndpoints(bool enable) {
  zx_status_t status;
  if (enable) {
    if ((status = function_.ConfigEp(&descriptors_.bulk_out_ep, NULL)) != ZX_OK ||
        (status = function_.ConfigEp(&descriptors_.bulk_in_ep, NULL)) != ZX_OK) {
      zxlogf(ERROR, "usb_function_config_ep failed - %d.", status);
      return status;
    }
  } else {
    if ((status = function_.DisableEp(bulk_out_addr())) != ZX_OK ||
        (status = function_.DisableEp(bulk_in_addr())) != ZX_OK) {
      zxlogf(ERROR, "usb_function_disable_ep failed - %d.", status);
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t UsbFastbootFunction::UsbFunctionInterfaceSetConfigured(bool configured,
                                                                   usb_speed_t speed) {
  zxlogf(INFO, "configured? - %d  speed - %d.", configured, speed);
  return ConfigureEndpoints(configured);
}

zx_status_t UsbFastbootFunction::UsbFunctionInterfaceSetInterface(uint8_t interface,
                                                                  uint8_t alt_setting) {
  zxlogf(INFO, "interface - %d alt_setting - %d.", interface, alt_setting);
  if (interface != descriptors_.fastboot_intf.b_interface_number || alt_setting > 1) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ConfigureEndpoints(alt_setting);
}

zx_status_t UsbFastbootFunction::Bind(void* ctx, zx_device_t* dev) {
  auto driver = std::make_unique<UsbFastbootFunction>(dev);
  zx_status_t status = driver->Bind();
  if (status != ZX_OK) {
    return status;
  }
  // The DriverFramework now owns driver.
  [[maybe_unused]] auto ptr = driver.release();
  return ZX_OK;
}

zx_status_t UsbFastbootFunction::Bind() {
  is_bound.Set(true);
  parent_request_size_ = function_.GetRequestSize();
  usb_request_size_ = usb::Request<>::RequestSize(parent_request_size_);

  auto status = function_.AllocInterface(&descriptors_.fastboot_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Fastboot interface alloc failed - %d.", status);
    return status;
  }

  status = function_.AllocInterface(&descriptors_.placehodler_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Placeholder interface alloc failed - %d.", status);
    return status;
  }

  status = function_.AllocEp(USB_DIR_OUT, &descriptors_.bulk_out_ep.b_endpoint_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Bulk out endpoint alloc failed - %d.", status);
    return status;
  }
  status = function_.AllocEp(USB_DIR_IN, &descriptors_.bulk_in_ep.b_endpoint_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Builk in endpoint alloc failed - %d.", status);
    return status;
  }

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  return DdkAdd(
      ddk::DeviceAddArgs("usb_fastboot_function").set_inspect_vmo(inspect_.DuplicateVmo()));
}

void UsbFastbootFunction::DdkInit(ddk::InitTxn txn) { txn.Reply(ZX_OK); }

void UsbFastbootFunction::DdkRelease() { delete this; }

static zx_driver_ops_t usb_fastboot_function_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = UsbFastbootFunction::Bind;
  return ops;
}();

}  // namespace usb_fastboot_function

ZIRCON_DRIVER(UsbFastbootFunction, usb_fastboot_function::usb_fastboot_function_driver_ops,
              "zircon", "0.1");
