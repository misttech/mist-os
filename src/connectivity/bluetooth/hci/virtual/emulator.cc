// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "emulator.h"

#include <fidl/fuchsia.bluetooth.test/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/dispatcher.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/random.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_controller.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_peer.h"
#include "src/connectivity/bluetooth/hci/vendor/broadcom/packets.h"
#include "src/connectivity/bluetooth/hci/virtual/emulated_peer.h"

namespace ftest = fuchsia_bluetooth_test;

using bt::DeviceAddress;
using bt::testing::FakeController;

namespace bt_hci_virtual {
namespace {

const char* ChannelTypeToString(ChannelType chan_type) {
  switch (chan_type) {
    case ChannelType::ACL:
      return "ACL";
    case ChannelType::EMULATOR:
      return "EMULATOR";
    case ChannelType::COMMAND:
      return "COMMAND";
    case ChannelType::ISO:
      return "ISO";
    case ChannelType::SNOOP:
      return "SNOOP";
  }
}

FakeController::Settings SettingsFromFidl(const ftest::EmulatorSettings& input) {
  FakeController::Settings settings;
  if (input.hci_config().has_value() && input.hci_config().value() == ftest::HciConfig::kLeOnly) {
    settings.ApplyLEOnlyDefaults();
  } else {
    settings.ApplyDualModeDefaults();
  }

  if (input.address().has_value()) {
    settings.bd_addr = DeviceAddress(DeviceAddress::Type::kBREDR, input.address()->bytes());
  }

  // TODO(armansito): Don't ignore "extended_advertising" setting when
  // supported.
  if (input.acl_buffer_settings().has_value()) {
    settings.acl_data_packet_length = input.acl_buffer_settings()->data_packet_length();
    settings.total_num_acl_data_packets = input.acl_buffer_settings()->total_num_data_packets();
  }

  if (input.le_acl_buffer_settings().has_value()) {
    settings.le_acl_data_packet_length = input.le_acl_buffer_settings()->data_packet_length();
    settings.le_total_num_acl_data_packets =
        input.le_acl_buffer_settings()->total_num_data_packets();
  }

  return settings;
}

std::optional<fuchsia_bluetooth::AddressType> LeOwnAddressTypeToFidl(
    pw::bluetooth::emboss::LEOwnAddressType type) {
  std::optional<fuchsia_bluetooth::AddressType> res;

  switch (type) {
    case pw::bluetooth::emboss::LEOwnAddressType::PUBLIC:
    case pw::bluetooth::emboss::LEOwnAddressType::PRIVATE_DEFAULT_TO_PUBLIC:
      res.emplace(fuchsia_bluetooth::AddressType::kPublic);
      return res;
    case pw::bluetooth::emboss::LEOwnAddressType::RANDOM:
    case pw::bluetooth::emboss::LEOwnAddressType::PRIVATE_DEFAULT_TO_RANDOM:
      res.emplace(fuchsia_bluetooth::AddressType::kRandom);
      return res;
  }

  ZX_PANIC("unsupported own address type");
  return res;
}

}  // namespace

EmulatorDevice::EmulatorDevice()
    : pw_dispatcher_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
      fake_device_(pw_dispatcher_),
      emulator_devfs_connector_(fit::bind_member<&EmulatorDevice::ConnectEmulator>(this)),
      vendor_devfs_connector_(fit::bind_member<&EmulatorDevice::ConnectVendor>(this)) {}

zx_status_t EmulatorDevice::Initialize(std::string_view name, AddChildCallback callback,
                                       ShutdownCallback shutdown) {
  shutdown_cb_ = std::move(shutdown);

  bt::set_random_generator(&rng_);

  // Initialize |fake_device_|
  auto init_complete_cb = [](pw::Status status) {
    if (!status.ok()) {
      FDF_LOG(WARNING, "FakeController failed to initialize: %s", pw_StatusString(status));
    }
  };
  auto error_cb = [this](pw::Status status) {
    FDF_LOG(WARNING, "FakeController error: %s", pw_StatusString(status));
    UnpublishHci();
  };
  fake_device_.Initialize(init_complete_cb, error_cb);

  fake_device_.set_controller_parameters_callback(
      fit::bind_member<&EmulatorDevice::OnControllerParametersChanged>(this));
  fake_device_.set_advertising_state_callback(
      fit::bind_member<&EmulatorDevice::OnLegacyAdvertisingStateChanged>(this));
  fake_device_.set_connection_state_callback(
      fit::bind_member<&EmulatorDevice::OnPeerConnectionStateChanged>(this));

  // Create args to add emulator as a child node on behalf of VirtualController
  zx::result connector =
      emulator_devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connecter to dispatcher: %u", connector.status_value());
    return connector.error_value();
  }

