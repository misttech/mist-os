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

#include "fuchsia/hardware/bluetooth/cpp/fidl.h"
#include "lib/fidl/cpp/interface_request.h"
#include "lib/fpromise/result.h"
#include "src/connectivity/bluetooth/core/bt-host/fidl/fake_hci_server.h"

namespace bt::fidl::testing {

class FakeVendorServer final : public fuchsia::hardware::bluetooth::testing::Vendor_TestBase {
 public:
  FakeVendorServer(::fidl::InterfaceRequest<fuchsia::hardware::bluetooth::Vendor> request,
                   async_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher) {
    binding_.Bind(std::move(request));
  }

  void Unbind() { binding_.Unbind(); }

  fidl::testing::FakeHciServer* hci_server() { return &fake_hci_server_.value(); }

  void set_open_hci_error(bool val) { open_hci_error_ = val; }

 private:
  void GetFeatures(GetFeaturesCallback callback) override {
    auto features = fuchsia::hardware::bluetooth::BtVendorFeatures::SET_ACL_PRIORITY_COMMAND;
    callback(fpromise::ok(features));
  }

  void EncodeCommand(::fuchsia::hardware::bluetooth::BtVendorCommand command,
                     EncodeCommandCallback callback) override {
    ::fidl::Encoder encoder;
    size_t offset = encoder.Alloc(sizeof(fidl_string_t));
    command.Encode(&encoder, offset);
    std::vector<uint8_t> tmp{static_cast<unsigned char>(WhichSetAclPriority(
        command.set_acl_priority().priority, command.set_acl_priority().direction))};
    callback(fpromise::ok(tmp));
  }

  void OpenHci(OpenHciCallback callback) override {
    if (open_hci_error_) {
      callback(fpromise::error(ZX_ERR_INTERNAL));
      return;
    }

    ::fidl::InterfaceHandle<::fuchsia::hardware::bluetooth::Hci> hci;
    fake_hci_server_.emplace(hci.NewRequest(), dispatcher_);

    callback(fpromise::ok(std::move(hci)));
  }

  void NotImplemented_(const std::string& name) override { FAIL() << name << " not implemented"; }

  void InitializeWait(async::WaitBase& wait, zx::channel& channel) {
    BT_ASSERT(channel.is_valid());
    wait.Cancel();
    wait.set_object(channel.get());
    wait.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
    BT_ASSERT(wait.Begin(dispatcher_) == ZX_OK);
  }

  uint8_t WhichSetAclPriority(fuchsia::hardware::bluetooth::BtVendorAclPriority priority,
                              fuchsia::hardware::bluetooth::BtVendorAclDirection direction) {
    if (priority == fuchsia::hardware::bluetooth::BtVendorAclPriority::HIGH) {
      if (direction == fuchsia::hardware::bluetooth::BtVendorAclDirection::SOURCE) {
        return static_cast<uint8_t>(pw::bluetooth::AclPriority::kSource);
      }
      return static_cast<uint8_t>(pw::bluetooth::AclPriority::kSink);
    }
    return static_cast<uint8_t>(pw::bluetooth::AclPriority::kNormal);
  }

  // Flag for testing. |OpenHci()| returns an error when set to true
  bool open_hci_error_ = false;

  std::optional<fidl::testing::FakeHciServer> fake_hci_server_;

  ::fidl::Binding<fuchsia::hardware::bluetooth::Vendor> binding_{this};

  async_dispatcher_t* dispatcher_;
};

}  // namespace bt::fidl::testing

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_VENDOR_SERVER_H_
