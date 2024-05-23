// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/overnet/usb/overnet_usb.h"

#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <iterator>
#include <optional>
#include <variant>

#include <fbl/auto_lock.h>
#include <usb/request-cpp.h>

#include "fidl/fuchsia.hardware.overnet/cpp/wire_types.h"
#include "lib/async/cpp/task.h"
#include "lib/async/cpp/wait.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/internal/transport.h"

static constexpr uint8_t kOvernetMagic[] = "OVERNET USB\xff\x00\xff\x00\xff";
static constexpr size_t kOvernetMagicSize = sizeof(kOvernetMagic) - 1;

zx::result<> OvernetUsb::Start() {
  zx::result<ddk::UsbFunctionProtocolClient> function =
      compat::ConnectBanjo<ddk::UsbFunctionProtocolClient>(incoming());

  if (function.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect function", KV("status", function.status_string()));
    return function.take_error();
  }
  function_ = *function;

  zx_status_t status =
      function_.AllocStringDesc("Overnet USB interface", &descriptors_.data_interface.i_interface);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate string descriptor",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = function_.AllocInterface(&descriptors_.data_interface.b_interface_number);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate data interface",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = function_.AllocEp(USB_DIR_OUT, &descriptors_.out_ep.b_endpoint_address);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate bulk out interface",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  status = function_.AllocEp(USB_DIR_IN, &descriptors_.in_ep.b_endpoint_address);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to allocate bulk in interface",
             KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  usb_request_size_ = function_.GetRequestSize();

  fbl::AutoLock lock(&lock_);

  for (size_t i = 0; i < kRequestPoolSize; i++) {
    std::optional<usb::Request<>> request;
    status = usb::Request<>::Alloc(&request, kMtu, BulkOutAddress(), usb_request_size_);
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Allocating reads failed", KV("status", status));
      return zx::error(status);
    }
    free_read_pool_.Add(*std::move(request));
  }

  for (size_t i = 0; i < kRequestPoolSize; i++) {
    std::optional<usb::Request<>> request;
    status = usb::Request<>::Alloc(&request, kMtu, BulkInAddress(), usb_request_size_);
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Allocating writes failed", KV("status", status));
      return zx::error(status);
    }
    free_write_pool_.Add(*std::move(request));
  }

  function_.SetInterface(this, &usb_function_interface_protocol_ops_);

  auto connector = devfs_connector_.Bind(dispatcher_);

  if (connector.is_error()) {
    FDF_SLOG(ERROR, "devfs_connector_.Bind() failed", KV("error", connector.status_string()));
    return connector.take_error();
  }

  fdf::DevfsAddArgs devfs;
  devfs.connector(std::move(connector.value()));
  devfs.class_name("overnet-usb");

  fdf::NodeAddArgs args;
  args.devfs_args(std::move(devfs));
  args.name("overnet_usb");

  auto controller_eps = fidl::CreateEndpoints<fdf::NodeController>();
  if (controller_eps.is_error()) {
    FDF_SLOG(ERROR, "Could not create node controller endpoints",
             KV("error", controller_eps.status_string()));
    return controller_eps.take_error();
  }
  node_controller_.Bind(std::move(controller_eps->client));

  auto node_eps = fidl::CreateEndpoints<fdf::Node>();
  if (node_eps.is_error()) {
    FDF_SLOG(ERROR, "Could not create node endpoints", KV("error", node_eps.status_string()));
    return node_eps.take_error();
  }
  node_.Bind(std::move(node_eps->client));

  auto result = fidl::Call(node())->AddChild({{
      .args = std::move(args),
      .controller = std::move(controller_eps->server),
      .node = std::move(node_eps->server),
  }});

  if (result.is_error()) {
    FDF_SLOG(ERROR, "Could not add child node",
             KV("error", result.error_value().FormatDescription().c_str()));
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok();
}

void OvernetUsb::FidlConnect(fidl::ServerEnd<fuchsia_hardware_overnet::Device> request) {
  device_binding_group_.AddBinding(dispatcher_, std::move(request), this,
                                   fidl::kIgnoreBindingClosure);
}

