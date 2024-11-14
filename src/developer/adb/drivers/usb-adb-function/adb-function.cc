// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adb-function.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/zx/vmar.h>
#include <zircon/assert.h>

#include <cstdint>
#include <optional>

#include <fbl/auto_lock.h>
#include <usb/peripheral.h>
#include <usb/request-cpp.h>

namespace usb_adb_function {

namespace {

// CompleterType follows fidl::internal::WireCompleter<RequestType>::Async
template <typename CompleterType>
void CompleteTxn(CompleterType& completer, zx_status_t status) {
  if (status == ZX_OK) {
    completer.Reply(fit::ok());
  } else {
    completer.Reply(fit::error(status));
  }
}

}  // namespace

void UsbAdbDevice::Start(StartRequestView request, StartCompleter::Sync& completer) {
  // Should always be started on a clean state.
  {
    fbl::AutoLock _(&bulk_in_ep_.mutex_);
    ZX_ASSERT(tx_pending_reqs_.empty());
  }
  {
    fbl::AutoLock _(&bulk_out_ep_.mutex_);
    ZX_ASSERT(rx_requests_.empty());
  }

  if (adb_binding_.has_value()) {
    zxlogf(ERROR, "Device is already bound");
    return CompleteTxn(completer, ZX_ERR_ALREADY_BOUND);
  }

  adb_binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                       std::move(request->interface), this, [this](fidl::UnbindInfo info) {
                         zxlogf(INFO, "Device closed with reason '%s'",
                                info.FormatDescription().c_str());
                         Stop();
                       });
  // Reset interface and configure endpoints as adb binding is set now.
  zx_status_t status = function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  if (status != ZX_OK && status != ZX_ERR_ALREADY_BOUND) {
    zxlogf(ERROR, "SetInterface failed %s", zx_status_get_string(status));
    CompleteTxn(completer, status);
    return;
  }
  auto result = fidl::WireSendEvent(adb_binding_.value())
                    ->OnStatusChanged(Online() ? fadb::StatusFlags::kOnline : fadb::StatusFlags(0));
  if (!result.ok()) {
    zxlogf(ERROR, "Could not call AdbInterface Status.");
    CompleteTxn(completer, result.error().status());
    return;
  }
  CompleteTxn(completer, status);

  // Run receive to pass up messages received while disconnected.
  fbl::AutoLock _(&bulk_out_ep_.mutex_);
  ReceiveLocked();
}

void UsbAdbDevice::Stop(StopCompleter::Sync& completer) {
  SetShutdownCallback([cmpl = completer.ToAsync()]() mutable { cmpl.ReplySuccess(); });
  Stop();
}

void UsbAdbDevice::Stop() {
  if (adb_binding_.has_value()) {
    auto result = fidl::WireSendEvent(adb_binding_.value())->OnStatusChanged(fadb::StatusFlags(0));
    if (!result.ok()) {
      zxlogf(ERROR, "Could not call AdbInterface Status.");
    }
    adb_binding_.reset();
  }

  // Cancel all requests in the pipeline -- the completion handler will free these requests as they
  // come in.
  //
  // Do not hold locks when calling this method. It might result in deadlock as completion callbacks
  // could be invoked during this call.
  bulk_out_ep_->CancelAll().Then([](fidl::Result<fendpoint::Endpoint::CancelAll>& result) {
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to cancel all for bulk out endpoint %s",
             result.error_value().FormatDescription().c_str());
    }
  });
  bulk_in_ep_->CancelAll().Then([](fidl::Result<fendpoint::Endpoint::CancelAll>& result) {
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to cancel all for bulk in endpoint %s",
             result.error_value().FormatDescription().c_str());
    }
  });

  {
    fbl::AutoLock _(&bulk_out_ep_.mutex_);
    while (!rx_requests_.empty()) {
      rx_requests_.front().Reply(fit::error(ZX_ERR_BAD_STATE));
      rx_requests_.pop();
    }
    while (!pending_replies_.empty()) {
      InsertUsbRequest(std::move(pending_replies_.front().request().value()), bulk_out_ep_);
      pending_replies_.pop();
    }
  }

  zx_status_t status = function_.SetInterface(nullptr, nullptr);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SetInterface failed %s", zx_status_get_string(status));
  }

  fbl::AutoLock _(&lock_);
  stop_completed_ = true;
  ShutdownComplete();
}

