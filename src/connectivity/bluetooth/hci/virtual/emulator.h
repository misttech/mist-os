// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_EMULATOR_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_EMULATOR_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <queue>
#include <unordered_map>

#include <pw_async_fuchsia/dispatcher.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_controller.h"
#include "src/connectivity/bluetooth/hci/virtual/emulated_peer.h"
#include "third_party/pigweed/backends/pw_random/zircon_random_generator.h"

namespace bt_hci_virtual {

enum class ChannelType : uint8_t { ACL, COMMAND, EMULATOR, ISO, SNOOP };

using AddChildCallback = fit::function<void(fuchsia_driver_framework::wire::NodeAddArgs)>;
using ShutdownCallback = fit::function<void()>;

class EmulatorDevice : public fidl::WireAsyncEventHandler<fuchsia_driver_framework::NodeController>,
                       public fidl::WireAsyncEventHandler<fuchsia_driver_framework::Node>,
                       public fidl::Server<fuchsia_hardware_bluetooth::Emulator>,
                       public fidl::WireServer<fuchsia_hardware_bluetooth::Hci>,
                       public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor> {
 public:
  explicit EmulatorDevice();

  // This error handler is called when the EmulatorDevice is shut down by DFv2. We call |Shutdown()|
  // to manually delete the heap-allocated EmulatorDevice.
  void on_fidl_error(fidl::UnbindInfo error) override { Shutdown(); }

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::Node> metadata) override {}
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}

  // Methods used by the VirtualController to control the EmulatorDevice's lifecycle
  zx_status_t Initialize(std::string_view name, AddChildCallback callback,
                         ShutdownCallback shutdown);
  void Shutdown();

  zx_status_t OpenChannel(ChannelType chan_type, zx_handle_t chan);

  void set_emulator_ptr(std::unique_ptr<EmulatorDevice> ptr) { emulator_ptr_ = std::move(ptr); }

  fidl::WireClient<fuchsia_driver_framework::Node>* emulator_child_node() {
    return &emulator_child_node_;
  }
  void set_emulator_child_node(fidl::WireClient<fuchsia_driver_framework::Node> node) {
    emulator_child_node_ = std::move(node);
  }

 private:
  // fuchsia_hardware_bluetooth::Emulator overrides:
  void Publish(PublishRequest& request, PublishCompleter::Sync& completer) override;
  void AddLowEnergyPeer(AddLowEnergyPeerRequest& request,
                        AddLowEnergyPeerCompleter::Sync& completer) override;
  void AddBredrPeer(AddBredrPeerRequest& request, AddBredrPeerCompleter::Sync& completer) override;
  void WatchControllerParameters(WatchControllerParametersCompleter::Sync& completer) override;
  void WatchLeScanStates(WatchLeScanStatesCompleter::Sync& completer) override;
  void WatchLegacyAdvertisingStates(
      WatchLegacyAdvertisingStatesCompleter::Sync& completer) override;

  // fuchsia_hardware_bluetooth::Vendor overrides:
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fuchsia_hardware_bluetooth::Hci overrides:
  void OpenCommandChannel(OpenCommandChannelRequestView request,
                          OpenCommandChannelCompleter::Sync& completer) override;
  void OpenAclDataChannel(OpenAclDataChannelRequestView request,
                          OpenAclDataChannelCompleter::Sync& completer) override;
  void OpenScoDataChannel(OpenScoDataChannelRequestView request,
                          OpenScoDataChannelCompleter::Sync& completer) override;
  void ConfigureSco(ConfigureScoRequestView request,
                    ConfigureScoCompleter::Sync& completer) override;
  void ResetSco(ResetScoCompleter::Sync& completer) override;
  void OpenIsoDataChannel(OpenIsoDataChannelRequestView request,
                          OpenIsoDataChannelCompleter::Sync& completer) override;
  void OpenSnoopChannel(OpenSnoopChannelRequestView request,
                        OpenSnoopChannelCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Hci> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  void ConnectEmulator(fidl::ServerEnd<fuchsia_hardware_bluetooth::Emulator> request);
  void ConnectVendor(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);

  // Helper function for fuchsia.hardware.bluetooth.Emulator.Publish that adds bt-hci-device as a
  // child of EmulatorDevice device node
  zx_status_t AddHciDeviceChildNode();

  // Helper function for fuchsia.hardware.bluetooth.Vendor.EncodeCommand
  void EncodeSetAclPriorityCommand(
      fuchsia_hardware_bluetooth::wire::VendorSetAclPriorityParams params, void* out_buffer);

  // Helper function used to initialize BR/EDR and LE peers
  void AddPeer(std::unique_ptr<EmulatedPeer> peer);

  // Event handlers
  void OnControllerParametersChanged();
  void MaybeUpdateControllerParametersChanged();
  void OnLegacyAdvertisingStateChanged();
  void MaybeUpdateLegacyAdvertisingStates();

  // Remove bt-hci-device node
  void UnpublishHci();

  void OnPeerConnectionStateChanged(const bt::DeviceAddress& address,
                                    bt::hci_spec::ConnectionHandle handle, bool connected,
                                    bool canceled);

  // Starts listening for command/event packets on the given channel.
  // Returns false if already listening on a command channel.
  bool StartCmdChannel(zx::channel chan);

  // Starts listening for ACL packets on the given channel.
  // Returns false if already listening on a ACL channel
  bool StartAclChannel(zx::channel chan);

  // Starts listening for ISO packets on the given channel.
  // Return false if already listening on an ISO channel.
  bool StartIsoChannel(zx::channel chan);

  void CloseCommandChannel();
  void CloseAclDataChannel();
  void CloseIsoDataChannel();

  void SendEvent(pw::span<const std::byte> buffer);
  void SendAclPacket(pw::span<const std::byte> buffer);
  void SendIsoPacket(pw::span<const std::byte> buffer);

  // Read and handle packets received over the channels
  void HandleCommandPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                           zx_status_t wait_status, const zx_packet_signal_t* signal);
  void HandleAclPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                       zx_status_t wait_status, const zx_packet_signal_t* signal);
  void HandleIsoPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                       zx_status_t wait_status, const zx_packet_signal_t* signal);

  pw_random_zircon::ZirconRandomGenerator rng_;

  // Responsible for running the thread-hostile |fake_device_|
  pw::async::fuchsia::FuchsiaDispatcher pw_dispatcher_;

  bt::testing::FakeController fake_device_;

  // List of active peers that have been registered with us
  std::unordered_map<bt::DeviceAddress, std::unique_ptr<EmulatedPeer>> peers_;

  ShutdownCallback shutdown_cb_;

  std::optional<fuchsia_hardware_bluetooth::ControllerParameters> controller_parameters_;
  std::optional<WatchControllerParametersCompleter::Async> controller_parameters_completer_;

  std::vector<fuchsia_hardware_bluetooth::LegacyAdvertisingState> legacy_adv_states_;
  std::queue<WatchLegacyAdvertisingStatesCompleter::Async> legacy_adv_states_completers_;

  zx::channel cmd_channel_;
  zx::channel acl_channel_;
  zx::channel iso_channel_;

  async::WaitMethod<EmulatorDevice, &EmulatorDevice::HandleCommandPacket> cmd_channel_wait_{this};
  async::WaitMethod<EmulatorDevice, &EmulatorDevice::HandleAclPacket> acl_channel_wait_{this};
  async::WaitMethod<EmulatorDevice, &EmulatorDevice::HandleIsoPacket> iso_channel_wait_{this};

  // EmulatorDevice
  fidl::WireClient<fuchsia_driver_framework::Node> emulator_child_node_;
  driver_devfs::Connector<fuchsia_hardware_bluetooth::Emulator> emulator_devfs_connector_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Emulator> emulator_binding_group_;
  driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor> vendor_devfs_connector_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Vendor> vendor_binding_group_;
  std::unique_ptr<EmulatorDevice> emulator_ptr_;

  // bt-hci-device
  fidl::WireClient<fuchsia_driver_framework::NodeController> hci_node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> hci_child_node_;
};

}  // namespace bt_hci_virtual

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_EMULATOR_H_
