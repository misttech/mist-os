// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adb-function.h"

#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/zx/vmar.h>
#include <zircon/assert.h>

#include <cstdint>
#include <mutex>
#include <optional>

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

void UsbAdbDevice::StartAdb(StartAdbRequestView request, StartAdbCompleter::Sync& completer) {
  std::lock_guard<async::sequence_checker> _(checker_);

  if (adb_binding_.has_value()) {
    zxlogf(WARNING, "ADB already connected");
    completer.ReplyError(ZX_ERR_ALREADY_BOUND);
    return;
  }

  switch (state_) {
    case State::kStoppingUsb:
      zxlogf(WARNING, "ADB connected while stopping");
      completer.ReplyError(ZX_ERR_BAD_STATE);
      return;
    case State::kConnected:
      // We're already online, so send the status change immediately.
      if (auto result =
              fidl::WireSendEvent(request->interface)->OnStatusChanged(fadb::StatusFlags::kOnline);
          !result.ok()) {
        zxlogf(ERROR, "Could not call UsbAdbImpl.OnStatusChanged.");
      }
      break;
    case State::kAwaitingUsbConnection:
      break;
  }
  zxlogf(INFO, "ADB client connected");

  adb_binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                       std::move(request->interface), this, [this](fidl::UnbindInfo info) {
                         std::lock_guard<async::sequence_checker> _(checker_);
                         zxlogf(INFO, "Device closed with reason '%s'",
                                info.FormatDescription().c_str());
                         ResetOrStopUsb();
                       });
  completer.ReplySuccess();
}

void UsbAdbDevice::StopAdb(StopAdbCompleter::Sync& completer) {
  zxlogf(INFO, "ADB client requested disconnect.");

  std::lock_guard<async::sequence_checker> _(checker_);
  stop_completers_.push_back(completer.ToAsync());
  ResetOrStopUsb();
}

void UsbAdbDevice::ResetOrStopUsb() {
  switch (state_) {
    case State::kStoppingUsb:
      zxlogf(INFO, "Stop requested, but already stopping");
      return;
    case State::kConnected:
      zxlogf(INFO, "Stopping USB");
      break;
    case State::kAwaitingUsbConnection:
      zxlogf(INFO, "Stop requested during USB startup");
      break;
  }

  // Purge any requests from internal queues.
  while (!rx_requests_.empty()) {
    rx_requests_.front().Reply(fit::error(ZX_ERR_BAD_STATE));
    rx_requests_.pop();
  }
  while (!pending_replies_.empty()) {
    bulk_out_ep_.PutRequest(
        usb::FidlRequest(std::move(pending_replies_.front().request().value())));
    pending_replies_.pop();
  }
  while (!tx_pending_reqs_.empty()) {
    CompleteTxn(tx_pending_reqs_.front().completer, ZX_ERR_CANCELED);
    tx_pending_reqs_.pop();
  }

  // Disconnect USB.
  zx_status_t status = function_.SetInterface(nullptr, nullptr);
  if (status != ZX_OK) {
    ZX_PANIC("SetInterface failed: %s", zx_status_get_string(status));
  }

  zxlogf(INFO, "state_ = State::kStoppingUsb");
  state_ = State::kStoppingUsb;

  CheckUsbStopComplete();
}

void UsbAdbDevice::SendQueued() {
  if (state_ != State::kConnected) {
    ZX_PANIC("Unexpected state: %d", state_);
  }
  while (SendQueuedOnce()) {
  }
}

// Returns true if any progress was made. Returns false if we didn't send
// anything, and therefore calling this again won't be useful until something
// changes.
bool UsbAdbDevice::SendQueuedOnce() {
  if (tx_pending_reqs_.empty()) {
    return false;
  }

  auto& current = tx_pending_reqs_.front();
  std::vector<fuchsia_hardware_usb_request::Request> requests;
  while (current.start < current.request.data().size()) {
    auto req = bulk_in_ep_.GetRequest();
    if (!req) {
      break;
    }
    req->clear_buffers();

    size_t to_copy = std::min(current.request.data().size() - current.start, kVmoDataSize);
    auto actual = req->CopyTo(0, current.request.data().data() + current.start, to_copy,
                              bulk_in_ep_.GetMapped());
    size_t actual_total = 0;
    for (size_t i = 0; i < actual.size(); i++) {
      // Fill in size of data.
      (*req)->data()->at(i).size(actual[i]);
      actual_total += actual[i];
    }
    auto status = req->CacheFlush(bulk_in_ep_.GetMapped());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Cache flush failed %d", status);
    }

    requests.emplace_back(req->take_request());
    current.start += actual_total;
  }

  if (requests.empty()) {
    return false;
  }
  auto result = bulk_in_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
  }

  if (current.start == current.request.data().size()) {
    CompleteTxn(current.completer, ZX_OK);
    tx_pending_reqs_.pop();
  }

  return true;
}

