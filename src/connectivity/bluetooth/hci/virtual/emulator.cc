// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "emulator.h"

#include <fuchsia/hardware/bt/hci/cpp/banjo.h>
#include <lib/async/cpp/task.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <cstdio>
#include <future>
#include <thread>

#include "src/connectivity/bluetooth/core/bt-host/common/random.h"
#include "src/connectivity/bluetooth/core/bt-host/testing/fake_peer.h"
#include "src/connectivity/bluetooth/hci/virtual/log.h"

namespace fbt = fuchsia::bluetooth;
namespace ftest = fuchsia::bluetooth::test;

using bt::DeviceAddress;
using bt::testing::FakeController;

namespace bt_hci_virtual {
namespace {

// Arbitrary value to signal between userland eventpairs.
constexpr uint32_t PEER_SIGNAL = ZX_USER_SIGNAL_0;

FakeController::Settings SettingsFromFidl(const ftest::EmulatorSettings& input) {
  FakeController::Settings settings;
  if (input.has_hci_config() && input.hci_config() == ftest::HciConfig::LE_ONLY) {
    settings.ApplyLEOnlyDefaults();
  } else {
    settings.ApplyDualModeDefaults();
  }

  if (input.has_address()) {
    settings.bd_addr = DeviceAddress(DeviceAddress::Type::kBREDR, input.address().bytes);
  }

  // TODO(armansito): Don't ignore "extended_advertising" setting when
  // supported.
  if (input.has_acl_buffer_settings()) {
    settings.acl_data_packet_length = input.acl_buffer_settings().data_packet_length;
    settings.total_num_acl_data_packets = input.acl_buffer_settings().total_num_data_packets;
  }

  if (input.has_le_acl_buffer_settings()) {
    settings.le_acl_data_packet_length = input.le_acl_buffer_settings().data_packet_length;
    settings.le_total_num_acl_data_packets = input.le_acl_buffer_settings().total_num_data_packets;
  }

  return settings;
}

fuchsia::bluetooth::AddressType LeOwnAddressTypeToFidl(
    pw::bluetooth::emboss::LEOwnAddressType type) {
  switch (type) {
    case pw::bluetooth::emboss::LEOwnAddressType::PUBLIC:
    case pw::bluetooth::emboss::LEOwnAddressType::PRIVATE_DEFAULT_TO_PUBLIC:
      return fuchsia::bluetooth::AddressType::PUBLIC;
    case pw::bluetooth::emboss::LEOwnAddressType::RANDOM:
    case pw::bluetooth::emboss::LEOwnAddressType::PRIVATE_DEFAULT_TO_RANDOM:
      return fuchsia::bluetooth::AddressType::RANDOM;
  }

  ZX_PANIC("unsupported own address type");
  return fuchsia::bluetooth::AddressType::PUBLIC;
}

constexpr EmulatorDevice* DEV(void* ctx) { return static_cast<EmulatorDevice*>(ctx); }

constexpr zx_protocol_device_t bt_emulator_device_ops = {
    .version = DEVICE_OPS_VERSION,
    .get_protocol = [](void* ctx, uint32_t proto_id, void* out_proto) -> zx_status_t {
      return DEV(ctx)->GetProtocol(proto_id, out_proto);
    },
    .unbind = [](void* ctx) { DEV(ctx)->Unbind(); },
    .release = [](void* ctx) { DEV(ctx)->Release(); },
    .message =
        [](void* ctx, fidl_incoming_msg_t msg, device_fidl_txn_t txn) {
          logf(TRACE, "EmulatorMessage\n");
          fidl::WireDispatch<fuchsia_hardware_bluetooth::Emulator>(
              DEV(ctx), fidl::IncomingHeaderAndMessage::FromEncodedCMessage(msg),
              ddk::FromDeviceFIDLTransaction(txn));
        },
};

// NOTE: We do not implement unbind and release. The lifecycle of the bt-hci
// device is strictly tied to the bt-emulator device (i.e. it can never out-live
// bt-emulator). We handle its destruction in the bt_emulator_device_ops
// messages.
constexpr zx_protocol_device_t bt_hci_device_ops = {
    .version = DEVICE_OPS_VERSION,
    .get_protocol = [](void* ctx, uint32_t proto_id, void* out_proto) -> zx_status_t {
      return DEV(ctx)->GetProtocol(proto_id, out_proto);
    },
    .message =
        [](void* ctx, fidl_incoming_msg_t msg, device_fidl_txn_t txn) {
          logf(TRACE, "HciMessage\n");
          fidl::WireDispatch<fuchsia_hardware_bluetooth::Hci>(
              DEV(ctx), fidl::IncomingHeaderAndMessage::FromEncodedCMessage(msg),
              ddk::FromDeviceFIDLTransaction(txn));
        },
};

constexpr bt_hci_protocol_ops_t hci_protocol_ops = {
    .open_command_channel = [](void* ctx, zx_handle_t chan) -> zx_status_t {
      return DEV(ctx)->OpenChan(Channel::COMMAND, chan);
    },
    .open_acl_data_channel = [](void* ctx, zx_handle_t chan) -> zx_status_t {
      return DEV(ctx)->OpenChan(Channel::ACL, chan);
    },
    .open_sco_channel = [](void* ctx, zx_handle_t channel) -> zx_status_t {
      zx_handle_close(channel);
      return ZX_ERR_NOT_SUPPORTED;
    },
    .configure_sco = [](void* ctx, sco_coding_format_t coding_format, sco_encoding_t encoding,
                        sco_sample_rate_t sample_rate, bt_hci_configure_sco_callback callback,
                        void* cookie) { callback(cookie, ZX_ERR_NOT_SUPPORTED); },
    .reset_sco = [](void* ctx, bt_hci_reset_sco_callback callback,
                    void* cookie) { callback(cookie, ZX_ERR_NOT_SUPPORTED); },
    .open_snoop_channel = [](void* ctx, zx_handle_t chan) -> zx_status_t {
      return DEV(ctx)->OpenChan(Channel::SNOOP, chan);
    },
};

}  // namespace

EmulatorDevice::EmulatorDevice(zx_device_t* device)
    : loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
      pw_dispatcher_(loop_.dispatcher()),
      parent_(device),
      hci_dev_(nullptr),
      emulator_dev_(nullptr),
      fake_device_(pw_dispatcher_),
      binding_(this) {}

zx_status_t EmulatorDevice::Bind(std::string_view name) {
  logf(TRACE, "bind\n");

  device_add_args_t args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = name.data(),
      .ctx = this,
      .ops = &bt_emulator_device_ops,
      .proto_id = ZX_PROTOCOL_BT_EMULATOR,
      .flags = DEVICE_ADD_NON_BINDABLE,
  };
  zx_status_t status = device_add(parent_, &args, &emulator_dev_);
  if (status != ZX_OK) {
    logf(ERROR, "could not add bt-emulator device: %s\n", zx_status_get_string(status));
    return status;
  }

