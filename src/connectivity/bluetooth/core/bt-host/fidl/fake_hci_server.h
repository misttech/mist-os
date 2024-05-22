// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_HCI_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_HCI_SERVER_H_

#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/channel.h>

#include <string>

#include "gmock/gmock.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_common.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/slab_allocators.h"

namespace bt::fidl::testing {

class FakeHciServer final : public ::fidl::Server<fuchsia_hardware_bluetooth::Hci> {
 public:
  FakeHciServer(::fidl::ServerEnd<fuchsia_hardware_bluetooth::Hci> server_end,
                async_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher),
        binding_(::fidl::BindServer(dispatcher_, std::move(server_end), this)) {}

  void Unbind() { binding_.Unbind(); }

  zx_status_t SendEvent(const BufferView& event) {
    return command_channel_.write(/*flags=*/0, event.data(), static_cast<uint32_t>(event.size()),
                                  /*handles=*/nullptr, /*num_handles=*/0);
  }
  zx_status_t SendAcl(const BufferView& buffer) {
    return acl_channel_.write(/*flags=*/0, buffer.data(), static_cast<uint32_t>(buffer.size()),
                              /*handles=*/nullptr, /*num_handles=*/0);
  }
  zx_status_t SendSco(const BufferView& buffer) {
    return sco_channel_.write(/*flags=*/0, buffer.data(), static_cast<uint32_t>(buffer.size()),
                              /*handles=*/nullptr, /*num_handles=*/0);
  }
  zx_status_t SendIso(const BufferView& buffer) {
    return iso_channel_.write(/*flags=*/0, buffer.data(), static_cast<uint32_t>(buffer.size()),
                              /*handles=*/nullptr, /*num_handles=*/0);
  }

  const std::vector<bt::DynamicByteBuffer>& commands_received() const { return commands_received_; }
  const std::vector<bt::DynamicByteBuffer>& acl_packets_received() const {
    return acl_packets_received_;
  }
  const std::vector<bt::DynamicByteBuffer>& sco_packets_received() const {
    return sco_packets_received_;
  }
  const std::vector<bt::DynamicByteBuffer>& iso_packets_received() const {
    return iso_packets_received_;
  }

  bool CloseCommandChannel() {
    bool was_valid = command_channel_valid();
    command_channel_.reset();
    return was_valid;
  }

  bool CloseAclChannel() {
    bool was_valid = acl_channel_.is_valid();
    acl_channel_.reset();
    return was_valid;
  }

  // Use custom |ConfigureScoTestCallback| to manually verify configuration fields from tests
  using ConfigureScoTestCallback = fit::function<void(fuchsia_hardware_bluetooth::ScoCodingFormat,
                                                      fuchsia_hardware_bluetooth::ScoEncoding,
                                                      fuchsia_hardware_bluetooth::ScoSampleRate)>;
  void set_check_configure_sco(ConfigureScoTestCallback callback) {
    check_configure_sco_ = std::move(callback);
  }

  // Uee custom |ResetScoTestCallback| to manually perform reset actions from tests
  using ResetScoTestCallback = fit::function<void()>;
  void set_reset_sco_callback(ResetScoTestCallback callback) {
    reset_sco_cb_ = std::move(callback);
  }

  bool acl_channel_valid() const { return acl_channel_.is_valid(); }
  bool command_channel_valid() const { return command_channel_.is_valid(); }
  bool sco_channel_valid() const { return sco_channel_.is_valid(); }
  bool iso_channel_valid() const { return iso_channel_.is_valid(); }

 private:
  void OpenCommandChannel(OpenCommandChannelRequest& request,
                          OpenCommandChannelCompleter::Sync& completer) override {
    command_channel_ = std::move(request.channel());
    InitializeWait(command_wait_, command_channel_);
    completer.Reply(fit::success());
  }

  void OpenAclDataChannel(OpenAclDataChannelRequest& request,
                          OpenAclDataChannelCompleter::Sync& completer) override {
    acl_channel_ = std::move(request.channel());
    InitializeWait(acl_wait_, acl_channel_);
    completer.Reply(fit::success());
  }

