// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_FIDL_DEVICE_SERVER_H_
#define SRC_DEVICES_LIB_FIDL_DEVICE_SERVER_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire_test_base.h>
#include <lib/ddk/device.h>
#include <lib/fidl/cpp/wire/internal/server_details.h>

namespace devfs_fidl {

class DeviceInterface : public fidl::WireServer<fuchsia_device::Controller> {
 public:
  // Have the `DeviceInterface` log this error. This lets the `DeviceInterface`
  // add extra context around the error, and supports logging in both the DFv1
  // and DFv2 environments.
  virtual void LogError(const char* error) = 0;

  virtual bool IsUnbound() = 0;
  virtual zx_status_t MessageOp(fidl_incoming_msg_t* msg, fidl_txn_t* txn) = 0;
};

class DeviceServer {
 public:
  // `device` must outlive `DeviceServer`.
  DeviceServer(DeviceInterface& device, async_dispatcher_t* dispatcher);

  void ConnectToController(fidl::ServerEnd<fuchsia_device::Controller> server_end);
  void ConnectToDeviceFidl(zx::channel channel);
  void ServeMultiplexed(zx::channel channel, bool include_node, bool include_controller);

  // Asynchronously close all connections and call `callback` when all connections have completed
  // their teardown. Must not be called with `callback != nullptr` while a previous `callback` is
  // pending.
  void CloseAllConnections(fit::callback<void()> callback);

 private:
  void Serve(zx::channel channel, fidl::internal::IncomingMessageDispatcher* impl);

  class MessageDispatcher : public fidl::internal::IncomingMessageDispatcher {
   public:
    MessageDispatcher(DeviceServer& parent, bool multiplex_node, bool multiplex_controller);

   private:
    void dispatch_message(fidl::IncomingHeaderAndMessage&& msg, fidl::Transaction* txn,
                          fidl::internal::MessageStorageViewBase* storage_view) override;

    class Node : public fidl::testing::WireTestBase<fuchsia_io::Node> {
     public:
      explicit Node(MessageDispatcher& parent);

     private:
      void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override;
      void Close(CloseCompleter::Sync& completer) override;
      void Query(QueryCompleter::Sync& completer) override;
      void Clone(CloneRequestView request, CloneCompleter::Sync& completer) override;

      MessageDispatcher& parent_;
    };

    Node node_{*this};
    DeviceServer& parent_;
    const bool multiplex_node_;
    const bool multiplex_controller_;
  };

  DeviceInterface& controller_;
  async_dispatcher_t* const dispatcher_;

  MessageDispatcher device_{*this, false, false};
  MessageDispatcher device_and_node_{*this, true, false};
  MessageDispatcher device_and_controller_{*this, false, true};
  MessageDispatcher device_and_node_and_controller_{*this, true, true};

  // Note: this protocol is a lie with respect to the bindings below; some of them speak the stated
  // protocol, some speak the device-specific protocol, others speak a mix of protocols multiplexed
  // at run-time. The protocol is not particularly important as this collection is only used to
  // synchronize the destruction of all its bindings in response to driver lifecycle events.
  using TypeErasedProtocol = fuchsia_device::Controller;

  std::unordered_map<zx_handle_t, fidl::ServerBindingRef<TypeErasedProtocol>> bindings_;
  // Set via `CloseAllConnections` and called when all bindings have completed their teardown.
  std::optional<fit::callback<void()>> callback_;
};

}  // namespace devfs_fidl

#endif  // SRC_DEVICES_LIB_FIDL_DEVICE_SERVER_H_