void UsbAdbDevice::ReceiveQueued() {
  if (state_ != State::kConnected) {
    ZX_PANIC("Unexpected state: %d", state_);
  }
  while (ReceiveQueuedOnce()) {
  }
}

bool UsbAdbDevice::ReceiveQueuedOnce() {
  if (pending_replies_.empty() || rx_requests_.empty()) {
    return false;
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
    auto addr = bulk_out_ep_.GetMappedAddr(req.request(), 0);
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

  req.reset_buffers(bulk_out_ep_.GetMapped());
  auto status = req.CacheFlushInvalidate(bulk_out_ep_.GetMapped());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Cache flush and invalidate failed %d", status);
  }

  std::vector<fuchsia_hardware_usb_request::Request> requests;
  requests.emplace_back(req.take_request());
  auto result = bulk_out_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
  }

  return true;
}

void UsbAdbDevice::QueueTx(QueueTxRequest& request, QueueTxCompleter::Sync& completer) {
  std::lock_guard<async::sequence_checker> _(checker_);
  size_t length = request.data().size();
  if (length == 0) {
    zxlogf(INFO, "Invalid argument - Length = 0");
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  switch (state_) {
    case State::kStoppingUsb:
      // Return early during shutdown.
      completer.Reply(fit::error(ZX_ERR_BAD_STATE));
      return;
    case State::kConnected:
    case State::kAwaitingUsbConnection:
      tx_pending_reqs_.emplace(
          txn_req_t{.request = std::move(request), .start = 0, .completer = completer.ToAsync()});
      SendQueued();
  }
}

void UsbAdbDevice::Receive(ReceiveCompleter::Sync& completer) {
  std::lock_guard<async::sequence_checker> _(checker_);
  switch (state_) {
    case State::kStoppingUsb:
      // Return early during shutdown.
      completer.Reply(fit::error(ZX_ERR_BAD_STATE));
      return;
    case State::kAwaitingUsbConnection:
      rx_requests_.emplace(completer.ToAsync());
      break;
    case State::kConnected:
      rx_requests_.emplace(completer.ToAsync());
      ReceiveQueued();
      break;
  }
}

void UsbAdbDevice::RxComplete(fendpoint::Completion completion) {
  // This should always be true because when we registered VMOs, we only registered one per request.
  ZX_ASSERT(completion.request()->data()->size() == 1);

  switch (state_) {
    case State::kAwaitingUsbConnection:
      ZX_PANIC("Completion arrived before we sent any requests?");
    case State::kStoppingUsb:
      bulk_out_ep_.PutRequest(usb::FidlRequest(std::move(completion.request().value())));
      CheckUsbStopComplete();
      return;
    case State::kConnected:
      pending_replies_.push(std::move(completion));
      ReceiveQueued();
      return;
  }
}

void UsbAdbDevice::TxComplete(fendpoint::Completion completion) {
  switch (state_) {
    case State::kAwaitingUsbConnection:
      ZX_PANIC("Completion arrived before we sent any requests?");
    case State::kStoppingUsb:
      bulk_in_ep_.PutRequest(usb::FidlRequest(std::move(completion.request().value())));
      CheckUsbStopComplete();
      return;
    case State::kConnected:
      bulk_in_ep_.PutRequest(usb::FidlRequest(std::move(completion.request().value())));

      // Do not queue requests if status is ZX_ERR_IO_NOT_PRESENT, as the underlying connection
      // could be disconnected or USB_RESET is being processed. Calling adb_send_locked in such
      // scenario will deadlock and crash the driver (see https://fxbug.dev/42174506).
      if (*completion.status() != ZX_ERR_IO_NOT_PRESENT) {
        SendQueued();
      }
      return;
  }
}

void UsbAdbDevice::RxCompleteCallback(fendpoint::Completion completion) {
  async::PostTask(dispatcher(), [this, completion = std::move(completion)]() mutable {
    std::lock_guard<async::sequence_checker> _(checker_);
    RxComplete(std::move(completion));
  });
}

void UsbAdbDevice::TxCompleteCallback(fendpoint::Completion completion) {
  async::PostTask(dispatcher(), [this, completion = std::move(completion)]() mutable {
    std::lock_guard<async::sequence_checker> _(checker_);
    TxComplete(std::move(completion));
  });
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
void UsbAdbDevice::EnableEndpoints() {
  switch (state_) {
    case State::kConnected:
      zxlogf(INFO, "USB endpoints already enabled");
      return;
    case State::kStoppingUsb:
      zxlogf(ERROR, "This is unexpected: UsbFunctionInterface is disconnected while stopping");
      return;
    case State::kAwaitingUsbConnection:
      zxlogf(INFO, "Enabling USB endpoints");
      break;
  }

  zx_status_t status = function_.ConfigEp(&descriptors_.bulk_out_ep, nullptr);
  if (status != ZX_OK) {
    ZX_PANIC("Failed to Config BULK OUT ep - %d.", status);
  }

  status = function_.ConfigEp(&descriptors_.bulk_in_ep, nullptr);
  if (status != ZX_OK) {
    ZX_PANIC("Failed to Config BULK IN ep - %d.", status);
  }

  // queue RX requests
  std::vector<fuchsia_hardware_usb_request::Request> requests;
  while (auto req = bulk_out_ep_.GetRequest()) {
    req->reset_buffers(bulk_out_ep_.GetMapped());
    auto status = req->CacheFlushInvalidate(bulk_out_ep_.GetMapped());
    if (status != ZX_OK) {
      ZX_PANIC("Cache flush and invalidate failed %d", status);
    }

    requests.emplace_back(req->take_request());
  }
  auto result = bulk_out_ep_->QueueRequests(std::move(requests));
  if (result.is_error()) {
    ZX_PANIC("Failed to QueueRequests %s", result.error_value().FormatDescription().c_str());
  }

  if (adb_binding_.has_value()) {
    auto result = fidl::WireSendEvent(*adb_binding_)->OnStatusChanged(fadb::StatusFlags::kOnline);
    if (!result.ok()) {
      zxlogf(ERROR, "Could not call UsbAdbImpl.OnStatusChanged.");
    }
  }

  zxlogf(INFO, "state_ = State::kConnected");
  state_ = State::kConnected;
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  zxlogf(INFO, "configured? - %d  speed - %d.", configured, speed);
  async::PostTask(dispatcher(), [this, configured]() {
    std::lock_guard<async::sequence_checker> _(checker_);
    if (configured) {
      EnableEndpoints();
    } else {
      switch (state_) {
        case State::kAwaitingUsbConnection:
          // It's normal to receive SetConfigured(false) while the connection is
          // starting up - ignore it.
          break;
        case State::kConnected:
          ResetOrStopUsb();
          break;
        case State::kStoppingUsb:
          zxlogf(
              WARNING,
              "Received SetConfigured(false) while stopping. This is unexpected, but probably fine.");
          break;
      }
    }
  });
  return ZX_OK;
}

zx_status_t UsbAdbDevice::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  zxlogf(INFO, "interface - %d alt_setting - %d.", interface, alt_setting);

  // We don't support any alt_settings - just validate that the main setting has
  // been requested, and then don't do anything.
  if (interface != descriptors_.adb_intf.b_interface_number || alt_setting > 0) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

void UsbAdbDevice::CheckUsbStopComplete() {
  if (state_ != State::kStoppingUsb) {
    ZX_PANIC("Unexpected state: %d", state_);
  }

  if (!bulk_in_ep_.RequestsFull() || !bulk_out_ep_.RequestsFull()) {
    // Still waiting for outstanding USB requests to return.
    return;
  }

  zxlogf(INFO, "All USB requests complete. Completing USB stop.");

  if (adb_binding_.has_value()) {
    auto result = fidl::WireSendEvent(*adb_binding_)->OnStatusChanged(fadb::StatusFlags(0));
    if (!result.ok()) {
      zxlogf(ERROR, "Could not call UsbAdbImpl.OnStatusChanged.");
    }
  }

  adb_binding_.reset();

  while (!stop_completers_.empty()) {
    stop_completers_.back().Reply(zx::ok());
    stop_completers_.pop_back();
  }

  // Is this a proper shutdown, or a restart of USB?
  if (shutdown_callback_.has_value()) {
    zxlogf(INFO, "Shutting down driver.");
    shutdown_callback_.value()(zx::ok());
    shutdown_callback_.reset();
  } else {
    zxlogf(INFO, "Restarting USB connection.");
    StartUsb();
  }
}

void UsbAdbDevice::PrepareStop(fdf::PrepareStopCompleter completer) {
  std::lock_guard<async::sequence_checker> _(checker_);
  shutdown_callback_.emplace(std::move(completer));
  ResetOrStopUsb();
}

zx_status_t UsbAdbDevice::InitEndpoint(
    fidl::ClientEnd<fuchsia_hardware_usb_function::UsbFunction>& client, uint8_t direction,
    uint8_t* ep_addrs, usb::EndpointClient<UsbAdbDevice>& ep, uint32_t req_count) {
  auto status = function_.AllocEp(direction, ep_addrs);
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb_function_alloc_ep failed - %d.", status);
    return status;
  }

  status = ep.Init(*ep_addrs, client, usb_dispatcher_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to init UsbEndpoint %d", status);
    return status;
  }

  // TODO(127854): When we support active pinning of VMOs, adb may want to use VMOs that are not
  // perpetually pinned.
  auto actual =
      ep.AddRequests(req_count, kVmoDataSize, fuchsia_hardware_usb_request::Buffer::Tag::kVmoId);
  if (actual != req_count) {
    zxlogf(ERROR, "Wanted %u requests, only got %zu requests", req_count, actual);
  }
  return actual == 0 ? ZX_ERR_INTERNAL : ZX_OK;
}

zx::result<> UsbAdbDevice::Start() {
  std::lock_guard<async::sequence_checker> _(checker_);
  zx::result<ddk::UsbFunctionProtocolClient> function =
      compat::ConnectBanjo<ddk::UsbFunctionProtocolClient>(incoming());
  if (function.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect function", KV("status", function.status_string()));
    return function.take_error();
  }
  function_ = *function;

  auto client = incoming()->Connect<fuchsia_hardware_usb_function::UsbFunctionService::Device>();

  if (client.is_error()) {
    zxlogf(ERROR, "Failed to connect fidl protocol");
    return client.take_error();
  }

  auto status = function_.AllocInterface(&descriptors_.adb_intf.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "usb_function_alloc_interface failed - %d.", status);
    return zx::error(status);
  }

  status = InitEndpoint(*client, USB_DIR_OUT, &descriptors_.bulk_out_ep.b_endpoint_address,
                        bulk_out_ep_, kBulkRxCount);
  if (status != ZX_OK) {
    zxlogf(ERROR, "InitEndpoint failed - %d.", status);
    return zx::error(status);
  }
  status = InitEndpoint(*client, USB_DIR_IN, &descriptors_.bulk_in_ep.b_endpoint_address,
                        bulk_in_ep_, kBulkTxCount);
  if (status != ZX_OK) {
    zxlogf(ERROR, "InitEndpoint failed - %d.", status);
    return zx::error(status);
  }
  auto serve_result = outgoing()->AddService<fadb::Service>(fadb::Service::InstanceHandler({
      .adb = device_bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                            fidl::kIgnoreBindingClosure),
  }));
  if (serve_result.is_error()) {
    zxlogf(ERROR, "Failed to add Device service %s", serve_result.status_string());
    return serve_result.take_error();
  }

  StartUsb();
  return zx::ok();
}

void UsbAdbDevice::StartUsb() {
  zx_status_t status = function_.SetInterface(this, &usb_function_interface_protocol_ops_);
  if (status != ZX_OK) {
    ZX_PANIC("SetInterface failed %s", zx_status_get_string(status));
  }

  zxlogf(INFO, "state_ = State::kAwaitingUsbConnection");
  state_ = State::kAwaitingUsbConnection;
}

}  // namespace usb_adb_function

FUCHSIA_DRIVER_EXPORT(usb_adb_function::UsbAdbDevice);
