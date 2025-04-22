// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/firmware/drivers/usb-fastboot-function/usb_fastboot_function.h"

#include <lib/driver/compat/cpp/compat.h>
#include <lib/zircon-internal/align.h>

namespace usb_fastboot_function {
namespace {
size_t CalculateRxHeaderLength(size_t data_size) {
  // Adjusts USB RX request length to bypass zero-length-packet. Upstream fastboot implementation
  // doesn't send zero-length packet when download completes. This causes host side driver to stall
  // if size of download data happens to be multiples of USB max packet size but not multiples of
  // bulk size. For example, suppose bulk request size is 2048, and USB max packet size is 512, and
  // host is sending the last 512/1024/1536 bytes during download, this last packet will not reach
  // this driver immediately. But if the host sends another 10 bytes of data, the driver will
  // receive a single packet of size (512/1024/1536 + 10) bytes. Thus we adjust the value of bulk
  // request based on the expected amount of data to receive. The size is required to be multiples
  // of kBulkMaxPacketSize. Thus we adjust it to be the smaller between kBulkReqSize and the round
  // up value of `data_size' w.r.t kBulkMaxPacketSize. For example, if we are expecting exactly 512
  // bytes of data, the following will give 512 exactly.
  return std::min(static_cast<size_t>(kBulkReqSize), ZX_ROUNDUP(data_size, kBulkMaxPacketSize));
}
}  // namespace

void UsbFastbootFunction::CleanUpTx(zx_status_t status, usb::FidlRequest req) {
  send_vmo_.Reset();
  bulk_in_ep_.PutRequest(std::move(req));
  if (status == ZX_OK) {
    send_completer_->ReplySuccess();
  } else {
    send_completer_->ReplyError(status);
  }
  send_completer_.reset();
}

void UsbFastbootFunction::QueueTx(usb::FidlRequest req) {
  size_t to_send = std::min(static_cast<size_t>(kBulkReqSize), total_to_send_ - sent_size_);
  auto actual = req.CopyTo(0, static_cast<uint8_t*>(send_vmo_.start()) + sent_size_, to_send,
                           bulk_in_ep_.GetMapped());
  ZX_ASSERT(actual.size() == 1);
  req->data()->at(0).size(actual[0]);
  if (auto status = req.CacheFlush(bulk_in_ep_.GetMapped()); status != ZX_OK) {
    ZX_PANIC("Cache flush failed %d", status);
  }

  std::vector<fuchsia_hardware_usb_request::Request> requests;
  requests.emplace_back(req.take_request());
  auto result = bulk_in_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    ZX_PANIC("Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
  }
}

void UsbFastbootFunction::TxComplete(fuchsia_hardware_usb_endpoint::Completion completion) {
  std::lock_guard<std::mutex> _(send_lock_);
  usb::FidlRequest req{std::move(completion.request().value())};
  auto status = *completion.status();
  // Do not queue request on error.
  if (status != ZX_OK) {
    zxlogf(ERROR, "tx_completion error: %s", zx_status_get_string(status));
    CleanUpTx(status, std::move(req));
    return;
  }

  // If succeeds, update `sent_size_`, otherwise keep it the same to retry.
  sent_size_ += *completion.transfer_size();
  if (sent_size_ == total_to_send_) {
    CleanUpTx(ZX_OK, std::move(req));
    return;
  }

  QueueTx(std::move(req));
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

  if (zx_status_t status = send_vmo_.Map(std::move(request->data), 0, total_to_send_,
                                         ZX_VM_PERM_READ | ZX_VM_PERM_WRITE);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to map vmo %d", status);
    completer.ReplyError(status);
    return;
  }

  auto req = bulk_in_ep_.GetRequest();
  ZX_ASSERT(req);
  sent_size_ = 0;
  send_completer_ = completer.ToAsync();
  QueueTx(std::move(*req));
}

void UsbFastbootFunction::CleanUpRx(zx_status_t status, usb::FidlRequest req) {
  bulk_out_ep_.PutRequest(std::move(req));
  if (status == ZX_OK) {
    receive_completer_->ReplySuccess(receive_vmo_.Release());
  } else {
    receive_vmo_.Reset();
    receive_completer_->ReplyError(status);
  }
  receive_completer_.reset();
}

void UsbFastbootFunction::QueueRx(usb::FidlRequest req) {
  ZX_ASSERT(req->data()->size() == 1);
  req.reset_buffers(bulk_out_ep_.GetMapped());
  req->data()->at(0).size(CalculateRxHeaderLength(requested_size_ - received_size_));
  if (auto status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped()); status != ZX_OK) {
    ZX_PANIC("Cache flush and invalidate failed %d", status);
  }

  std::vector<fuchsia_hardware_usb_request::Request> requests;
  requests.emplace_back(req.take_request());
  auto result = bulk_out_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    ZX_PANIC("Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
  }
}