  fidl::Arena args_arena;
  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(args_arena)
                   .connector(std::move(connector.value()))
                   .connector_supports(fuchsia_device_fs::ConnectionType::kController)
                   .class_name("bt-emulator")
                   .Build();
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(args_arena)
                  .name(name.data())
                  .devfs_args(devfs)
                  .Build();
  callback(args);

  return ZX_OK;
}

void EmulatorDevice::Shutdown() {
  // Closing |bindings_| stops servicing HciEmulator FIDL messages and unpublishes the bt-hci-device
  bindings_.CloseAll(ZX_OK);

  fake_device_.Stop();
  peers_.clear();

  if (shutdown_cb_) {
    shutdown_cb_();
  }
}

zx_status_t EmulatorDevice::OpenChannel(ChannelType chan_type, zx_handle_t chan) {
  FDF_LOG(TRACE, "Opening %s HCI channel", ChannelTypeToString(chan_type));

  zx::channel in(chan);

  if (chan_type == ChannelType::EMULATOR) {
    fidl::ServerEnd<ftest::HciEmulator> server_end(std::move(in));
    StartEmulatorInterface(std::move(server_end));
  } else if (chan_type == ChannelType::COMMAND) {
    StartCmdChannel(std::move(in));
  } else if (chan_type == ChannelType::ACL) {
    StartAclChannel(std::move(in));
  } else if (chan_type == ChannelType::ISO) {
    StartIsoChannel(std::move(in));
  } else {
    return ZX_ERR_NOT_SUPPORTED;
  }
  return ZX_OK;
}

void EmulatorDevice::Open(OpenRequestView request, OpenCompleter::Sync& completer) {
  if (zx_status_t status =
          OpenChannel(ChannelType::EMULATOR, request->channel.TakeChannel().release());
      status != ZX_OK) {
    completer.Close(status);
  }
}

void EmulatorDevice::Publish(PublishRequest& request, PublishCompleter::Sync& completer) {
  FDF_LOG(TRACE, "HciEmulator.Publish\n");

  if (hci_node_controller_.is_valid()) {
    FDF_LOG(INFO, "bt-hci-device is already published");
    completer.Reply(fit::error(ftest::EmulatorError::kHciAlreadyPublished));
    return;
  }

  FakeController::Settings settings = SettingsFromFidl(request.settings());
  fake_device_.set_settings(settings);

  zx_status_t status = AddHciDeviceChildNode();
  if (status != ZX_OK) {
    FDF_LOG(WARNING, "Failed to publish bt-hci-device node");
    completer.Reply(fit::error(ftest::EmulatorError::kFailed));
  } else {
    FDF_LOG(INFO, "Successfully published bt-hci-device node");
    completer.Reply(fit::success());
  }
}

void EmulatorDevice::AddLowEnergyPeer(AddLowEnergyPeerRequest& request,
                                      AddLowEnergyPeerCompleter::Sync& completer) {
  FDF_LOG(TRACE, "HciEmulator.AddLowEnergyPeer\n");

  auto result =
      EmulatedPeer::NewLowEnergy(request.parameters(), std::move(request.peer()), &fake_device_,
                                 fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (result.is_error()) {
    completer.Reply(fit::error(result.error()));
    return;
  }

  AddPeer(result.take_value());
  completer.Reply(fit::success());
}

void EmulatorDevice::AddBredrPeer(AddBredrPeerRequest& request,
                                  AddBredrPeerCompleter::Sync& completer) {
  FDF_LOG(TRACE, "HciEmulator.AddBredrPeer\n");

  auto result =
      EmulatedPeer::NewBredr(request.parameters(), std::move(request.peer()), &fake_device_,
                             fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (result.is_error()) {
    completer.Reply(fit::error(result.error()));
    return;
  }

  AddPeer(result.take_value());
  completer.Reply(fit::success());
}

void EmulatorDevice::WatchControllerParameters(
    WatchControllerParametersCompleter::Sync& completer) {
  FDF_LOG(TRACE, "HciEmulator.WatchControllerParameters\n");

  controller_parameters_completer_.emplace(completer.ToAsync());
  MaybeUpdateControllerParametersChanged();
}

void EmulatorDevice::WatchLeScanStates(WatchLeScanStatesCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/42162739): Implement
}

void EmulatorDevice::WatchLegacyAdvertisingStates(
    WatchLegacyAdvertisingStatesCompleter::Sync& completer) {
  FDF_LOG(TRACE, "HciEmulator.WatchLegacyAdvertisingState\n");

  legacy_adv_states_completers_.emplace(completer.ToAsync());
  MaybeUpdateLegacyAdvertisingStates();
}

void EmulatorDevice::EncodeCommand(EncodeCommandRequestView request,
                                   EncodeCommandCompleter::Sync& completer) {
  uint8_t data_buffer[bt_hci_broadcom::kBcmSetAclPriorityCmdSize];
  switch (request->Which()) {
    case fuchsia_hardware_bluetooth::wire::VendorCommand::Tag::kSetAclPriority: {
      EncodeSetAclPriorityCommand(request->set_acl_priority(), data_buffer);
      auto encoded_cmd = fidl::VectorView<uint8_t>::FromExternal(
          data_buffer, bt_hci_broadcom::kBcmSetAclPriorityCmdSize);
      completer.ReplySuccess(encoded_cmd);
      return;
    }
    default: {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
  }
}

void EmulatorDevice::OpenHci(OpenHciCompleter::Sync& completer) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::Hci>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", zx_status_get_string(endpoints.error_value()));
    completer.ReplyError(endpoints.error_value());
    return;
  }
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(endpoints->server),
                   this);
  completer.ReplySuccess(std::move(endpoints->client));
}

void EmulatorDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Vendor request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void EmulatorDevice::OpenCommandChannel(OpenCommandChannelRequestView request,
                                        OpenCommandChannelCompleter::Sync& completer) {
  if (zx_status_t status = OpenChannel(ChannelType::COMMAND, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void EmulatorDevice::OpenAclDataChannel(OpenAclDataChannelRequestView request,
                                        OpenAclDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = OpenChannel(ChannelType::ACL, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void EmulatorDevice::OpenScoDataChannel(OpenScoDataChannelRequestView request,
                                        OpenScoDataChannelCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void EmulatorDevice::ConfigureSco(ConfigureScoRequestView request,
                                  ConfigureScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void EmulatorDevice::ResetSco(ResetScoCompleter::Sync& completer) {
  // This interface is not implemented.
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void EmulatorDevice::OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                                        OpenIsoDataChannelCompleter::Sync& completer) {
  if (zx_status_t status = OpenChannel(ChannelType::ISO, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void EmulatorDevice::OpenSnoopChannel(OpenSnoopChannelRequestView request,
                                      OpenSnoopChannelCompleter::Sync& completer) {
  if (zx_status_t status = OpenChannel(ChannelType::SNOOP, request->channel.release());
      status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

void EmulatorDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Hci request, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void EmulatorDevice::ConnectEmulator(
    fidl::ServerEnd<fuchsia_hardware_bluetooth::Emulator> request) {
  emulator_binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                     std::move(request), this, fidl::kIgnoreBindingClosure);
}

void EmulatorDevice::ConnectVendor(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request) {
  vendor_binding_group_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   std::move(request), this, fidl::kIgnoreBindingClosure);

  vendor_binding_group_.ForEachBinding(
      [](const fidl::ServerBinding<fuchsia_hardware_bluetooth::Vendor>& binding) {
        fidl::Arena arena;
        auto builder = fuchsia_hardware_bluetooth::wire::VendorFeatures::Builder(arena);
        builder.acl_priority_command(true);
        fidl::Status status = fidl::WireSendEvent(binding)->OnFeatures(builder.Build());

        if (status.status() != ZX_OK) {
          FDF_LOG(ERROR, "Failed to send vendor features to bt-host: %s", status.status_string());
        }
      });
}

void EmulatorDevice::StartEmulatorInterface(fidl::ServerEnd<ftest::HciEmulator> request) {
  FDF_LOG(TRACE, "start HciEmulator interface\n");

  if (bindings_.size() > 0) {
    FDF_LOG(TRACE, "HciEmulator channel already bound\n");
    return;
  }

  // Process HciEmulator messages on a thread that can safely access the FakeController, which is
  // thread-hostile.
  auto cb = [this](auto impl, fidl::UnbindInfo info) {
    FDF_LOG(TRACE, "emulator channel closed (status: %s); unpublish device\n",
            zx_status_get_string(info.status()));
    UnpublishHci();
  };
  bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request), this,
                       std::move(cb));
}

zx_status_t EmulatorDevice::AddHciDeviceChildNode() {
  // Create args to add bt-hci-device as a child node on behalf of VirtualController
  zx::result connector =
      vendor_devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connecter to dispatcher: %u", connector.status_value());
    return connector.error_value();
  }

  fidl::Arena args_arena;
  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(args_arena)
                   .connector(std::move(connector.value()))
                   .connector_supports(fuchsia_device_fs::ConnectionType::kController)
                   .class_name("bt-hci")
                   .Build();
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(args_arena)
                  .name("bt-hci-device")
                  .devfs_args(devfs)
                  .Build();

  // Create the endpoints of fuchsia_driver_framework::NodeController protocol
  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create node controller endpoints failed: %s",
            zx_status_get_string(controller_endpoints.error_value()));
    return controller_endpoints.error_value();
  }

  // Create the endpoints of fuchsia_driver_framework::Node protocol for the child node, and hold
  // the client end of it, because no driver will bind to the child node.
  auto child_node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (child_node_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create child node endpoints failed: %s",
            zx_status_get_string(child_node_endpoints.error_value()));
    return child_node_endpoints.error_value();
  }

  // Add bt-hci-device as a child node of the EmulatorDevice
  ZX_DEBUG_ASSERT(emulator_child_node()->is_valid());
  auto child_result = emulator_child_node()->sync()->AddChild(
      std::move(args), std::move(controller_endpoints->server),
      std::move(child_node_endpoints->server));
  if (!child_result.ok()) {
    FDF_LOG(ERROR, "Failed to add bt-hci-device node, FIDL error: %s",
            child_result.status_string());
    return child_result.status();
  }

  if (child_result->is_error()) {
    FDF_LOG(ERROR, "Failed to add bt-hci-device node: %u",
            static_cast<uint32_t>(child_result->error_value()));
    return ZX_ERR_INTERNAL;
  }

  // |hci_child_node_| does not need to create more child nodes so we do not need an event_handler
  // and we do not need to worry about it being re-bound
  hci_child_node_.Bind(std::move(child_node_endpoints->client),
                       fdf::Dispatcher::GetCurrent()->async_dispatcher());
  hci_node_controller_.Bind(std::move(controller_endpoints->client),
                            fdf::Dispatcher::GetCurrent()->async_dispatcher());

  return ZX_OK;
}

void EmulatorDevice::EncodeSetAclPriorityCommand(
    fuchsia_hardware_bluetooth::wire::VendorSetAclPriorityParams params, void* out_buffer) {
  if (!params.has_connection_handle() || !params.has_priority() || !params.has_direction()) {
    FDF_LOG(ERROR,
            "The command cannot be encoded because the following fields are missing: %s %s %s",
            params.has_connection_handle() ? "" : "connection_handle",
            params.has_priority() ? "" : "priority", params.has_direction() ? "" : "direction");
    return;
  }
  bt_hci_broadcom::BcmSetAclPriorityCmd command = {
      .header =
          {
              .opcode = htole16(bt_hci_broadcom::kBcmSetAclPriorityCmdOpCode),
              .parameter_total_size = sizeof(bt_hci_broadcom::BcmSetAclPriorityCmd) -
                                      sizeof(bt_hci_broadcom::HciCommandHeader),
          },
      .connection_handle = htole16(params.connection_handle()),
      .priority = (params.priority() == fuchsia_hardware_bluetooth::VendorAclPriority::kNormal)
                      ? bt_hci_broadcom::kBcmAclPriorityNormal
                      : bt_hci_broadcom::kBcmAclPriorityHigh,
      .direction = (params.direction() == fuchsia_hardware_bluetooth::VendorAclDirection::kSource)
                       ? bt_hci_broadcom::kBcmAclDirectionSource
                       : bt_hci_broadcom::kBcmAclDirectionSink,
  };

  memcpy(out_buffer, &command, sizeof(command));
}

void EmulatorDevice::AddPeer(std::unique_ptr<EmulatedPeer> peer) {
  auto address = peer->address();
  peer->set_closed_callback([this, address] { peers_.erase(address); });
  peers_[address] = std::move(peer);
}

void EmulatorDevice::OnControllerParametersChanged() {
  FDF_LOG(TRACE, "HciEmulator.OnControllerParametersChanged\n");

  ftest::ControllerParameters fidl_value;
  fidl_value.local_name(fake_device_.local_name());

  const auto& device_class_bytes = fake_device_.device_class().bytes();
  uint32_t device_class = 0;
  device_class |= device_class_bytes[0];
  device_class |= static_cast<uint32_t>(device_class_bytes[1]) << 8;
  device_class |= static_cast<uint32_t>(device_class_bytes[2]) << 16;

  std::optional<fuchsia_bluetooth::DeviceClass> device_class_option =
      fuchsia_bluetooth::DeviceClass{device_class};
  fidl_value.device_class(device_class_option);

  controller_parameters_.emplace(fidl_value);
  MaybeUpdateControllerParametersChanged();
}

void EmulatorDevice::MaybeUpdateControllerParametersChanged() {
  if (!controller_parameters_.has_value() || !controller_parameters_completer_.has_value()) {
    return;
  }
  controller_parameters_completer_->Reply(std::move(controller_parameters_.value()));
  controller_parameters_.reset();
  controller_parameters_completer_.reset();
}

void EmulatorDevice::OnLegacyAdvertisingStateChanged() {
  FDF_LOG(TRACE, "HciEmulator.OnLegacyAdvertisingStateChanged\n");

  // We have requests to resolve. Construct the FIDL table for the current state.
  ftest::LegacyAdvertisingState fidl_state;
  const FakeController::LEAdvertisingState& adv_state = fake_device_.legacy_advertising_state();
  fidl_state.enabled(adv_state.enabled);

  // Populate the rest only if advertising is enabled.
  fidl_state.type(static_cast<ftest::LegacyAdvertisingType>(
      bt::hci::LowEnergyAdvertiser::AdvertisingEventPropertiesToLEAdvertisingType(
          adv_state.properties)));
  fidl_state.address_type(LeOwnAddressTypeToFidl(adv_state.own_address_type));

  if (adv_state.interval_min) {
    fidl_state.interval_min(adv_state.interval_min);
  }
  if (adv_state.interval_max) {
    fidl_state.interval_max(adv_state.interval_max);
  }

  if (adv_state.data_length) {
    std::vector<uint8_t> output(adv_state.data_length);
    bt::MutableBufferView output_view(output.data(), output.size());
    output_view.Write(adv_state.data, adv_state.data_length);
    fidl_state.advertising_data(std::move(output));
  }
  if (adv_state.scan_rsp_length) {
    std::vector<uint8_t> output(adv_state.scan_rsp_length);
    bt::MutableBufferView output_view(output.data(), output.size());
    output_view.Write(adv_state.scan_rsp_data, adv_state.scan_rsp_length);
    fidl_state.scan_response(std::move(output));
  }

  legacy_adv_states_.emplace_back(fidl_state);
  MaybeUpdateLegacyAdvertisingStates();
}

void EmulatorDevice::MaybeUpdateLegacyAdvertisingStates() {
  if (legacy_adv_states_.empty() || legacy_adv_states_completers_.empty()) {
    return;
  }
  while (!legacy_adv_states_completers_.empty()) {
    legacy_adv_states_completers_.front().Reply(legacy_adv_states_);
    legacy_adv_states_completers_.pop();
  }
  legacy_adv_states_.clear();
}

void EmulatorDevice::OnPeerConnectionStateChanged(const DeviceAddress& address,
                                                  bt::hci_spec::ConnectionHandle handle,
                                                  bool connected, bool canceled) {
  FDF_LOG(TRACE,
          "Peer connection state changed: %s (handle: %#.4x) (connected: %s) (canceled: %s):\n",
          address.ToString().c_str(), handle, (connected ? "true" : "false"),
          (canceled ? "true" : "false"));

  auto iter = peers_.find(address);
  if (iter != peers_.end()) {
    iter->second->UpdateConnectionState(connected);
  }
}

void EmulatorDevice::UnpublishHci() {
  // Unpublishing the bt-hci-device child node shuts down the associated bt-host component
  auto status = hci_node_controller_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove bt-hci-device child node: %s", status.status_string());
  }
}

bool EmulatorDevice::StartCmdChannel(zx::channel chan) {
  if (cmd_channel_.is_valid()) {
    return false;
  }

  fake_device_.SetEventFunction(fit::bind_member<&EmulatorDevice::SendEvent>(this));

  cmd_channel_ = std::move(chan);
  cmd_channel_wait_.set_object(cmd_channel_.get());
  cmd_channel_wait_.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
  zx_status_t status = cmd_channel_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (status != ZX_OK) {
    cmd_channel_.reset();
    FDF_LOG(ERROR, "failed to start command channel: %s", zx_status_get_string(status));
    return false;
  }
  return true;
}

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
  zx_status_t status = acl_channel_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (status != ZX_OK) {
    acl_channel_.reset();
    FDF_LOG(ERROR, "failed to start ACL channel: %s", zx_status_get_string(status));
    return false;
  }
  return true;
}

bool EmulatorDevice::StartIsoChannel(zx::channel chan) {
  if (iso_channel_.is_valid()) {
    return false;
  }

  // Enable FakeController to send packets to bt-host.
  fake_device_.SetReceiveIsoFunction(fit::bind_member<&EmulatorDevice::SendIsoPacket>(this));

  // Enable bt-host to send packets to FakeController.
  iso_channel_ = std::move(chan);
  iso_channel_wait_.set_object(iso_channel_.get());
  iso_channel_wait_.set_trigger(ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
  zx_status_t status = iso_channel_wait_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (status != ZX_OK) {
    iso_channel_.reset();
    FDF_LOG(ERROR, "failed to start ISO channel: %s", zx_status_get_string(status));
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

void EmulatorDevice::CloseIsoDataChannel() {
  if (iso_channel_.is_valid()) {
    iso_channel_wait_.Cancel();
    iso_channel_.reset();
  }
  fake_device_.Stop();
}

void EmulatorDevice::SendEvent(pw::span<const std::byte> buffer) {
  zx_status_t status = cmd_channel_.write(/*flags=*/0, buffer.data(), buffer.size(),
                                          /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    FDF_LOG(WARNING, "failed to write event");
  }
}

void EmulatorDevice::SendAclPacket(pw::span<const std::byte> buffer) {
  zx_status_t status = acl_channel_.write(/*flags=*/0, buffer.data(), buffer.size(),
                                          /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    FDF_LOG(WARNING, "failed to write ACL packet");
  }
}

void EmulatorDevice::SendIsoPacket(pw::span<const std::byte> buffer) {
  zx_status_t status = iso_channel_.write(/*flags=*/0, buffer.data(), buffer.size(),
                                          /*handles=*/nullptr, /*num_handles=*/0);
  if (status != ZX_OK) {
    FDF_LOG(WARNING, "failed to write ISO packet");
  }
}

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
      FDF_LOG(INFO, "command channel was closed");
    } else {
      FDF_LOG(ERROR, "failed to read on cmd channel: %s", zx_status_get_string(status));
    }
    CloseCommandChannel();
    return;
  }

  fake_device_.SendCommand(buffer);

  status = wait->Begin(dispatcher);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "failed to wait on cmd channel: %s", zx_status_get_string(status));
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
      FDF_LOG(INFO, "ACL channel was closed");
    } else {
      FDF_LOG(ERROR, "failed to read on ACL channel: %s", zx_status_get_string(status));
    }

    CloseAclDataChannel();
    return;
  }

  if (read_size < sizeof(bt::hci_spec::ACLDataHeader)) {
    FDF_LOG(ERROR, "malformed ACL packet received");
  } else {
    fake_device_.SendAclData(buffer);
  }

  status = wait->Begin(dispatcher);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "failed to wait on ACL channel: %s", zx_status_get_string(status));
    CloseAclDataChannel();
  }
}