  bt::set_random_generator(&rng_);

  auto init_complete_cb = [](pw::Status status) {
    if (!status.ok()) {
    }
  };
  auto error_cb = [this](pw::Status status) {
    logf(WARNING, "FakeController error: %s", pw_StatusString(status));
    UnpublishHci();
  };

  fake_device_.Initialize(init_complete_cb, error_cb);

  fake_device_.set_controller_parameters_callback(
      fit::bind_member<&EmulatorDevice::OnControllerParametersChanged>(this));
  fake_device_.set_advertising_state_callback(
      fit::bind_member<&EmulatorDevice::OnLegacyAdvertisingStateChanged>(this));
  fake_device_.set_connection_state_callback(
      fit::bind_member<&EmulatorDevice::OnPeerConnectionStateChanged>(this));

  loop_.StartThread("bt_hci_virtual");

  return status;
}

void EmulatorDevice::Unbind() {
  logf(TRACE, "unbind\n");
  zx::eventpair this_thread_waiter, loop_signaller;
  zx_status_t status = zx::eventpair::create(0, &this_thread_waiter, &loop_signaller);
  // If eventpair creation fails, the rest of Unbind will not work properly, so we assert to fail
  // fast and obviously. This is OK since the emulator is only run in tests anyway.
  ZX_ASSERT_MSG(status == ZX_OK, "could not create eventpair: %s\n", zx_status_get_string(status));

  // It is OK to capture a self-reference since this function blocks on the task completion.
  async::PostTask(loop_.dispatcher(), [this, loop_signaller = std::move(loop_signaller)] {
    // Stop servicing HciEmulator FIDL messages from higher layers.
    binding_.Unbind();
    // Unpublish the bt-hci device.
    UnpublishHci();
    zx_status_t status = loop_signaller.signal_peer(/*clear_mask=*/0, /*set_mask=*/PEER_SIGNAL);
    if (status != ZX_OK) {
      logf(ERROR, "could not signal event peer: %s\n", zx_status_get_string(status));
    }
  });

  // Block here to ensure that UnpublishHci runs before Unbind completion.  We use EventPair instead
  // of loop_.JoinThreads to block because fake_device_ has tasks on the loop_ that won't complete
  // until fake_device_ is stopped during Release.
  zx_signals_t _ignored;
  status = this_thread_waiter.wait_one(PEER_SIGNAL, zx::time::infinite(), &_ignored);
  if (status != ZX_OK) {
    logf(ERROR, "failed to wait for eventpair signal: %s\n", zx_status_get_string(status));
  } else {
    logf(TRACE, "emulator's bt-hci device unpublished\n");
  }

  device_unbind_reply(emulator_dev_);
  emulator_dev_ = nullptr;
}

