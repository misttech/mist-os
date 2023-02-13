// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/lib/fidl/device_server.h"

#include <lib/async/cpp/task.h>
#include <lib/syslog/global.h>

#include <string_view>

#include "src/devices/lib/fidl/transaction.h"

namespace devfs_fidl {

DeviceServer::DeviceServer(DeviceInterface& device, async_dispatcher_t* dispatcher)
    : controller_(device), dispatcher_(dispatcher) {}

void DeviceServer::ConnectToController(fidl::ServerEnd<fuchsia_device::Controller> server_end) {
  Serve(server_end.TakeChannel(), &controller_);
}

void DeviceServer::ConnectToDeviceFidl(zx::channel channel) { Serve(std::move(channel), &device_); }

void DeviceServer::ServeMultiplexed(zx::channel channel, bool include_node,
                                    bool include_controller) {
  MessageDispatcher* message_dispatcher = [this, include_node, include_controller]() {
    if (include_node) {
      if (include_controller) {
        return &device_and_node_and_controller_;
      }
      return &device_and_node_;
    }
    if (include_controller) {
      return &device_and_controller_;
    }
    return &device_;
  }();
  Serve(std::move(channel), message_dispatcher);
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

DeviceServer::MessageDispatcher::Node::Node(MessageDispatcher& parent) : parent_(parent) {}

void DeviceServer::MessageDispatcher::Node::NotImplemented_(const std::string& name,
                                                            fidl::CompleterBase& completer) {
  std::string error = "Unsupported call to " + name;
  parent_.parent_.controller_.LogError(error.c_str());
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void DeviceServer::MessageDispatcher::Node::Close(CloseCompleter::Sync& completer) {
  completer.ReplySuccess();
  completer.Close(ZX_OK);
}

void DeviceServer::MessageDispatcher::Node::Query(QueryCompleter::Sync& completer) {
  const std::string_view kProtocol = fuchsia_io::wire::kNodeProtocolName;
  // TODO(https://fxbug.dev/101890): avoid the const cast.
  uint8_t* data = reinterpret_cast<uint8_t*>(const_cast<char*>(kProtocol.data()));
  completer.Reply(fidl::VectorView<uint8_t>::FromExternal(data, kProtocol.size()));
}

void DeviceServer::MessageDispatcher::Node::Clone(CloneRequestView request,
                                                  CloneCompleter::Sync& completer) {
  if (request->flags != fuchsia_io::wire::OpenFlags::kCloneSameRights) {
    std::string error =
        "Unsupported clone flags=0x" + std::to_string(static_cast<uint32_t>(request->flags));
    parent_.parent_.controller_.LogError(error.c_str());
    request->object.Close(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  parent_.parent_.ServeMultiplexed(request->object.TakeChannel(), parent_.multiplex_node_,
                                   parent_.multiplex_controller_);
}

DeviceServer::MessageDispatcher::MessageDispatcher(DeviceServer& parent, bool multiplex_node,
                                                   bool multiplex_controller)
    : parent_(parent),
      multiplex_node_(multiplex_node),
      multiplex_controller_(multiplex_controller) {}

namespace {

// TODO(fxbug.dev/85473): This target uses uses internal FIDL machinery to ad-hoc compose protocols.
// Ad-hoc composition of protocols (try to match a method against protocol A, then B, etc.) is not
// supported by FIDL. We should move to public supported APIs.
template <typename FidlProtocol>
fidl::DispatchResult TryDispatch(fidl::WireServer<FidlProtocol>* impl,
                                 fidl::IncomingHeaderAndMessage& msg, fidl::Transaction* txn) {
  return fidl::internal::WireServerDispatcher<FidlProtocol>::TryDispatch(impl, msg, nullptr, txn);
}

}  // namespace

void DeviceServer::MessageDispatcher::dispatch_message(
    fidl::IncomingHeaderAndMessage&& msg, fidl::Transaction* txn,
    fidl::internal::MessageStorageViewBase* storage_view) {
  // If the device is unbound it shouldn't receive messages so close the channel.
  if (parent_.controller_.IsUnbound()) {
    txn->Close(ZX_ERR_IO_NOT_PRESENT);
    return;
  }

  if (multiplex_node_) {
    if (TryDispatch(&node_, msg, txn) == fidl::DispatchResult::kFound) {
      return;
    }
  }

  if (multiplex_controller_) {
    if (TryDispatch(&parent_.controller_, msg, txn) == fidl::DispatchResult::kFound) {
      return;
    }
  }

  fidl_incoming_msg_t c_msg = std::move(msg).ReleaseToEncodedCMessage();
  auto ddk_txn = MakeDdkInternalTransaction(txn);
  zx_status_t status = parent_.controller_.MessageOp(&c_msg, ddk_txn.Txn());
  if (status != ZX_OK && status != ZX_ERR_ASYNC) {
    if (status == ZX_ERR_NOT_SUPPORTED) {
      constexpr char kNotSupportedErrorMessage[] =
          "Failed to send message to device: ZX_ERR_NOT_SUPPORTED\n"
          "It is possible that this message relied on deprecated FIDL multiplexing.\n"
          "For more information see https://fxbug.dev/112484.\n";
      parent_.controller_.LogError(kNotSupportedErrorMessage);
    }

    // Close the connection on any error
    txn->Close(status);
  }
}

}  // namespace devfs_fidl