void UsbFastbootFunction::RxComplete(fuchsia_hardware_usb_endpoint::Completion completion) {
  std::lock_guard<std::mutex> _(receive_lock_);

  usb::FidlRequest req{std::move(completion.request().value())};
  zx_status_t status = *completion.status();

  if (status != ZX_OK) {
    zxlogf(ERROR, "rx_completion error: %s", zx_status_get_string(status));
    CleanUpRx(status, std::move(req));
    return;
  }

  // This should always be true because when we registered VMOs, we only registered one per
  // request.
  ZX_ASSERT(req->data()->size() == 1);
  auto addr = bulk_out_ep_.GetMappedAddr(req.request(), 0);
  if (!addr.has_value()) {
    zxlogf(ERROR, "Failed to get mapped");
    CleanUpRx(ZX_ERR_INTERNAL, std::move(req));
    return;
  }

  if (auto status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped()); status != ZX_OK) {
    ZX_PANIC("Cache flush and invalidate failed %d", status);
  }

  const uint8_t* data = reinterpret_cast<const uint8_t*>(*addr);
  memcpy(static_cast<uint8_t*>(receive_vmo_.start()) + received_size_, data,
         *completion.transfer_size());
  received_size_ += *completion.transfer_size();
  if (received_size_ >= requested_size_) {
    zx_status_t status = receive_vmo_.vmo().set_prop_content_size(received_size_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to set content size %d", status);
    }
    CleanUpRx(status, std::move(req));
    return;
  }

  QueueRx(std::move(req));
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

  auto req = bulk_out_ep_.GetRequest();
  ZX_ASSERT(req);
  receive_completer_ = completer.ToAsync();
  QueueRx(std::move(*req));
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

zx::result<> UsbFastbootFunction::Start() {
  std::lock_guard<std::mutex> _guard_receive(receive_lock_);
  std::lock_guard<std::mutex> _guard_send(send_lock_);
  zx::result<ddk::UsbFunctionProtocolClient> function =
      compat::ConnectBanjo<ddk::UsbFunctionProtocolClient>(incoming());
  if (function.is_error()) {
    zxlogf(ERROR, "Failed to connect function %s", function.status_string());
    return function.take_error();
  }
  function_ = *function;

  auto client = incoming()->Connect<fuchsia_hardware_usb_function::UsbFunctionService::Device>();

  auto status = function_.AllocInterface(&descriptors_.fastboot_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Fastboot interface alloc failed - %d.", status);
    return zx::error(status);
  }

  status = function_.AllocInterface(&descriptors_.placehodler_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Placeholder interface alloc failed - %d.", status);
    return zx::error(status);
  }

  status = function_.AllocEp(USB_DIR_OUT, &descriptors_.bulk_out_ep.b_endpoint_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Bulk out endpoint alloc failed - %d.", status);
    return zx::error(status);
  }
  status = function_.AllocEp(USB_DIR_IN, &descriptors_.bulk_in_ep.b_endpoint_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Builk in endpoint alloc failed - %d.", status);
    return zx::error(status);
  }

  auto dispatcher =
      fdf::SynchronizedDispatcher::Create({}, "fastboot-ep-dispatcher", [](fdf_dispatcher_t*) {});
  if (dispatcher.is_error()) {
    zxlogf(ERROR, "[bug] fdf::SynchronizedDispatcher::Create(): %s", dispatcher.status_string());
    return dispatcher.take_error();
  }
  dispatcher_ = std::move(dispatcher.value());

  // Allocates a bulk out usb request.
  status = bulk_out_ep_.Init(descriptors_.bulk_out_ep.b_endpoint_address, *client,
                             dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] bulk_out_ep_.Init(): %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // Allocates a bulk in usb request.
  status = bulk_in_ep_.Init(descriptors_.bulk_in_ep.b_endpoint_address, *client,
                            dispatcher_.async_dispatcher());
  if (status != ZX_OK) {
    zxlogf(ERROR, "[bug] bulk_in_ep_.Init(): %s", zx_status_get_string(status));
    return zx::error(status);
  }

  // Allocates RX request
  if (auto actual = bulk_out_ep_.AddRequests(1, kBulkReqSize,
                                             fuchsia_hardware_usb_request::Buffer::Tag::kVmoId);
      actual != 1) {
    zxlogf(ERROR, "Failed to allocate RX requests");
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Allocates TX request
  if (auto actual = bulk_in_ep_.AddRequests(1, kBulkReqSize,
                                            fuchsia_hardware_usb_request::Buffer::Tag::kVmoId);
      actual != 1) {
    zxlogf(ERROR, "Failed to allocate TX requests");
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto serve_result = outgoing()->AddService<fuchsia_hardware_fastboot::Service>(
      fuchsia_hardware_fastboot::Service::InstanceHandler({
          .fastboot = bindings_.CreateHandler(
              this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
      }));
  if (serve_result.is_error()) {
    zxlogf(ERROR, "Failed to add Device service %s", serve_result.status_string());
    return serve_result.take_error();
  }

  status = function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  if (status != ZX_OK) {
    ZX_PANIC("SetInterface failed %s", zx_status_get_string(status));
  }

  is_bound.Set(true);
  return zx::ok();
}

void UsbFastbootFunction::PrepareStop(fdf::PrepareStopCompleter completer) { completer(zx::ok()); }

}  // namespace usb_fastboot_function

FUCHSIA_DRIVER_EXPORT(usb_fastboot_function::UsbFastbootFunction);