void EmulatorDevice::Release() {
  logf(TRACE, "release\n");
  // Clean up fake_device_ on the dispatcher thread due to its thread-safety requirements. It is OK
  // to capture references to members in the task since this function blocks until the dispatcher
  // loop terminates.
  // This is done in Release (vs. Unbind) because bt-host, a child of this device's bt-hci child,
  // has open channels to fake_device_, so fake_device_ cannot be safely shut down until that bt-
  // host child is released, which is only guaranteed during this Release.
  async::PostTask(loop_.dispatcher(), [this] {
    fake_device_.Stop();
    loop_.Quit();
    // Clean up all fake peers. This will close their local channels and remove them
    // from the fake controller.
    peers_.clear();
  });

  // Block here until all the shutdown tasks we just posted are completed on the FIDL/emulator
  // dispatcher thread to guarantee that the operations below don't happen concurrently with them.
  loop_.JoinThreads();
  logf(TRACE, "emulator dispatcher shut down\n");

  delete this;
}

zx_status_t EmulatorDevice::GetProtocol(uint32_t proto_id, void* out_proto) {
  // The bt-emulator device doesn't support a non-FIDL protocol.
  if (proto_id != ZX_PROTOCOL_BT_HCI) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  bt_hci_protocol_t* hci_proto = static_cast<bt_hci_protocol_t*>(out_proto);
  hci_proto->ctx = this;
  hci_proto->ops = &hci_protocol_ops;

  return ZX_OK;
}

zx_status_t EmulatorDevice::OpenChan(Channel chan_type, zx_handle_t chan) {
  logf(TRACE, "open HCI channel\n");

  zx::channel in(chan);

  if (chan_type == Channel::COMMAND) {
    async::PostTask(loop_.dispatcher(),
                    [this, in = std::move(in)]() mutable { StartCmdChannel(std::move(in)); });
  } else if (chan_type == Channel::ACL) {
    async::PostTask(loop_.dispatcher(),
                    [this, in = std::move(in)]() mutable { StartAclChannel(std::move(in)); });
  } else if (chan_type == Channel::EMULATOR) {
    async::PostTask(loop_.dispatcher(), [this, in = std::move(in)]() mutable {
      StartEmulatorInterface(std::move(in));
    });
  } else {
    return ZX_ERR_NOT_SUPPORTED;
  }
  return ZX_OK;
}

void EmulatorDevice::StartEmulatorInterface(zx::channel chan) {
  logf(TRACE, "start HciEmulator interface\n");

  if (binding_.is_bound()) {
    logf(TRACE, "HciEmulator channel already bound\n");
    return;
  }

  // Process HciEmulator messages on a thread that can safely access the
  // FakeController, which is thread-hostile.
  binding_.Bind(std::move(chan), loop_.dispatcher());
  binding_.set_error_handler([this](zx_status_t status) {
    logf(TRACE, "emulator channel closed (status: %s); unpublish device\n",
         zx_status_get_string(status));
    UnpublishHci();
  });
}