  void OpenScoDataChannel(OpenScoDataChannelRequest& request,
                          OpenScoDataChannelCompleter::Sync& completer) override {
    sco_channel_ = std::move(request.channel());
    InitializeWait(sco_wait_, sco_channel_);
    completer.Reply(fit::success());
  }

  void OpenIsoDataChannel(OpenIsoDataChannelRequest& request,
                          OpenIsoDataChannelCompleter::Sync& completer) override {
    iso_channel_ = std::move(request.channel());
    InitializeWait(iso_wait_, iso_channel_);
    completer.Reply(fit::success());
  }

  void ConfigureSco(ConfigureScoRequest& request, ConfigureScoCompleter::Sync& completer) override {
    if (check_configure_sco_) {
      check_configure_sco_(request.coding_format(), request.encoding(), request.sample_rate());
    }
    completer.Reply(fit::success());
  }

  void ResetSco(ResetScoCompleter::Sync& completer) override {
    if (reset_sco_cb_) {
      reset_sco_cb_();
    }
    completer.Reply(fit::success());
  }

  void OpenSnoopChannel(OpenSnoopChannelRequest& request,
                        OpenSnoopChannelCompleter::Sync& completer) override {
    completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
  }

  void handle_unknown_method(
      ::fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
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

  void OnScoSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                   const zx_packet_signal_t* signal) {
    ASSERT_TRUE(status == ZX_OK);
    if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
      sco_channel_.reset();
      return;
    }
    ASSERT_TRUE(signal->observed & ZX_CHANNEL_READABLE);

    bt::StaticByteBuffer<hci::allocators::kMaxScoDataPacketSize> buffer;
    uint32_t read_size = 0;
    zx_status_t read_status = sco_channel_.read(0u, buffer.mutable_data(), /*handles=*/nullptr,
                                                static_cast<uint32_t>(buffer.size()), 0, &read_size,
                                                /*actual_handles=*/nullptr);
    ASSERT_TRUE(read_status == ZX_OK);
    sco_packets_received_.emplace_back(bt::BufferView(buffer, read_size));
    sco_wait_.Begin(dispatcher_);
  }

  void OnIsoSignal(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                   const zx_packet_signal_t* signal) {
    ASSERT_TRUE(status == ZX_OK);
    if (signal->observed & ZX_CHANNEL_PEER_CLOSED) {
      iso_channel_.reset();
      return;
    }
    ASSERT_TRUE(signal->observed & ZX_CHANNEL_READABLE);

    bt::StaticByteBuffer<iso::kMaxIsochronousDataPacketSize> buffer;
    uint32_t read_size = 0;
    zx_status_t read_status = iso_channel_.read(0u, buffer.mutable_data(), /*handles=*/nullptr,
                                                static_cast<uint32_t>(buffer.size()), 0, &read_size,
                                                /*actual_handles=*/nullptr);
    ASSERT_TRUE(read_status == ZX_OK);
    iso_packets_received_.emplace_back(bt::BufferView(buffer, read_size));
    iso_wait_.Begin(dispatcher_);
  }

  zx::channel command_channel_;
  std::vector<bt::DynamicByteBuffer> commands_received_;

  zx::channel acl_channel_;
  std::vector<bt::DynamicByteBuffer> acl_packets_received_;

  zx::channel sco_channel_;
  std::vector<bt::DynamicByteBuffer> sco_packets_received_;
  ConfigureScoTestCallback check_configure_sco_;
  ResetScoTestCallback reset_sco_cb_;

  zx::channel iso_channel_;
  std::vector<bt::DynamicByteBuffer> iso_packets_received_;

  async::WaitMethod<FakeHciServer, &FakeHciServer::OnAclSignal> acl_wait_{this};
  async::WaitMethod<FakeHciServer, &FakeHciServer::OnCommandSignal> command_wait_{this};
  async::WaitMethod<FakeHciServer, &FakeHciServer::OnScoSignal> sco_wait_{this};
  async::WaitMethod<FakeHciServer, &FakeHciServer::OnIsoSignal> iso_wait_{this};

  async_dispatcher_t* dispatcher_;
  ::fidl::ServerBindingRef<fuchsia_hardware_bluetooth::Hci> binding_;
};

}  // namespace bt::fidl::testing

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_FAKE_HCI_SERVER_H_
