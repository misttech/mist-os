// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/overnet/usb/overnet_usb.h"

#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <iterator>
#include <optional>
#include <variant>

#include <bind/fuchsia/google/usb/cpp/bind.h>
#include <fbl/auto_lock.h>
#include <usb/request-cpp.h>

#include "fidl/fuchsia.hardware.overnet/cpp/wire_types.h"
#include "lib/async/cpp/task.h"
#include "lib/async/cpp/wait.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/internal/transport.h"

static constexpr uint8_t kOvernetMagic[] = "OVERNET USB\xff\x00\xff\x00\xff";
static constexpr size_t kOvernetMagicSize = sizeof(kOvernetMagic) - 1;

size_t OvernetUsb::UsbFunctionInterfaceGetDescriptorsSize() { return sizeof(descriptors_); }

void OvernetUsb::UsbFunctionInterfaceGetDescriptors(uint8_t* out_descriptors_buffer,
                                                    size_t descriptors_size,
                                                    size_t* out_descriptors_actual) {
  memcpy(out_descriptors_buffer, &descriptors_,
         std::min(descriptors_size, UsbFunctionInterfaceGetDescriptorsSize()));
  *out_descriptors_actual = UsbFunctionInterfaceGetDescriptorsSize();
}

// NOLINTNEXTLINE(readability-convert-member-functions-to-static)
zx_status_t OvernetUsb::UsbFunctionInterfaceControl(const usb_setup_t* setup,
                                                    const uint8_t* write_buffer, size_t write_size,
                                                    uint8_t* out_read_buffer, size_t read_size,
                                                    size_t* out_read_actual) {
  zxlogf(WARNING, "Overnet USB driver received control message");
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t OvernetUsb::UsbFunctionInterfaceSetConfigured(bool configured, usb_speed_t speed) {
  fbl::AutoLock lock(&lock_);
  if (!configured) {
    if (std::holds_alternative<Unconfigured>(state_)) {
      return ZX_OK;
    }

    function_.CancelAll(BulkInAddress());
    function_.CancelAll(BulkOutAddress());

    state_ = Unconfigured();
    callback_ = std::nullopt;

    zx_status_t status = function_.DisableEp(BulkInAddress());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to disable data in endpoint: %s", zx_status_get_string(status));
      return status;
    }
    status = function_.DisableEp(BulkOutAddress());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to disable data out endpoint: %s", zx_status_get_string(status));
      return status;
    }
    return ZX_OK;
  }

  if (!std::holds_alternative<Unconfigured>(state_)) {
    return ZX_OK;
  }

  zx_status_t status = function_.ConfigEp(&descriptors_.in_ep, nullptr);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to configure bulk in endpoint: %s", zx_status_get_string(status));
    return status;
  }
  status = function_.ConfigEp(&descriptors_.out_ep, nullptr);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to configure bulk out endpoint: %s", zx_status_get_string(status));
    return status;
  }

  state_ = Ready();

  std::optional<usb::Request<>> pending_request;
  size_t request_length = usb::Request<>::RequestSize(usb_request_size_);
  while ((pending_request = free_read_pool_.Get(request_length))) {
    pending_requests_++;
    function_.RequestQueue(pending_request->take(), &read_request_complete_);
  }

  return ZX_OK;
}

// NOLINTNEXTLINE(readability-convert-member-functions-to-static,bugprone-easily-swappable-parameters)
zx_status_t OvernetUsb::UsbFunctionInterfaceSetInterface(uint8_t interface, uint8_t alt_setting) {
  return ZX_OK;
}

std::optional<usb::Request<>> OvernetUsb::PrepareTx() {
  if (!Online()) {
    return std::nullopt;
  }

  std::optional<usb::Request<>> request;
  request = free_write_pool_.Get(usb::Request<>::RequestSize(usb_request_size_));
  if (!request) {
    zxlogf(DEBUG, "No available TX requests");
    return std::nullopt;
  }

  return request;
}