void EmulatorDevice::Publish(ftest::EmulatorSettings in_settings, PublishCallback callback) {
  logf(TRACE, "HciEmulator.Publish\n");

  ftest::HciEmulator_Publish_Result result;
  // Between Device::Unbind & Device::Release, this->hci_dev_ == nullptr, but this->fake_device_
  // != nullptr. This seems like it might cause issues for this logic; however, because binding_ is
  // unbound during Device::Unbind, it is impossible for further messages, including Publish, to be
  // received during this window.
  if (hci_dev_) {
    result.set_err(ftest::EmulatorError::HCI_ALREADY_PUBLISHED);
    callback(std::move(result));
    return;
  }

  FakeController::Settings settings = SettingsFromFidl(in_settings);
  fake_device_.set_settings(settings);

  // Publish the bt-hci device.
  device_add_args_t args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = "bt_hci_virtual",
      .ctx = this,
      .ops = &bt_hci_device_ops,
      .proto_id = ZX_PROTOCOL_BT_HCI,
  };
  zx_status_t status = device_add(emulator_dev_, &args, &hci_dev_);
  if (status != ZX_OK) {
    result.set_err(ftest::EmulatorError::FAILED);
  } else {
    result.set_response(ftest::HciEmulator_Publish_Response{});
  }

  callback(std::move(result));
}

void EmulatorDevice::AddLowEnergyPeer(ftest::LowEnergyPeerParameters params,
                                      fidl::InterfaceRequest<ftest::Peer> request,
                                      AddLowEnergyPeerCallback callback) {
  logf(TRACE, "HciEmulator.AddLowEnergyPeer\n");

  ftest::HciEmulator_AddLowEnergyPeer_Result fidl_result;

  auto result = EmulatedPeer::NewLowEnergy(std::move(params), std::move(request), &fake_device_);
  if (result.is_error()) {
    fidl_result.set_err(result.error());
    callback(std::move(fidl_result));
    return;
  }

  AddPeer(result.take_value());
  fidl_result.set_response(ftest::HciEmulator_AddLowEnergyPeer_Response{});
  callback(std::move(fidl_result));
}

void EmulatorDevice::AddBredrPeer(ftest::BredrPeerParameters params,
                                  fidl::InterfaceRequest<fuchsia::bluetooth::test::Peer> request,
                                  AddBredrPeerCallback callback) {
  logf(TRACE, "HciEmulator.AddBredrPeer\n");

  ftest::HciEmulator_AddBredrPeer_Result fidl_result;

  auto result = EmulatedPeer::NewBredr(std::move(params), std::move(request), &fake_device_);
  if (result.is_error()) {
    fidl_result.set_err(result.error());
    callback(std::move(fidl_result));
    return;
  }

  AddPeer(result.take_value());
  fidl_result.set_response(ftest::HciEmulator_AddBredrPeer_Response{});
  callback(std::move(fidl_result));
}

void EmulatorDevice::WatchControllerParameters(WatchControllerParametersCallback callback) {
  logf(TRACE, "HciEmulator.WatchControllerParameters\n");
  controller_parameters_getter_.Watch(std::move(callback));
}

void EmulatorDevice::WatchLeScanStates(WatchLeScanStatesCallback callback) {
  // TODO(fxbug.dev/822): Implement
}

void EmulatorDevice::WatchLegacyAdvertisingStates(WatchLegacyAdvertisingStatesCallback callback) {
  logf(TRACE, "HciEmulator.WatchLegacyAdvertisingState\n");
  legacy_adv_state_getter_.Watch(std::move(callback));
}

void EmulatorDevice::AddPeer(std::unique_ptr<EmulatedPeer> peer) {
  auto address = peer->address();
  peer->set_closed_callback([this, address] { peers_.erase(address); });
  peers_[address] = std::move(peer);
}

void EmulatorDevice::OnControllerParametersChanged() {
  logf(TRACE, "HciEmulator.OnControllerParametersChanged\n");

  ftest::ControllerParameters fidl_value;
  fidl_value.set_local_name(fake_device_.local_name());

  const auto& device_class_bytes = fake_device_.device_class().bytes();
  uint32_t device_class = 0;
  device_class |= device_class_bytes[0];
  device_class |= static_cast<uint32_t>(device_class_bytes[1]) << 8;
  device_class |= static_cast<uint32_t>(device_class_bytes[2]) << 16;
  fidl_value.set_device_class(fbt::DeviceClass{device_class});

  controller_parameters_getter_.Set(std::move(fidl_value));
}

