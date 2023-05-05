// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/firmware/drivers/usb-fastboot-function/usb_fastboot_function.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/zircon-internal/align.h>

namespace usb_fastboot_function {
namespace {
size_t CalculateRxHeaderLength(size_t data_size) {
  // By default `usb_request_t.header.length` takes the value of `kBulkReqSize` when it is
  // allocated with `usb::Request<>::Alloc(...)`. It seems to affect the amount of data to be
  // received from the host. One phenomena I observe is that when 1) the host sends less than
  // `usb_request_t.header.length` amount of data in a packet and 2) the data size is a multiple of
  // kBulkMaxPacketSize, the packet will appear to be "delayed" and combined with the next packet.
  // For example, suppose usb_request_t.header.length=2048, kBulkMaxPacketSize=512, and host is
  // sending the last 512/1024/1536 bytes during download, this last packet will not reach this
  // driver immediately. But if the host sends another 10 bytes of data, the driver will receive a
  // single packet of size (512/1024/1536 + 10) bytes. This causes hangs during download. Thus we
  // adjust the value of `usb_request_t.header.length` based on the expected amount of data to
  // receive. usb_request_t.header.length is required to be multiples of kBulkMaxPacketSize. Thus we
  // adjust it to be the smaller between kBulkReqSize and the round up value of `data_size' w.r.t
  // kBulkMaxPacketSize. For example, if we are expecting exactly 512 bytes of data, the following
  // will give 512 exactly.
  return std::min(static_cast<size_t>(kBulkReqSize), ZX_ROUNDUP(data_size, kBulkMaxPacketSize));
}
}  // namespace

void UsbFastbootFunction::CleanUpTx(zx_status_t status, usb::Request<> req) {
  send_vmo_.Reset();
  bulk_in_reqs_.Add(std::move(req));
  if (status == ZX_OK) {
    send_completer_->ReplySuccess();
  } else {
    send_completer_->ReplyError(status);
  }
  send_completer_.reset();
}

zx_status_t UsbFastbootFunction::PrepareSendRequest(usb::Request<>& request) {
  size_t to_send = std::min(static_cast<size_t>(kBulkReqSize), total_to_send_ - sent_size_);
  request.request()->header.length = to_send;
  ssize_t bytes_copied =
      request.CopyTo(static_cast<uint8_t*>(send_vmo_.start()) + sent_size_, to_send, 0);
  if (bytes_copied < 0) {
    zxlogf(ERROR, "Failed to copy data into send req %zd.", bytes_copied);
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

void UsbFastbootFunction::TxComplete(usb_request_t* req) {
  std::lock_guard<std::mutex> _(send_lock_);
  usb::Request<> request(req, parent_request_size_);

  // Following the same practice as cdc eth and adb, do not queue request if error is
  // ZX_ERR_IO_NOT_PRESENT.
  if (req->response.status == ZX_ERR_IO_NOT_PRESENT) {
    CleanUpTx(req->response.status, std::move(request));
    return;
  }

  // If succeeds, update `sent_size_`, otherwise keep it the same to retry.
  if (req->response.status == ZX_OK) {
    sent_size_ += req->header.length;
    if (sent_size_ == total_to_send_) {
      CleanUpTx(ZX_OK, std::move(request));
      return;
    }
  }

  if (zx_status_t status = PrepareSendRequest(request); status != ZX_OK) {
    CleanUpTx(ZX_ERR_INTERNAL, std::move(request));
    return;
  }

  function_.RequestQueue(request.take(), &tx_complete_);
}

void UsbFastbootFunction::Send(::fuchsia_hardware_fastboot::wire::FastbootImplSendRequest* request,
                               SendCompleter::Sync& completer) {
  if (!configured_) {
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  std::lock_guard<std::mutex> _(send_lock_);
  if (send_completer_.has_value()) {
    // A previous call to Send() is pending
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  if (zx_status_t status = request->data.get_prop_content_size(&total_to_send_); status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  if (total_to_send_ == 0) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (zx_status_t status = send_vmo_.Map(std::move(request->data), total_to_send_);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to map vmo %d", status);
    completer.ReplyError(status);
    return;
  }

  std::optional<usb::Request<>> tx_request = bulk_in_reqs_.Get(usb_request_size_);
  if (!tx_request) {
    zxlogf(ERROR, "Failed to get tx request");
    send_vmo_.Reset();
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  sent_size_ = 0;
  send_completer_ = completer.ToAsync();

  if (zx_status_t status = PrepareSendRequest(*tx_request); status != ZX_OK) {
    CleanUpTx(status, std::move(*tx_request));
    return;
  }

  function_.RequestQueue(tx_request->take(), &tx_complete_);
}

void UsbFastbootFunction::CleanUpRx(zx_status_t status, usb::Request<> req) {
  bulk_out_reqs_.Add(std::move(req));
  if (status == ZX_OK) {
    receive_completer_->ReplySuccess(receive_vmo_.Release());
  } else {
    receive_vmo_.Reset();
    receive_completer_->ReplyError(status);
  }
  receive_completer_.reset();
}

void UsbFastbootFunction::RxComplete(usb_request_t* req) {
  std::lock_guard<std::mutex> _(receive_lock_);
  usb::Request<> request(req, parent_request_size_);

  // Following the same practice as cdc eth and adb driver, do not queue request if error is
  // ZX_ERR_IO_NOT_PRESENT.
  if (req->response.status == ZX_ERR_IO_NOT_PRESENT) {
    zxlogf(INFO, "IO not present");
    CleanUpRx(req->response.status, std::move(request));
    return;
  }

  if (req->response.status != ZX_OK) {
    // For all other failures, simply re-queue the request.
    function_.RequestQueue(request.take(), &rx_complete_);
    return;
  }

  // Extracts data from the request
  void* data;
  if (zx_status_t status = request.Mmap(&data); status != ZX_OK) {
    zxlogf(ERROR, "Failed to map request data %d", status);
    CleanUpRx(status, std::move(request));
    return;
  }

  memcpy(static_cast<uint8_t*>(receive_vmo_.start()) + received_size_, data, req->response.actual);
  received_size_ += req->response.actual;
  if (received_size_ >= requested_size_) {
    zx_status_t status = receive_vmo_.vmo().set_prop_content_size(received_size_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to set content size %d", status);
    }
    CleanUpRx(status, std::move(request));
    return;
  }

  request.request()->header.length = CalculateRxHeaderLength(requested_size_ - received_size_);
  function_.RequestQueue(request.take(), &rx_complete_);
}

void UsbFastbootFunction::Receive(
    ::fuchsia_hardware_fastboot::wire::FastbootImplReceiveRequest* request,
    ReceiveCompleter::Sync& completer) {
  if (!configured_) {
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  std::lock_guard<std::mutex> _(receive_lock_);
  if (receive_completer_.has_value()) {
    // A previous call to Receive() is pending
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  received_size_ = 0;
  // Minimum set to 1 so that round up works correctly.
  requested_size_ = std::max(uint64_t{1}, request->requested);
  // Create vmo for receiving data. Roundup by `kBulkMaxPacketSize` since USB transmission is in
  // the unit of packet.
  zx_status_t status = receive_vmo_.CreateAndMap(ZX_ROUNDUP(requested_size_, kBulkMaxPacketSize),
                                                 "usb fastboot receive");
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create vmo %d.", status);
    completer.ReplyError(status);
    return;
  }

  std::optional<usb::Request<>> rx_request = bulk_out_reqs_.Get(usb_request_size_);
  if (!rx_request) {
    zxlogf(ERROR, "Failed to get rx request");
    receive_vmo_.Reset();
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  receive_completer_ = completer.ToAsync();
  rx_request->request()->header.length = CalculateRxHeaderLength(requested_size_);
  function_.RequestQueue(rx_request->take(), &rx_complete_);
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
    configured_ = true;
  } else {
    if ((status = function_.DisableEp(bulk_out_addr())) != ZX_OK ||
        (status = function_.DisableEp(bulk_in_addr())) != ZX_OK) {
      zxlogf(ERROR, "usb_function_disable_ep failed - %d.", status);
      return status;
    }
    configured_ = false;
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

  // Allocate bulk out usb requests.
  std::optional<usb::Request<>> request;
  status = usb::Request<>::Alloc(&request, kBulkReqSize, bulk_out_addr(), parent_request_size_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Allocating bulk out request failed - %d.", status);
    return status;
  }
  {
    std::lock_guard<std::mutex> _(receive_lock_);
    bulk_out_reqs_.Add(*std::move(request));
  }

  // Allocate bulk in usb requests.
  status = usb::Request<>::Alloc(&request, kBulkReqSize, bulk_in_addr(), parent_request_size_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Allocating bulk in request failed - %d.", status);
    return status;
  }
  {
    std::lock_guard<std::mutex> _(send_lock_);
    bulk_in_reqs_.Add(*std::move(request));
  }

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  return DdkAdd(ddk::DeviceAddArgs("usb_fastboot_function")
                    .set_flags(DEVICE_ADD_NON_BINDABLE)
                    .set_inspect_vmo(inspect_.DuplicateVmo()));
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