void OvernetUsb::HandleSocketReadable(async_dispatcher_t*, async::WaitBase*, zx_status_t status,
                                      const zx_packet_signal_t*) {
  if (status != ZX_OK) {
    if (status != ZX_ERR_CANCELED) {
      zxlogf(WARNING, "Unexpected error waiting on socket: %s", zx_status_get_string(status));
    }

    return;
  }

  fbl::AutoLock lock(&lock_);
  auto request = PrepareTx();

  if (!request) {
    return;
  }

  uint8_t* buf;
  status = request->Mmap(reinterpret_cast<void**>(&buf));

  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to map TX buffer: %s", zx_status_get_string(status));
    free_write_pool_.Add(*std::move(request));
    // Not clear what the right way to handle this error is. We'll just drop the socket. We could
    // retry but we'd end up retrying in a tight loop if the failure persists.
    ResetState();
    return;
  }

  size_t actual;
  std::visit(
      [this, buf, &actual, &status](auto&& state) __TA_REQUIRES(lock_) {
        state_ = std::forward<decltype(state)>(state).SendData(buf, kMtu, &actual, &status);
      },
      std::move(state_));

  if (status == ZX_OK) {
    request->request()->header.length = actual;
    pending_requests_++;
    function_.RequestQueue(request->take(), &write_request_complete_);
  } else {
    free_write_pool_.Add(*std::move(request));
  }

  std::visit(
      [this](auto& state) __TA_REQUIRES(lock_) {
        if (std::forward<decltype(state)>(state).ReadsWaiting()) {
          ProcessReadsFromSocket();
        }
      },
      state_);
}

OvernetUsb::State OvernetUsb::Running::SendData(uint8_t* data, size_t len, size_t* actual,
                                                zx_status_t* status) && {
  *status = socket_.read(0, data, len, actual);

  if (*status != ZX_OK && *status != ZX_ERR_SHOULD_WAIT) {
    if (*status != ZX_ERR_PEER_CLOSED) {
      zxlogf(ERROR, "Failed to read from socket: %s", zx_status_get_string(*status));
    }
    return Ready();
  }

  return std::move(*this);
}

void OvernetUsb::HandleSocketWritable(async_dispatcher_t*, async::WaitBase*, zx_status_t status,
                                      const zx_packet_signal_t*) {
  fbl::AutoLock lock(&lock_);

  if (status != ZX_OK) {
    if (status != ZX_ERR_CANCELED) {
      zxlogf(WARNING, "Unexpected error waiting on socket: %s", zx_status_get_string(status));
    }

    return;
  }

  std::visit([this](auto&& state)
                 __TA_REQUIRES(lock_) { state_ = std::forward<decltype(state)>(state).Writable(); },
             std::move(state_));
  std::visit(
      [this](auto& state) __TA_REQUIRES(lock_) {
        if (std::forward<decltype(state)>(state).WritesWaiting()) {
          ProcessWritesToSocket();
        }
      },
      state_);
}

OvernetUsb::State OvernetUsb::Running::Writable() && {
  if (socket_out_queue_.empty()) {
    return std::move(*this);
  }

  size_t actual;
  zx_status_t status =
      socket_.write(0, socket_out_queue_.data(), socket_out_queue_.size(), &actual);

  if (status == ZX_OK) {
    socket_out_queue_.erase(socket_out_queue_.begin(),
                            socket_out_queue_.begin() + static_cast<ssize_t>(actual));
  } else if (status != ZX_ERR_SHOULD_WAIT) {
    if (status != ZX_ERR_PEER_CLOSED) {
      zxlogf(ERROR, "Failed to read from socket: %s", zx_status_get_string(status));
    }
    return Ready();
  }

  return std::move(*this);
}

void OvernetUsb::SetCallback(fuchsia_hardware_overnet::wire::DeviceSetCallbackRequest* request,
                             SetCallbackCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  callback_ = Callback(fidl::WireSharedClient(std::move(request->callback), loop_.dispatcher(),
                                              fidl::ObserveTeardown([this]() {
                                                fbl::AutoLock lock(&lock_);
                                                callback_ = std::nullopt;
                                              })));
  HandleSocketAvailable();
  lock.release();

  completer.Reply();
}

void OvernetUsb::HandleSocketAvailable() {
  if (!callback_) {
    return;
  }

  if (!peer_socket_) {
    return;
  }

  (*callback_)(std::move(*peer_socket_));
  peer_socket_ = std::nullopt;
}

