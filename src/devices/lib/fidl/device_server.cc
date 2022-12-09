// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/lib/fidl/device_server.h"

#include <lib/async/cpp/task.h>
#include <lib/syslog/global.h>

#include <string_view>

#include "src/devices/lib/fidl/transaction.h"

namespace {
constexpr char kLogTag[] = "devfs";
}  // namespace

DeviceServer::DeviceServer(DeviceInterface& device, async_dispatcher_t* dispatcher)
    : dev_(device), dispatcher_(dispatcher) {}

void DeviceServer::ConnectToController(fidl::ServerEnd<fuchsia_device::Controller> server_end) {
  Serve(server_end.TakeChannel(), &dev_);
}

void DeviceServer::ConnectToDeviceFidl(zx::channel channel) { Serve(std::move(channel), &device_); }

void DeviceServer::ServeMultiplexed(zx::channel channel) {
  Serve(std::move(channel), &multiplexed_);
}

void DeviceServer::CloseAllConnections(fit::callback<void()> callback) {
  async::PostTask(dispatcher_, [this, callback = std::move(callback)]() mutable {
    if (bindings_.empty()) {
      if (callback != nullptr) {
        callback();
      }
      return;
    }
    if (callback != nullptr) {
      ZX_ASSERT(!std::exchange(callback_, std::move(callback)).has_value());
    }
    for (auto& [handle, binding] : bindings_) {
      binding.Unbind();
    }
  });
}

void DeviceServer::Serve(zx::channel channel, fidl::internal::IncomingMessageDispatcher* impl) {
  fidl::ServerEnd<TypeErasedProtocol> server_end(std::move(channel));
  async::PostTask(dispatcher_, [this, server_end = std::move(server_end), impl]() mutable {
    const zx_handle_t key = server_end.channel().get();
    const fidl::ServerBindingRef binding = fidl::internal::BindServerTypeErased<TypeErasedProtocol>(
        dispatcher_, std::move(server_end), impl,
        fidl::internal::ThreadingPolicy::kCreateAndTeardownFromDispatcherThread,
        [this, key](fidl::internal::IncomingMessageDispatcher*, fidl::UnbindInfo,
                    fidl::internal::AnyTransport) {
          DeviceServer& self = *this;
          size_t erased = self.bindings_.erase(key);
          ZX_ASSERT_MSG(erased == 1, "erased=%zu", erased);
          if (self.bindings_.empty()) {
            if (std::optional callback = std::exchange(self.callback_, {}); callback.has_value()) {
              callback.value()();
            }
          }
        });

    auto [it, inserted] = bindings_.try_emplace(key, binding);
    ZX_ASSERT_MSG(inserted, "handle=%d", key);
  });
}

DeviceServer::Node::Node(DeviceServer& parent) : parent_(parent) {}

void DeviceServer::Node::NotImplemented_(const std::string& name, fidl::CompleterBase& completer) {
  zx::result topo_path = parent_.dev_.GetTopologicalPath();
  FX_LOGF(ERROR, kLogTag, "%s: unsupported call to %s", name.c_str(),
          topo_path.is_ok() ? topo_path.value().c_str() : topo_path.status_string());
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void DeviceServer::Node::Close(CloseCompleter::Sync& completer) {
  completer.ReplySuccess();
  completer.Close(ZX_OK);
}

void DeviceServer::Node::Query(QueryCompleter::Sync& completer) {
  const std::string_view kProtocol = fuchsia_io::wire::kNodeProtocolName;
  uint8_t* data = reinterpret_cast<uint8_t*>(const_cast<char*>(kProtocol.data()));
  completer.Reply(fidl::VectorView<uint8_t>::FromExternal(data, kProtocol.size()));
}

void DeviceServer::Node::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  if (request->flags != fuchsia_io::wire::OpenFlags::kCloneSameRights) {
    zx::result topo_path = parent_.dev_.GetTopologicalPath();
    FX_LOGF(ERROR, kLogTag, "%s: unsupported clone flags=0x%x",
            topo_path.is_ok() ? topo_path.value().c_str() : topo_path.status_string(),
            static_cast<uint32_t>(request->flags));
    request->object.Close(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  parent_.ServeMultiplexed(request->object.TakeChannel());
}

DeviceServer::MessageDispatcher::MessageDispatcher(DeviceServer& parent, bool multiplexing)
    : parent_(parent), multiplexing_(multiplexing) {}

void DeviceServer::MessageDispatcher::dispatch_message(
    fidl::IncomingHeaderAndMessage&& msg, fidl::Transaction* txn,
    fidl::internal::MessageStorageViewBase* storage_view) {
  // If the device is unbound it shouldn't receive messages so close the channel.
  if (parent_.dev_.IsUnbound()) {
    txn->Close(ZX_ERR_IO_NOT_PRESENT);
    return;
  }

  if (multiplexing_) {
    if (fidl::WireTryDispatch(&parent_.node_, msg, txn) == fidl::DispatchResult::kFound) {
      return;
    }
    if (fidl::WireTryDispatch(&parent_.dev_, msg, txn) == fidl::DispatchResult::kFound) {
      return;
    }
  }

  fidl_incoming_msg_t c_msg = std::move(msg).ReleaseToEncodedCMessage();
  auto ddk_txn = MakeDdkInternalTransaction(txn);
  zx_status_t status = parent_.dev_.MessageOp(&c_msg, ddk_txn.Txn());
  if (status != ZX_OK && status != ZX_ERR_ASYNC) {
    // Close the connection on any error
    txn->Close(status);
  }
}
