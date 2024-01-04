// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/lib/fidl/device_server.h"

#include <lib/async/cpp/task.h>
#include <lib/ddk/device.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/syslog/global.h>
#include <zircon/errors.h>

#include <string_view>

#include <ddktl/fidl.h>

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
    const auto binding =
        fidl::ServerBindingRef<TypeErasedProtocol>{fidl::internal::BindServerTypeErased(
            dispatcher_, fidl::internal::MakeAnyTransport(server_end.TakeHandle()), impl,
            fidl::internal::ThreadingPolicy::kCreateAndTeardownFromDispatcherThread,
            [this, key](fidl::internal::IncomingMessageDispatcher*, fidl::UnbindInfo,
                        fidl::internal::AnyTransport) {
              DeviceServer& self = *this;
              size_t erased = self.bindings_.erase(key);
              ZX_ASSERT_MSG(erased == 1, "erased=%zu", erased);
              if (self.bindings_.empty()) {
                if (std::optional callback = std::exchange(self.callback_, {});
                    callback.has_value()) {
                  callback.value()();
                }
              }
            })};

    auto [it, inserted] = bindings_.try_emplace(key, binding);
    ZX_ASSERT_MSG(inserted, "handle=%d", key);
  });
}

DeviceServer::MessageDispatcher::Node::Node(MessageDispatcher& parent) : parent_(parent) {}

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

void DeviceServer::MessageDispatcher::Node::GetAttr(GetAttrCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to GetAttr");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::SetAttr(fuchsia_io::wire::Node1SetAttrRequest* request,
                                                    SetAttrCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to SetAttr");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::GetFlags(GetFlagsCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to GetFlags");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::SetFlags(
    fuchsia_io::wire::Node1SetFlagsRequest* request, SetFlagsCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to SetFlags");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::QueryFilesystem(
    QueryFilesystemCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to QueryFilesystem");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::Reopen(fuchsia_io::wire::Node2ReopenRequest* request,
                                                   ReopenCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to Reopen");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::GetConnectionInfo(
    GetConnectionInfoCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to GetConnectionInfo");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::GetAttributes(
    ::fuchsia_io::wire::Node2GetAttributesRequest* request,
    GetAttributesCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to GetAttributes");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::UpdateAttributes(
    ::fuchsia_io::wire::MutableNodeAttributes* request,
    UpdateAttributesCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to UpdateAttributes");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}
void DeviceServer::MessageDispatcher::Node::Sync(SyncCompleter::Sync& completer) {
  parent_.parent_.controller_.LogError("Unsupported call to Sync");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

DeviceServer::MessageDispatcher::MessageDispatcher(DeviceServer& parent, bool multiplex_node,
                                                   bool multiplex_controller)
    : parent_(parent),
      multiplex_node_(multiplex_node),
      multiplex_controller_(multiplex_controller) {}

namespace {

// TODO(https://fxbug.dev/85473): This target uses uses internal FIDL machinery to ad-hoc compose protocols.
// Ad-hoc composition of protocols (try to match a method against protocol A, then B, etc.) is not
// supported by FIDL. We should move to public supported APIs.
template <typename FidlProtocol>
fidl::DispatchResult TryDispatch(fidl::WireServer<FidlProtocol>* impl,
                                 fidl::IncomingHeaderAndMessage& msg, fidl::Transaction* txn) {
  return fidl::internal::WireServerDispatcher<FidlProtocol>::TryDispatch(impl, msg, nullptr, txn);
}

class Transaction final : public fidl::Transaction {
 public:
  explicit Transaction(fidl::Transaction* inner) : inner_(inner) {}
  ~Transaction() final = default;

  const std::optional<std::tuple<fidl::UnbindInfo, fidl::ErrorOrigin>>& internal_error() const {
    return internal_error_;
  }

 private:
  std::unique_ptr<fidl::Transaction> TakeOwnership() final { return inner_->TakeOwnership(); }

  zx_status_t Reply(fidl::OutgoingMessage* message, fidl::WriteOptions write_options) final {
    return inner_->Reply(message, std::move(write_options));
  }

  void Close(zx_status_t epitaph) final { return inner_->Close(epitaph); }

  void InternalError(fidl::UnbindInfo error, fidl::ErrorOrigin origin) final {
    internal_error_.emplace(error, origin);
    return inner_->InternalError(error, origin);
  }

  void EnableNextDispatch() final { return inner_->EnableNextDispatch(); }

  bool DidOrGoingToUnbind() final { return inner_->DidOrGoingToUnbind(); }

  fidl::Transaction* const inner_;
  std::optional<std::tuple<fidl::UnbindInfo, fidl::ErrorOrigin>> internal_error_;
};

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

  if (!msg.ok()) {
    // Mimic fidl::internal::TryDispatch.
    txn->InternalError(fidl::UnbindInfo{msg}, fidl::ErrorOrigin::kReceive);
    return;
  }

  uint64_t ordinal = msg.header()->ordinal;

  // Use shadowing lambda captures to ensure consumed values aren't used.
  [&, txn = Transaction(txn)]() mutable {
    if (!parent_.controller_.MessageOp(std::move(msg), ddk::IntoDeviceFIDLTransaction(&txn))) {
      // The device doesn't implement zx_protocol_device::message.
      static_cast<fidl::Transaction*>(&txn)->InternalError(fidl::UnbindInfo::UnknownOrdinal(),
                                                           fidl::ErrorOrigin::kReceive);
    }
    std::optional internal_error = txn.internal_error();
    if (!internal_error.has_value()) {
      return;
    }
    auto& [error, origin] = internal_error.value();
    if (error.reason() == fidl::Reason::kUnexpectedMessage) {
      std::string message;
      message.append("Failed to send message with ordinal=");
      message.append(std::to_string(ordinal));
      message.append(" to device: ");
      message.append(error.FormatDescription());
      message.append(
          "\n"
          "It is possible that this message relied on deprecated FIDL multiplexing.\n"
          "For more information see https://fuchsia.dev/fuchsia-src/contribute/open_projects/drivers/fidl_multiplexing");
      parent_.controller_.LogError(message.c_str());
    }
  }();
}

}  // namespace devfs_fidl