void OvernetUsb::Callback::operator()(zx::socket socket) {
  if (!fidl_.is_valid()) {
    return;
  }

  fidl_->NewLink(std::move(socket))
      .Then([](fidl::WireUnownedResult<fuchsia_hardware_overnet::Callback::NewLink>& result) {
        if (!result.ok()) {
          auto res = result.FormatDescription();
          zxlogf(ERROR, "Failed to share socket with component: %s", res.c_str());
        }
      });
}

OvernetUsb::State OvernetUsb::Unconfigured::ReceiveData(uint8_t*, size_t len,
                                                        std::optional<zx::socket>*,
                                                        OvernetUsb* owner) && {
  zxlogf(WARNING, "Dropped %zu bytes of incoming data (device not configured)", len);
  return *this;
}

OvernetUsb::State OvernetUsb::ShuttingDown::ReceiveData(uint8_t*, size_t len,
                                                        std::optional<zx::socket>*,
                                                        OvernetUsb* owner) && {
  zxlogf(WARNING, "Dropped %zu bytes of incoming data (device shutting down)", len);
  return std::move(*this);
}

OvernetUsb::State OvernetUsb::Ready::ReceiveData(uint8_t* data, size_t len,
                                                 std::optional<zx::socket>* peer_socket,
                                                 OvernetUsb* owner) && {
  if (len != kOvernetMagicSize ||
      !std::equal(kOvernetMagic, kOvernetMagic + kOvernetMagicSize, data)) {
    zxlogf(WARNING, "Dropped %zu bytes of incoming data (driver not synchronized)", len);
    return *this;
  }

  zx::socket socket;
  *peer_socket = zx::socket();

  zx_status_t status = zx::socket::create(ZX_SOCKET_STREAM, &socket, &peer_socket->value());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create socket: %s", zx_status_get_string(status));
    // There are two errors that can happen here: a kernel out of memory condition which the docs
    // say we shouldn't try to handle, and invalid arguments, which should be impossible.
    abort();
  }

  return Running(std::move(socket), owner);
}

OvernetUsb::State OvernetUsb::Running::ReceiveData(uint8_t* data, size_t len,
                                                   std::optional<zx::socket>* peer_socket,
                                                   OvernetUsb* owner) && {
  if (len == kOvernetMagicSize &&
      std::equal(kOvernetMagic, kOvernetMagic + kOvernetMagicSize, data)) {
    return Ready().ReceiveData(data, len, peer_socket, owner);
  }

  zx_status_t status;

  if (socket_out_queue_.empty()) {
    size_t actual = 0;
    while (len > 0) {
      status = socket_.write(0, data, len, &actual);

      if (status != ZX_OK) {
        break;
      }

      len -= actual;
      data += actual;
    }

    if (len == 0) {
      return std::move(*this);
    }

    if (status != ZX_ERR_SHOULD_WAIT) {
      if (status != ZX_ERR_PEER_CLOSED) {
        zxlogf(ERROR, "Failed to write to socket: %s", zx_status_get_string(status));
      }
      return Ready();
    }
  }

  if (len != 0) {
    std::copy(data, data + len, std::back_inserter(socket_out_queue_));
  }

  return std::move(*this);
}

void OvernetUsb::SendMagicReply(usb::Request<> request) {
  ssize_t result = request.CopyTo(kOvernetMagic, kOvernetMagicSize, 0);
  if (result < 0) {
    zxlogf(ERROR, "Failed to copy reply magic data: %zd", result);
    free_write_pool_.Add(std::move(request));
    ResetState();
    return;
  }

  pending_requests_++;
  request.request()->header.length = kOvernetMagicSize;

  function_.RequestQueue(request.take(), &write_request_complete_);

  auto& state = std::get<Running>(state_);

  // We have to count MagicSent as a pending request to keep ourselves from being
  // free'd out from under the task it spawns, so we increment again.
  pending_requests_++;

  state.MagicSent();
  ProcessReadsFromSocket();
  HandleSocketAvailable();
}