void EmulatorDevice::OnLegacyAdvertisingStateChanged() {
  logf(TRACE, "HciEmulator.OnLegacyAdvertisingStateChanged\n");

  // We have requests to resolve. Construct the FIDL table for the current state.
  ftest::LegacyAdvertisingState fidl_state;
  const FakeController::LEAdvertisingState& adv_state = fake_device_.legacy_advertising_state();
  fidl_state.set_enabled(adv_state.enabled);

  // Populate the rest only if advertising is enabled.
  fidl_state.set_type(static_cast<ftest::LegacyAdvertisingType>(adv_state.adv_type));
  fidl_state.set_address_type(LeOwnAddressTypeToFidl(adv_state.own_address_type));

  if (adv_state.interval_min) {
    fidl_state.set_interval_min(adv_state.interval_min);
  }
  if (adv_state.interval_max) {
    fidl_state.set_interval_max(adv_state.interval_max);
  }

  if (adv_state.data_length) {
    std::vector<uint8_t> output(adv_state.data_length);
    bt::MutableBufferView output_view(output.data(), output.size());
    output_view.Write(adv_state.data, adv_state.data_length);
    fidl_state.set_advertising_data(std::move(output));
  }
  if (adv_state.scan_rsp_length) {
    std::vector<uint8_t> output(adv_state.scan_rsp_length);
    bt::MutableBufferView output_view(output.data(), output.size());
    output_view.Write(adv_state.scan_rsp_data, adv_state.scan_rsp_length);
    fidl_state.set_scan_response(std::move(output));
  }

  legacy_adv_state_getter_.Add(std::move(fidl_state));
}

void EmulatorDevice::UnpublishHci() {
  if (hci_dev_) {
    device_async_remove(hci_dev_);
    hci_dev_ = nullptr;
  }
}

void EmulatorDevice::OnPeerConnectionStateChanged(const bt::DeviceAddress& address,
                                                  bt::hci_spec::ConnectionHandle handle,
                                                  bool connected, bool canceled) {
  logf(TRACE, "Peer connection state changed: %s (handle: %#.4x) (connected: %s) (canceled: %s):\n",
       address.ToString().c_str(), handle, (connected ? "true" : "false"),
       (canceled ? "true" : "false"));

  auto iter = peers_.find(address);
  if (iter != peers_.end()) {
    iter->second->UpdateConnectionState(connected);
  }
}

// Starts listening for command/event packets on the given channel.
// Returns false if already listening on a command channel
bool EmulatorDevice::StartCmdChannel(zx::channel chan) {
  if (cmd_channel_.is_valid()) {
    return false;
  }

  fake_device_.SetEventFunction(fit::bind_member<&EmulatorDevice::SendEvent>(this));

  cmd_channel_ = std::move(chan);
  cmd_channel_wait_.set_object(cmd_channel_.get());
  cmd_channel_wait_.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
  zx_status_t status = cmd_channel_wait_.Begin(async_get_default_dispatcher());
  if (status != ZX_OK) {
    cmd_channel_.reset();
    logf(ERROR, "failed to start command channel: %s", zx_status_get_string(status));
    return false;
  }
  return true;
}

// Starts listening for acl packets on the given channel.
// Returns false if already listening on a acl channel
bool EmulatorDevice::StartAclChannel(zx::channel chan) {
  if (acl_channel_.is_valid()) {
    return false;
  }

  // Enable FakeController to send packets to bt-host.
  fake_device_.SetReceiveAclFunction(fit::bind_member<&EmulatorDevice::SendAclPacket>(this));

  // Enable bt-host to send packets to FakeController.
  acl_channel_ = std::move(chan);
  acl_channel_wait_.set_object(acl_channel_.get());
  acl_channel_wait_.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
  zx_status_t status = acl_channel_wait_.Begin(async_get_default_dispatcher());
  if (status != ZX_OK) {
    acl_channel_.reset();
    logf(ERROR, "failed to start ACL channel: %s", zx_status_get_string(status));
    return false;
  }
  return true;
}

void EmulatorDevice::CloseCommandChannel() {
  if (cmd_channel_.is_valid()) {
    cmd_channel_wait_.Cancel();
    cmd_channel_.reset();
  }
  fake_device_.Stop();
}