zx::result<> UsbAdbDevice::SendLocked() {
  if (tx_pending_reqs_.empty()) {
    return zx::ok();
  }

  if (!Online()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  auto& current = tx_pending_reqs_.front();
  std::vector<fuchsia_hardware_usb_request::Request> requests;
  while (current.start < current.request.data().size()) {
    auto req = bulk_in_ep_.GetRequest();
    if (!req) {
      break;
    }
    req->clear_buffers();

    size_t to_copy = std::min(current.request.data().size() - current.start, vmo_data_size_);
    auto actual = req->CopyTo(0, current.request.data().data() + current.start, to_copy,
                              bulk_in_ep_.GetMappedLocked);
    size_t actual_total = 0;
    for (size_t i = 0; i < actual.size(); i++) {
      // Fill in size of data.
      (*req)->data()->at(i).size(actual[i]);
      actual_total += actual[i];
    }
    auto status = req->CacheFlush(bulk_in_ep_.GetMappedLocked);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Cache flush failed %d", status);
    }

    requests.emplace_back(req->take_request());
    current.start += actual_total;
  }

  if (!requests.empty()) {
    auto result = bulk_in_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
    }
  }

  if (current.start == current.request.data().size()) {
    CompleteTxn(current.completer, ZX_OK);
    tx_pending_reqs_.pop();
  }

  return zx::ok();
}

void UsbAdbDevice::ReceiveLocked() {
  if (pending_replies_.empty() || rx_requests_.empty()) {
    return;
  }

  auto completion = std::move(pending_replies_.front());
  pending_replies_.pop();

  auto req = usb::FidlRequest(std::move(completion.request().value()));

  if (*completion.status() != ZX_OK) {
    zxlogf(ERROR, "RxComplete called with error %d.", *completion.status());
    rx_requests_.front().Reply(fit::error(ZX_ERR_INTERNAL));
  } else {
    // This should always be true because when we registered VMOs, we only registered one per
    // request.
    ZX_ASSERT(req->data()->size() == 1);
    auto addr = bulk_out_ep_.GetMappedAddrLocked(req.request(), 0);
    if (!addr.has_value()) {
      zxlogf(ERROR, "Failed to get mapped");
      rx_requests_.front().Reply(fit::error(ZX_ERR_INTERNAL));
    } else {
      rx_requests_.front().Reply(fit::ok(
          std::vector<uint8_t>(reinterpret_cast<uint8_t*>(*addr),
                               reinterpret_cast<uint8_t*>(*addr) + *completion.transfer_size())));
    }
  }
  rx_requests_.pop();

  req.reset_buffers(bulk_out_ep_.GetMappedLocked);
  auto status = req.CacheFlushInvalidate(bulk_out_ep_.GetMappedLocked);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
  }

  std::vector<fuchsia_hardware_usb_request::Request> requests;
  requests.emplace_back(req.take_request());
  auto result = bulk_out_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
  }
}

void UsbAdbDevice::QueueTx(QueueTxRequest& request, QueueTxCompleter::Sync& completer) {
  size_t length = request.data().size();

  if (!Online() || length == 0) {
    zxlogf(INFO, "Invalid state - Online %d Length %zu", Online(), length);
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  fbl::AutoLock _(&bulk_in_ep_.mutex_);
  tx_pending_reqs_.emplace(
      txn_req_t{.request = std::move(request), .start = 0, .completer = completer.ToAsync()});

  auto result = SendLocked();
  if (result.is_error()) {
    zxlogf(INFO, "SendLocked failed %d", result.error_value());
  }
}

void UsbAdbDevice::Receive(ReceiveCompleter::Sync& completer) {
  // Return early during shutdown.
  if (!Online()) {
    completer.Reply(fit::error(ZX_ERR_BAD_STATE));
    return;
  }

  fbl::AutoLock lock(&bulk_out_ep_.mutex_);
  rx_requests_.emplace(completer.ToAsync());
  ReceiveLocked();
}

zx_status_t UsbAdbDevice::InsertUsbRequest(fuchsia_hardware_usb_request::Request req,
                                           usb::EndpointClient<UsbAdbDevice>& ep) {
  ep.PutRequest(usb::FidlRequest(std::move(req)));
  fbl::AutoLock _(&lock_);
  // Return without adding the request to the pool during shutdown.
  auto ret = shutdown_callback_ ? ZX_ERR_CANCELED : ZX_OK;
  ShutdownComplete();
  return ret;
}

void UsbAdbDevice::RxComplete(fendpoint::Completion completion) {
  // This should always be true because when we registered VMOs, we only registered one per request.
  ZX_ASSERT(completion.request()->data()->size() == 1);
  // Return early during shutdown.
  bool is_shutdown = [this]() {
    fbl::AutoLock _(&lock_);
    return !!shutdown_callback_;
  }();
  if (is_shutdown || (*completion.status() == ZX_ERR_IO_NOT_PRESENT)) {
    InsertUsbRequest(std::move(completion.request().value()), bulk_out_ep_);
    return;
  }

  fbl::AutoLock _(&bulk_out_ep_.mutex_);
  pending_replies_.push(std::move(completion));
  ReceiveLocked();
}

void UsbAdbDevice::TxComplete(fendpoint::Completion completion) {
  fbl::AutoLock _(&bulk_in_ep_.mutex_);
  if (InsertUsbRequest(std::move(completion.request().value()), bulk_in_ep_) != ZX_OK) {
    return;
  }
  // Do not queue requests if status is ZX_ERR_IO_NOT_PRESENT, as the underlying connection could
  // be disconnected or USB_RESET is being processed. Calling adb_send_locked in such scenario
  // will deadlock and crash the driver (see https://fxbug.dev/42174506).
  if (*completion.status() == ZX_ERR_IO_NOT_PRESENT) {
    return;
  }

  auto result = SendLocked();
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to SendLocked %d", result.error_value());
  }
}