void OvernetUsb::Running::MagicSent() {
  socket_is_new_ = false;

  async::PostTask(owner_->loop_.dispatcher(), [this]() {
    fbl::AutoLock lock(&owner_->lock_);
    owner_->pending_requests_--;

    if (std::holds_alternative<ShuttingDown>(owner_->state_)) {
      if (owner_->pending_requests_ == 0) {
        lock.release();
        owner_->ShutdownComplete();
      }
      return;
    }

    if (std::get_if<Running>(&owner_->state_) != this) {
      return;
    }

    if (read_waiter_->is_pending()) {
      read_waiter_->Cancel();
    }
    read_waiter_->set_object(socket()->get());
    if (write_waiter_->is_pending()) {
      write_waiter_->Cancel();
    }
    write_waiter_->set_object(socket()->get());
  });
}

void OvernetUsb::ReadComplete(usb_request_t* usb_request) {
  fbl::AutoLock lock(&lock_);
  usb::Request<> request(usb_request, usb_request_size_);
  if (usb_request->response.status == ZX_ERR_IO_NOT_PRESENT) {
    pending_requests_--;
    if (std::holds_alternative<ShuttingDown>(state_)) {
      request.Release();
      if (pending_requests_ == 0) {
        lock.release();
        ShutdownComplete();
      }
      return;
    }
    free_read_pool_.Add(std::move(request));
    return;
  }

  if (usb_request->response.status == ZX_OK) {
    uint8_t* data;
    zx_status_t status = request.Mmap(reinterpret_cast<void**>(&data));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to map RX data: %s", zx_status_get_string(status));
      return;
    }

    size_t data_length = static_cast<size_t>(request.request()->response.actual);

    std::visit(
        [this, data, data_length](auto&& state) __TA_REQUIRES(lock_) {
          state_ = std::forward<decltype(state)>(state).ReceiveData(data, data_length,
                                                                    &peer_socket_, this);
        },
        std::move(state_));
    std::visit(
        [this](auto& state) __TA_REQUIRES(lock_) {
          if (std::forward<decltype(state)>(state).NewSocket()) {
            auto request = PrepareTx();

            if (request) {
              SendMagicReply(std::move(*request));
            }
          }
          if (std::forward<decltype(state)>(state).WritesWaiting()) {
            ProcessWritesToSocket();
          }
        },
        state_);
  } else if (usb_request->response.status != ZX_ERR_CANCELED) {
    zxlogf(ERROR, "Read failed: %s", zx_status_get_string(usb_request->response.status));
  }

  if (Online()) {
    function_.RequestQueue(request.take(), &read_request_complete_);
  } else {
    if (std::holds_alternative<ShuttingDown>(state_)) {
      request.Release();
      pending_requests_--;
      if (pending_requests_ == 0) {
        lock.release();
        ShutdownComplete();
      }
      return;
    }
    free_read_pool_.Add(std::move(request));
  }
}

void OvernetUsb::WriteComplete(usb_request_t* usb_request) {
  usb::Request<> request(usb_request, usb_request_size_);
  fbl::AutoLock lock(&lock_);
  pending_requests_--;
  if (std::holds_alternative<ShuttingDown>(state_)) {
    request.Release();
    if (pending_requests_ == 0) {
      lock.release();
      ShutdownComplete();
    }
    return;
  }

  if (auto state = std::get_if<Running>(&state_)) {
    if (state->NewSocket()) {
      SendMagicReply(std::move(request));
      return;
    }
  }

  free_write_pool_.Add(std::move(request));
  ProcessReadsFromSocket();
}