void EmulatorDevice::CloseAclDataChannel() {
  if (acl_channel_.is_valid()) {
    acl_channel_wait_.Cancel();
    acl_channel_.reset();
  }
  fake_device_.Stop();
}

void EmulatorDevice::SendEvent(pw::span<const std::byte> buffer) {
  zx_status_t status = cmd_channel_.write(/*flags=*/0, buffer.data(), buffer.size(),
                                          /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    logf(WARNING, "failed to write event");
  }
}

void EmulatorDevice::SendAclPacket(pw::span<const std::byte> buffer) {
  zx_status_t status = acl_channel_.write(/*flags=*/0, buffer.data(), buffer.size(),
                                          /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    logf(WARNING, "failed to write ACL packet");
  }
}

// Read and handle packets received over the channels.
void EmulatorDevice::HandleCommandPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                                         zx_status_t wait_status,
                                         const zx_packet_signal_t* signal) {
  std::array<std::byte,
             bt::hci_spec::kMaxCommandPacketPayloadSize + sizeof(bt::hci_spec::CommandHeader)>
      buffer;

  uint32_t read_size;
  zx_status_t status =
      cmd_channel_.read(0u, buffer.data(), /*handles=*/nullptr, buffer.size(), 0, &read_size,
                        /*actual_handles=*/nullptr);
  ZX_DEBUG_ASSERT(status == ZX_OK || status == ZX_ERR_PEER_CLOSED);
  if (status < 0) {
    if (status == ZX_ERR_PEER_CLOSED) {
      logf(INFO, "command channel was closed");
    } else {
      logf(ERROR, "failed to read on cmd channel: %s", zx_status_get_string(status));
    }
    CloseCommandChannel();
    return;
  }

  fake_device_.SendCommand(buffer);

  status = wait->Begin(dispatcher);
  if (status != ZX_OK) {
    bt_log(ERROR, "fake-hci", "failed to wait on cmd channel: %s", zx_status_get_string(status));
    CloseCommandChannel();
  }
}

void EmulatorDevice::HandleAclPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                                     zx_status_t wait_status, const zx_packet_signal_t* signal) {
  std::array<std::byte, bt::hci_spec::kMaxACLPayloadSize + sizeof(bt::hci_spec::ACLDataHeader)>
      buffer;
  uint32_t read_size;
  zx_status_t status = acl_channel_.read(0u, buffer.data(), /*handles=*/nullptr, buffer.size(), 0,
                                         &read_size, /*actual_handles=*/nullptr);
  ZX_DEBUG_ASSERT(status == ZX_OK || status == ZX_ERR_PEER_CLOSED);
  if (status < 0) {
    if (status == ZX_ERR_PEER_CLOSED) {
      logf(INFO, "ACL channel was closed");
    } else {
      logf(ERROR, "failed to read on ACL channel: %s", zx_status_get_string(status));
    }

    CloseAclDataChannel();
    return;
  }

  if (read_size < sizeof(bt::hci_spec::ACLDataHeader)) {
    logf(ERROR, "malformed ACL packet received");
  } else {
    fake_device_.SendAclData(buffer);
  }

  status = wait->Begin(dispatcher);
  if (status != ZX_OK) {
    logf(ERROR, "failed to wait on ACL channel: %s", zx_status_get_string(status));
    CloseAclDataChannel();
  }
}

void EmulatorDevice::OpenCommandChannel(OpenCommandChannelRequestView request,
                                        OpenCommandChannelCompleter::Sync& completer) {
  if (zx_status_t status = OpenChan(Channel::COMMAND, request->channel.release());
      status != ZX_OK) {
    completer.Close(status);
  }
}

void EmulatorDevice::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                        OpenAclDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = OpenChan(Channel::ACL, request->channel.release()); status != ZX_OK) {
    completer.Close(status);
  }
}

void EmulatorDevice::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                      OpenSnoopChannelCompleter::Sync& completer) {
  if (zx_status_t status = OpenChan(Channel::SNOOP, request->channel.release()); status != ZX_OK) {
    completer.Close(status);
  }
}

void EmulatorDevice::Open(OpenRequestView request, OpenCompleter::Sync& completer) {
  if (zx_status_t status = OpenChan(Channel::EMULATOR, request->channel.TakeChannel().release());
      status != ZX_OK) {
    completer.Close(status);
  }
}

}  // namespace bt_hci_virtual