size_t UsbAdbDevice::UsbFunctionInterfaceGetDescriptorsSize() { return sizeof(descriptors_); }

void UsbAdbDevice::UsbFunctionInterfaceGetDescriptors(uint8_t* buffer, size_t buffer_size,
                                                      size_t* out_actual) {
  const size_t length = std::min(sizeof(descriptors_), buffer_size);
  std::memcpy(buffer, &descriptors_, length);
  *out_actual = length;
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceControl(const usb_setup_t* setup,
                                                      const uint8_t* write_buffer,
                                                      size_t write_size, uint8_t* out_read_buffer,
                                                      size_t read_size, size_t* out_read_actual) {
  if (out_read_actual != NULL) {
    *out_read_actual = 0;
  }

  return ZX_OK;
}

zx_status_t UsbAdbDevice::ConfigureEndpoints(bool enable) {
  zx_status_t status;
  // Configure endpoint if not already done.
  if (enable && !bulk_out_ep_.RequestsEmpty()) {
    status = function_.ConfigEp(&descriptors_.bulk_out_ep, nullptr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to Config BULK OUT ep - %d.", status);
      return status;
    }

    status = function_.ConfigEp(&descriptors_.bulk_in_ep, nullptr);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to Config BULK IN ep - %d.", status);
      return status;
    }

    // queue RX requests
    std::vector<fuchsia_hardware_usb_request::Request> requests;
    while (auto req = bulk_out_ep_.GetRequest()) {
      req->reset_buffers(bulk_out_ep_.GetMapped);
      auto status = req->CacheFlushInvalidate(bulk_out_ep_.GetMapped);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
      }

      requests.emplace_back(req->take_request());
    }
    auto result = bulk_out_ep_->QueueRequests(std::move(requests));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
      return result.error_value().status();
    }
    zxlogf(INFO, "ADB endpoints configured.");
  } else if (!enable) {
    status = function_.DisableEp(bulk_out_addr());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to disable BULK OUT ep - %d.", status);
      return status;
    }

    status = function_.DisableEp(bulk_in_addr());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to disable BULK IN ep - %d.", status);
      return status;
    }
  }

  if (adb_binding_.has_value()) {
    auto result = fidl::WireSendEvent(adb_binding_.value())
                      ->OnStatusChanged(enable ? fadb::StatusFlags::kOnline : fadb::StatusFlags(0));
    if (!result.ok()) {
      zxlogf(ERROR, "Could not call AdbInterface Status.");
      return ZX_ERR_IO;
    }
  }

  return ZX_OK;
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  zxlogf(INFO, "configured? - %d  speed - %d.", configured, speed);
  {
    fbl::AutoLock _(&lock_);
    status_ = fadb::StatusFlags(configured);
  }

  return ConfigureEndpoints(configured);
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  zxlogf(INFO, "interface - %d alt_setting - %d.", interface, alt_setting);

  if (interface != descriptors_.adb_intf.b_interface_number || alt_setting > 1) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto status = ConfigureEndpoints(alt_setting);
  if (status != ZX_OK) {
    zxlogf(ERROR, "ConfigureEndpoints failed %d", status);
    return status;
  }

  {
    fbl::AutoLock _(&lock_);
    status_ = alt_setting ? fadb::StatusFlags::kOnline : fadb::StatusFlags(0);
  }

  return status;
}