zx_status_t OvernetUsb::Bind() {
  descriptors_.data_interface = usb_interface_descriptor_t{
      .b_length = sizeof(usb_interface_descriptor_t),
      .b_descriptor_type = USB_DT_INTERFACE,
      .b_interface_number = 0,  // set later
      .b_alternate_setting = 0,
      .b_num_endpoints = 2,
      .b_interface_class = USB_CLASS_VENDOR,
      .b_interface_sub_class = bind_fuchsia_google_usb::BIND_USB_SUBCLASS_OVERNET,
      .b_interface_protocol = bind_fuchsia_google_usb::BIND_USB_PROTOCOL_OVERNET,
      .i_interface = 0,
  };
  descriptors_.in_ep = usb_endpoint_descriptor_t{
      .b_length = sizeof(usb_endpoint_descriptor_t),
      .b_descriptor_type = USB_DT_ENDPOINT,
      .b_endpoint_address = 0,  // set later
      .bm_attributes = USB_ENDPOINT_BULK,
      .w_max_packet_size = htole16(kMaxPacketSize),
      .b_interval = 0,
  };
  descriptors_.out_ep = usb_endpoint_descriptor_t{
      .b_length = sizeof(usb_endpoint_descriptor_t),
      .b_descriptor_type = USB_DT_ENDPOINT,
      .b_endpoint_address = 0,  // set later
      .bm_attributes = USB_ENDPOINT_BULK,
      .w_max_packet_size = htole16(kMaxPacketSize),
      .b_interval = 0,
  };

  zx_status_t status =
      function_.AllocStringDesc("Overnet USB interface", &descriptors_.data_interface.i_interface);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate string descriptor: %s", zx_status_get_string(status));
    return status;
  }

  status = function_.AllocInterface(&descriptors_.data_interface.b_interface_number);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate data interface: %s", zx_status_get_string(status));
    return status;
  }

  status = function_.AllocEp(USB_DIR_OUT, &descriptors_.out_ep.b_endpoint_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate bulk out interface: %s", zx_status_get_string(status));
    return status;
  }

  status = function_.AllocEp(USB_DIR_IN, &descriptors_.in_ep.b_endpoint_address);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate bulk in interface: %s", zx_status_get_string(status));
    return status;
  }

  usb_request_size_ = function_.GetRequestSize();

  fbl::AutoLock lock(&lock_);

  for (size_t i = 0; i < kRequestPoolSize; i++) {
    std::optional<usb::Request<>> request;
    status = usb::Request<>::Alloc(&request, kMtu, BulkOutAddress(), usb_request_size_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Allocating reads failed %d", status);
      return status;
    }
    free_read_pool_.Add(*std::move(request));
  }

  for (size_t i = 0; i < kRequestPoolSize; i++) {
    std::optional<usb::Request<>> request;
    status = usb::Request<>::Alloc(&request, kMtu, BulkInAddress(), usb_request_size_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Allocating writes failed %d", status);
      return status;
    }
    free_write_pool_.Add(*std::move(request));
  }

  status = loop_.StartThread("overnet-usb-dispatch");
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start thread: %s", zx_status_get_string(status));
    return status;
  }

  status = DdkAdd(ddk::DeviceAddArgs("overnet-usb").set_proto_id(ZX_PROTOCOL_OVERNET));
  if (status != ZX_OK) {
    return status;
  }

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);

  return ZX_OK;
}

void OvernetUsb::Shutdown(fit::function<void()> callback) {
  fbl::AutoLock lock(&lock_);
  function_.CancelAll(BulkInAddress());
  function_.CancelAll(BulkOutAddress());

  free_read_pool_.Release();
  free_write_pool_.Release();

  state_ = ShuttingDown(std::move(callback));

  if (pending_requests_ == 0) {
    lock.release();
    ShutdownComplete();
  }
}

void OvernetUsb::ShutdownComplete() {
  if (auto state = std::get_if<ShuttingDown>(&state_)) {
    state->FinishWithCallback();
  } else {
    zxlogf(ERROR, "ShutdownComplete called outside of shutdown path");
  }
}

void OvernetUsb::DdkUnbind(ddk::UnbindTxn txn) {
  fbl::AutoLock lock(&lock_);
  if (std::holds_alternative<ShuttingDown>(state_)) {
    txn.Reply();
    return;
  }
  lock.release();
  Shutdown([unbind_txn = std::move(txn)]() mutable { unbind_txn.Reply(); });
}

void OvernetUsb::DdkSuspend(ddk::SuspendTxn txn) {
  Shutdown([suspend_txn = std::move(txn)]() mutable { suspend_txn.Reply(ZX_OK, 0); });
}

void OvernetUsb::DdkRelease() { delete this; }

zx_status_t OvernetUsb::Create(void* ctx, zx_device_t* parent) {
  auto device = std::make_unique<OvernetUsb>(parent);

  device->Bind();

  // Intentionally leak this device because it's owned by the driver framework.
  [[maybe_unused]] auto unused = device.release();
  return ZX_OK;
}

namespace {
zx_driver_ops_t usb_overnet_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops{};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = OvernetUsb::Create;
  return ops;
}();
}  //  namespace

ZIRCON_DRIVER(usb_overnet, usb_overnet_driver_ops, "fuchsia", "0.1");