void OvernetUsb::PrepareStop(fdf::PrepareStopCompleter completer) {
  Shutdown([completer = std::move(completer)]() mutable { completer(zx::ok()); });
}

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
  FDF_SLOG(WARNING, "Overnet USB driver received control message");
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
      FDF_SLOG(ERROR, "Failed to disable data in endpoint",
               KV("status", zx_status_get_string(status)));
      return status;
    }
    status = function_.DisableEp(BulkOutAddress());
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Failed to disable data out endpoint",
               KV("status", zx_status_get_string(status)));
      return status;
    }
    return ZX_OK;
  }

  if (!std::holds_alternative<Unconfigured>(state_)) {
    return ZX_OK;
  }

  zx_status_t status = function_.ConfigEp(&descriptors_.in_ep, nullptr);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to configure bulk in endpoint",
             KV("status", zx_status_get_string(status)));
    return status;
  }
  status = function_.ConfigEp(&descriptors_.out_ep, nullptr);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to configure bulk out endpoint",
             KV("status", zx_status_get_string(status)));
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
    FDF_SLOG(DEBUG, "No available TX requests");
    return std::nullopt;
  }

  return request;
}

void OvernetUsb::HandleSocketReadable(async_dispatcher_t*, async::WaitBase*, zx_status_t status,
                                      const zx_packet_signal_t*) {
  if (status != ZX_OK) {
    if (status != ZX_ERR_CANCELED) {
      FDF_SLOG(WARNING, "Unexpected error waiting on socket",
               KV("status", zx_status_get_string(status)));
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
    FDF_SLOG(ERROR, "Failed to map TX buffer", KV("status", zx_status_get_string(status)));
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
      FDF_SLOG(ERROR, "Failed to read from socket", KV("status", zx_status_get_string(*status)));
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
      FDF_SLOG(WARNING, "Unexpected error waiting on socket",
               KV("status", zx_status_get_string(status)));
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
      FDF_SLOG(ERROR, "Failed to read from socket", KV("status", zx_status_get_string(status)));
    }
    return Ready();
  }

  return std::move(*this);
}

void OvernetUsb::SetCallback(fuchsia_hardware_overnet::wire::DeviceSetCallbackRequest* request,
                             SetCallbackCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  callback_ = Callback(fidl::WireSharedClient(std::move(request->callback), dispatcher_,
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
          FDF_SLOG(ERROR, "Failed to share socket with component", KV("status", res));
        }
      });
}

OvernetUsb::State OvernetUsb::Unconfigured::ReceiveData(uint8_t*, size_t len,
                                                        std::optional<zx::socket>*,
                                                        OvernetUsb* owner) && {
  FDF_SLOG(WARNING, "Dropped incoming data (device not configured)", KV("bytes", len));
  return *this;
}

OvernetUsb::State OvernetUsb::ShuttingDown::ReceiveData(uint8_t*, size_t len,
                                                        std::optional<zx::socket>*,
                                                        OvernetUsb* owner) && {
  FDF_SLOG(WARNING, "Dropped incoming data (device shutting down)", KV("bytes", len));
  return std::move(*this);
}

OvernetUsb::State OvernetUsb::Ready::ReceiveData(uint8_t* data, size_t len,
                                                 std::optional<zx::socket>* peer_socket,
                                                 OvernetUsb* owner) && {
  if (len != kOvernetMagicSize ||
      !std::equal(kOvernetMagic, kOvernetMagic + kOvernetMagicSize, data)) {
    FDF_SLOG(WARNING, "Dropped incoming data (driver not synchronized)", KV("bytes", len));
    return *this;
  }

  zx::socket socket;
  *peer_socket = zx::socket();

  zx_status_t status = zx::socket::create(ZX_SOCKET_STREAM, &socket, &peer_socket->value());
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to create socket", KV("status", zx_status_get_string(status)));
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
        FDF_SLOG(ERROR, "Failed to write to socket", KV("status", zx_status_get_string(status)));
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
    FDF_SLOG(ERROR, "Failed to copy reply magic data", KV("status", result));
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

  async::PostTask(owner_->dispatcher_, [this]() {
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
      FDF_SLOG(ERROR, "Failed to map RX data", KV("status", zx_status_get_string(status)));
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
    FDF_SLOG(ERROR, "Read failed",
             KV("status", zx_status_get_string(usb_request->response.status)));
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
    FDF_SLOG(ERROR, "ShutdownComplete called outside of shutdown path");
  }
}

FUCHSIA_DRIVER_EXPORT(OvernetUsb);
