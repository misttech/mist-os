// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_HCI_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_HCI_SERVER_H_

#include <fuchsia/hardware/bluetooth/cpp/fidl_test_base.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/zx/channel.h>

#include <string>

#include "fuchsia/hardware/bluetooth/cpp/fidl.h"
#include "gmock/gmock.h"
#include "src/connectivity/bluetooth/core/bt-host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/transport/slab_allocators.h"

namespace bt::fidl::testing {

class FakeHciServer final : public fuchsia::hardware::bluetooth::testing::FullHci_TestBase {
 public:
  FakeHciServer(::fidl::InterfaceRequest<fuchsia::hardware::bluetooth::FullHci> request,
                async_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher) {
    binding_.Bind(std::move(request));
  }

  void Unbind() { binding_.Unbind(); }

  zx_status_t SendEvent(const BufferView& event) {
    return command_channel_.write(/*flags=*/0, event.data(), static_cast<uint32_t>(event.size()),
                                  /*handles=*/nullptr, /*num_handles=*/0);
  }
  zx_status_t SendAcl(const BufferView& buffer) {
    return acl_channel_.write(/*flags=*/0, buffer.data(), static_cast<uint32_t>(buffer.size()),
                              /*handles=*/nullptr, /*num_handles=*/0);
  }

  const std::vector<bt::DynamicByteBuffer>& commands_received() const { return commands_received_; }
  const std::vector<bt::DynamicByteBuffer>& acl_packets_received() const {
    return acl_packets_received_;
  }

  bool CloseAclChannel() {
    bool was_valid = acl_channel_.is_valid();
    acl_channel_.reset();
    return was_valid;
  }

  bool acl_channel_valid() const { return acl_channel_.is_valid(); }
  bool command_channel_valid() const { return command_channel_.is_valid(); }

 private:
  void OpenCommandChannel(zx::channel channel, OpenCommandChannelCallback callback) override {
    command_channel_ = std::move(channel);
    InitializeWait(command_wait_, command_channel_);
    callback(fpromise::ok());
  }

  void OpenAclDataChannel(zx::channel channel, OpenAclDataChannelCallback callback) override {
    acl_channel_ = std::move(channel);
    InitializeWait(acl_wait_, acl_channel_);
    callback(fpromise::ok());
  }

  void NotImplemented_(const std::string& name) override { FAIL() << name << " not implemented"; }

  void InitializeWait(async::WaitBase& wait, zx::channel& channel) {
    BT_ASSERT(channel.is_valid());
    wait.Cancel();
    wait.set_object(channel.get());
    wait.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
    BT_ASSERT(wait.Begin(dispatcher_) == ZX_OK);
  }

  void OnAclSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                   const zx_packet_signal_t* signal) {
    ASSERT_TRUE(status == ZX_OK);
    if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
      acl_channel_.reset();
      return;
    }
    ASSERT_TRUE(signal->observed & ZX_CHANNEL_READABLE);

    bt::StaticByteBuffer<hci::allocators::kLargeACLDataPacketSize> buffer;
    uint32_t read_size = 0;
    zx_status_t read_status = acl_channel_.read(0u, buffer.mutable_data(), /*handles=*/nullptr,
                                                static_cast<uint32_t>(buffer.size()), 0, &read_size,
                                                /*actual_handles=*/nullptr);
    ASSERT_TRUE(read_status == ZX_OK);
    acl_packets_received_.emplace_back(bt::BufferView(buffer, read_size));
    acl_wait_.Begin(dispatcher_);
  }

  void OnCommandSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                       const zx_packet_signal_t* signal) {
    ASSERT_TRUE(status == ZX_OK);
    if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
      command_channel_.reset();
      return;
    }
    ASSERT_TRUE(signal->observed & ZX_CHANNEL_READABLE);

    bt::StaticByteBuffer<hci::allocators::kLargeControlPacketSize> buffer;
    uint32_t read_size = 0;
    zx_status_t read_status =
        command_channel_.read(0u, buffer.mutable_data(), /*handles=*/nullptr,
                              static_cast<uint32_t>(buffer.size()), 0, &read_size,
                              /*actual_handles=*/nullptr);
    ASSERT_TRUE(read_status == ZX_OK);
    commands_received_.emplace_back(bt::BufferView(buffer, read_size));
    command_wait_.Begin(dispatcher_);
  }

  ::fidl::Binding<fuchsia::hardware::bluetooth::FullHci> binding_{this};

  zx::channel command_channel_;
  std::vector<bt::DynamicByteBuffer> commands_received_;

  zx::channel acl_channel_;
  std::vector<bt::DynamicByteBuffer> acl_packets_received_;

  async::WaitMethod<FakeHciServer, &FakeHciServer::OnAclSignal> acl_wait_{this};
  async::WaitMethod<FakeHciServer, &FakeHciServer::OnCommandSignal> command_wait_{this};

  async_dispatcher_t* dispatcher_;
};

}  // namespace bt::fidl::testing

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_HCI_SERVER_H_
