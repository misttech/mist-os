// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_VENDOR_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_VENDOR_SERVER_H_

#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/channel.h>

#include <cstdint>
#include <string>

#include "lib/fidl/cpp/interface_request.h"
#include "lib/fpromise/result.h"
#include "src/connectivity/bluetooth/core/bt-host/fidl/fake_hci_server.h"

namespace bt::fidl::testing {

class FakeVendorServer final : public ::fidl::Server<fuchsia_hardware_bluetooth::Vendor> {
 public:
  FakeVendorServer(::fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> server_end,
                   async_dispatcher_t* dispatcher)
      : binding_(::fidl::BindServer(dispatcher, std::move(server_end), this)),
        dispatcher_(dispatcher) {}

  void Unbind() { binding_.Unbind(); }

  fidl::testing::FakeHciServer* hci_server() { return &fake_hci_server_.value(); }

  void set_open_hci_error(bool val) { open_hci_error_ = val; }

 private:
  void GetFeatures(GetFeaturesCompleter::Sync& completer) override {
    auto features = fuchsia_hardware_bluetooth::BtVendorFeatures::kSetAclPriorityCommand;
    completer.Reply(features);
  }

  void EncodeCommand(EncodeCommandRequest& request,
                     EncodeCommandCompleter::Sync& completer) override {
    std::vector<uint8_t> tmp{static_cast<unsigned char>(
        WhichSetAclPriority(request.command().set_acl_priority()->priority(),
                            request.command().set_acl_priority()->direction()))};
    completer.Reply(fit::success(tmp));
  }

  void OpenHci(OpenHciCompleter::Sync& completer) override {
    if (open_hci_error_) {
      completer.Reply(fit::error(ZX_ERR_INTERNAL));
      return;
    }

    auto [hci_client_end, hci_server_end] =
        ::fidl::Endpoints<fuchsia_hardware_bluetooth::Hci>::Create();

    fake_hci_server_.emplace(std::move(hci_server_end), dispatcher_);
    completer.Reply(fit::success(std::move(hci_client_end)));
  }

  void handle_unknown_method(
      ::fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      ::fidl::UnknownMethodCompleter::Sync& completer) override {
    // Not implemented
  }

  void InitializeWait(async::WaitBase& wait, zx::channel& channel) {
    BT_ASSERT(channel.is_valid());
    wait.Cancel();
    wait.set_object(channel.get());
    wait.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
    BT_ASSERT(wait.Begin(dispatcher_) == ZX_OK);
  }

  uint8_t WhichSetAclPriority(fuchsia_hardware_bluetooth::BtVendorAclPriority priority,
                              fuchsia_hardware_bluetooth::BtVendorAclDirection direction) {
    if (priority == fuchsia_hardware_bluetooth::BtVendorAclPriority::kHigh) {
      if (direction == fuchsia_hardware_bluetooth::BtVendorAclDirection::kSource) {
        return static_cast<uint8_t>(pw::bluetooth::AclPriority::kSource);
      }
      return static_cast<uint8_t>(pw::bluetooth::AclPriority::kSink);
    }
    return static_cast<uint8_t>(pw::bluetooth::AclPriority::kNormal);
  }

  // Flag for testing. |OpenHci()| returns an error when set to true
  bool open_hci_error_ = false;

  std::optional<fidl::testing::FakeHciServer> fake_hci_server_;

  ::fidl::ServerBindingRef<fuchsia_hardware_bluetooth::Vendor> binding_;

  async_dispatcher_t* dispatcher_;
};

}  // namespace bt::fidl::testing

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_VENDOR_SERVER_H_