void EmulatorDevice::HandleIsoPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                                     zx_status_t wait_status, const zx_packet_signal_t* signal) {
  std::array<std::byte, bt::hci_spec::kMaxIsochronousDataPacketPayloadSize +
                            sizeof(bt::hci_spec::IsoDataHeader)>
      buffer;
  uint32_t read_size;
  zx_status_t status = iso_channel_.read(0u, buffer.data(), /*handles=*/nullptr, buffer.size(), 0,
                                         &read_size, /*actual_handles=*/nullptr);
  ZX_DEBUG_ASSERT(status == ZX_OK || status == ZX_ERR_PEER_CLOSED);
  if (status < 0) {
    if (status == ZX_ERR_PEER_CLOSED) {
      FDF_LOG(INFO, "ISO channel was closed");
    } else {
      FDF_LOG(ERROR, "failed to read on ISO channel: %s", zx_status_get_string(status));
    }

    CloseIsoDataChannel();
    return;
  }

  if (read_size < sizeof(bt::hci_spec::IsoDataHeader)) {
    FDF_LOG(ERROR, "malformed ISO packet received");
  } else {
    fake_device_.SendIsoData(buffer);
  }

  status = wait->Begin(dispatcher);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "failed to wait on ISO channel: %s", zx_status_get_string(status));
    CloseIsoDataChannel();
  }
}

}  // namespace bt_hci_virtual