void UsbAdbDevice::ShutdownComplete() {
  // Multiple threads/callbacks could observe pending_request == 0 and call ShutdownComplete
  // multiple times. Only call the callback if not already called.
  // Only call the callback if all requests are returned.
  if (stop_completed_ && shutdown_callback_ && bulk_in_ep_.RequestsFull() &&
      bulk_out_ep_.RequestsFull()) {
    shutdown_callback_();
    stop_completed_ = false;
  }
}

void UsbAdbDevice::DdkUnbind(ddk::UnbindTxn txn) {
  SetShutdownCallback([unbind_txn = std::move(txn)]() mutable { unbind_txn.Reply(); });
  auto status = ConfigureEndpoints(false);
  if (status != ZX_OK) {
    zxlogf(ERROR, "ConfigureEndpoints failed %s", zx_status_get_string(status));
  }
  Stop();
}

void UsbAdbDevice::DdkRelease() { delete this; }

void UsbAdbDevice::DdkSuspend(ddk::SuspendTxn txn) {
  SetShutdownCallback([suspend_txn = std::move(txn)]() mutable {
    suspend_txn.Reply(ZX_OK, suspend_txn.requested_state());
  });
  auto status = ConfigureEndpoints(false);
  if (status != ZX_OK) {
    zxlogf(ERROR, "ConfigureEndpoints failed %s", zx_status_get_string(status));
  }
  Stop();
}

zx_status_t UsbAdbDevice::InitEndpoint(
    fidl::ClientEnd<fuchsia_hardware_usb_function::UsbFunction>& client, uint8_t direction,
    uint8_t* ep_addrs, usb::EndpointClient<UsbAdbDevice>& ep, uint32_t req_count) {
  auto status = function_.AllocEp(direction, ep_addrs);
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb_function_alloc_ep failed - %d.", status);
    return status;
  }

  status = ep.Init(*ep_addrs, client, dispatcher_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to init UsbEndpoint %d", status);
    return status;
  }

  // TODO(127854): When we support active pinning of VMOs, adb may want to use VMOs that are not
  // perpetually pinned.
  auto actual =
      ep.AddRequests(req_count, vmo_data_size_, fuchsia_hardware_usb_request::Buffer::Tag::kVmoId);
  if (actual != req_count) {
    zxlogf(ERROR, "Wanted %u requests, only got %zu requests", req_count, actual);
  }
  return actual == 0 ? ZX_ERR_INTERNAL : ZX_OK;
}

zx_status_t UsbAdbDevice::Init() {
  auto client =
      DdkConnectFidlProtocol<fuchsia_hardware_usb_function::UsbFunctionService::Device>(parent_);
  if (client.is_error()) {
    zxlogf(ERROR, "Failed to connect fidl protocol");
    return client.error_value();
  }

  auto status = function_.AllocInterface(&descriptors_.adb_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb_function_alloc_interface failed - %d.", status);
    return status;
  }

  status = InitEndpoint(*client, USB_DIR_OUT, &descriptors_.bulk_out_ep.b_endpoint_address,
                        bulk_out_ep_, bulk_rx_count_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "InitEndpoint failed - %d.", status);
    return status;
  }
  status = InitEndpoint(*client, USB_DIR_IN, &descriptors_.bulk_in_ep.b_endpoint_address,
                        bulk_in_ep_, bulk_tx_count_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "InitEndpoint failed - %d.", status);
    return status;
  }

  status = DdkAdd("usb-adb-function", DEVICE_ADD_NON_BINDABLE);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not add UsbAdbDevice %d.", status);
    return status;
  }

  status = function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SetInterface failed %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t UsbAdbDevice::Bind(void* ctx, zx_device_t* parent) {
  auto adb = std::make_unique<UsbAdbDevice>(parent, 16, 16, 2048);
  if (!adb) {
    zxlogf(ERROR, "Could not create UsbAdbDevice.");
    return ZX_ERR_NO_MEMORY;
  }
  auto status = adb->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not init UsbAdbDevice - %d.", status);
    adb->DdkRelease();
    return status;
  }

  {
    // The DDK now owns this reference.
    [[maybe_unused]] auto released = adb.release();
  }
  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops{};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = UsbAdbDevice::Bind;
  return ops;
}();

}  // namespace usb_adb_function

// clang-format off
ZIRCON_DRIVER(usb_adb, usb_adb_function::driver_ops, "zircon", "0.1");
