// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_EMULATOR_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_EMULATOR_H_

#include <fidl/fuchsia.bluetooth.test/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <fuchsia/hardware/bt/hci/cpp/banjo.h>
#include <fuchsia/hardware/test/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/wait.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fidl/cpp/binding.h>
#include <zircon/compiler.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <queue>
#include <unordered_map>

#include <pw_async_fuchsia/dispatcher.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/fake_controller.h"
#include "src/connectivity/bluetooth/hci/virtual/emulated_peer.h"
#include "third_party/pigweed/backends/pw_random/zircon_random_generator.h"

namespace bt_hci_virtual {

enum class Channel : uint8_t { ACL, COMMAND, EMULATOR, ISO, SNOOP };

class EmulatorDevice : public fidl::Server<fuchsia_bluetooth_test::HciEmulator>,
                       public fidl::WireServer<fuchsia_hardware_bluetooth::Hci>,
                       public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor>,
                       public fidl::WireServer<fuchsia_hardware_bluetooth::Emulator> {
 public:
  explicit EmulatorDevice(zx_device_t* device);

  zx_status_t Bind(std::string_view name);
  void Unbind();
  void Release();

  zx_status_t GetProtocol(uint32_t proto_id, void* out_proto);
  zx_status_t OpenChan(Channel chan_type, zx_handle_t chan);

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

  // fuchsia_hardware_bluetooth::Emulator overrides:
  void Open(OpenRequestView request, OpenCompleter::Sync& completer) override;

  void ClearHciDev() {
    std::lock_guard<std::mutex> lock(hci_dev_lock_);
    hci_dev_ = nullptr;
  }

 private:
  void StartEmulatorInterface(fidl::ServerEnd<fuchsia_bluetooth_test::HciEmulator> request);

  // fuchsia_bluetooth_test::HciEmulator overrides:
  void Publish(PublishRequest& request, PublishCompleter::Sync& completer) override;
  void AddLowEnergyPeer(AddLowEnergyPeerRequest& request,
                        AddLowEnergyPeerCompleter::Sync& completer) override;
  void AddBredrPeer(AddBredrPeerRequest& request, AddBredrPeerCompleter::Sync& completer) override;
  void WatchControllerParameters(WatchControllerParametersCompleter::Sync& completer) override;
  void WatchLeScanStates(WatchLeScanStatesCompleter::Sync& completer) override;
  void WatchLegacyAdvertisingStates(
      WatchLegacyAdvertisingStatesCompleter::Sync& completer) override;

  // fuchsia_hardware_bluetooth::Vendor overrides:
  void GetFeatures(GetFeaturesCompleter::Sync& completer) override;
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // Helper function for Vendor EncodeCommand
  void EncodeSetAclPriorityCommand(
      fuchsia_hardware_bluetooth::wire::BtVendorSetAclPriorityParams params, void* out_buffer);

  // Helper function used to initialize BR/EDR and LE peers.
  void AddPeer(std::unique_ptr<EmulatedPeer> peer);

  void OnControllerParametersChanged();
  void MaybeUpdateControllerParametersChanged();
  void OnLegacyAdvertisingStateChanged();
  void MaybeUpdateLegacyAdvertisingStates();

  // Remove the bt-hci device.
  void UnpublishHci();

  void OnPeerConnectionStateChanged(const bt::DeviceAddress& address,
                                    bt::hci_spec::ConnectionHandle handle, bool connected,
                                    bool canceled);

  // Starts listening for command/event packets on the given channel.
  // Returns false if already listening on a command channel
  bool StartCmdChannel(zx::channel chan);

  // Starts listening for acl packets on the given channel.
  // Returns false if already listening on a acl channel
  bool StartAclChannel(zx::channel chan);

  // Starts listening for ISO packets on the given channel.
  // Return false if already listenong on an ISO channel.
  bool StartIsoChannel(zx::channel chan);

  void CloseCommandChannel();
  void CloseAclDataChannel();
  void CloseIsoDataChannel();

  void SendEvent(pw::span<const std::byte> buffer);
  void SendAclPacket(pw::span<const std::byte> buffer);
  void SendIsoPacket(pw::span<const std::byte> buffer);

  // Read and handle packets received over the channels.
  void HandleCommandPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                           zx_status_t wait_status, const zx_packet_signal_t* signal);
  void HandleAclPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                       zx_status_t wait_status, const zx_packet_signal_t* signal);
  void HandleIsoPacket(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                       zx_status_t wait_status, const zx_packet_signal_t* signal);

  // Responsible for running the thread-hostile fake_device_, along with other members listed below.
  // Device publishes a bt-hci child, which is bound to by a bt-host child, which talks to the
  // fake_device_ over some channels. As such, |loop_| cannot be safely shut down until Device's
  // children are released, i.e. loop_ and members responsible for servicing bt-host live past
  // Unbind, and are shut down upon Release.
  async::Loop loop_;

  pw::async::fuchsia::FuchsiaDispatcher pw_dispatcher_;

  zx_device_t* const parent_;

  std::mutex hci_dev_lock_;
  // The device that implements the bt-hci protocol. |hci_dev_| will only be accessed on |loop_|,
  // and only in the following conditions:
  //   1. Initialized during Publish().
  //   2. Unpublished when the HciEmulator FIDL channel (i.e. |binding_|) gets closed, which gets
  //      processed on the |loop_| dispatcher.
  //   3. Unpublished in the DDK Unbind() call. While the Unbind method itself runs on a devhost
  //      thread, the Unpublish call is posted to |loop_| and joined upon during unbind, ensuring
  //      that |hci_dev_| is never accessed across threads.
  zx_device_t* hci_dev_ __TA_GUARDED(hci_dev_lock_);

  // The device that implements the bt-emulator protocol.
  zx_device_t* emulator_dev_;

  pw_random_zircon::ZirconRandomGenerator rng_;

  // All objects below are only accessed on the |loop_| dispatcher.
  bt::testing::FakeController fake_device_;

  // Binding for fuchsia.bluetooth.test.HciEmulator channel. |binding_| is only accessed on
  // |loop_|'s dispatcher.
  fidl::ServerBindingGroup<fuchsia_bluetooth_test::HciEmulator> bindings_;

  // List of active peers that have been registered with us.
  std::unordered_map<bt::DeviceAddress, std::unique_ptr<EmulatedPeer>> peers_;

  std::optional<fuchsia_bluetooth_test::ControllerParameters> controller_parameters_;
  std::optional<WatchControllerParametersCompleter::Async> controller_parameters_completer_;

  std::vector<fuchsia_bluetooth_test::LegacyAdvertisingState> legacy_adv_states_;
  std::queue<WatchLegacyAdvertisingStatesCompleter::Async> legacy_adv_states_completers_;

  zx::channel cmd_channel_;
  zx::channel acl_channel_;
  zx::channel iso_channel_;

  async::WaitMethod<EmulatorDevice, &EmulatorDevice::HandleCommandPacket> cmd_channel_wait_{this};
  async::WaitMethod<EmulatorDevice, &EmulatorDevice::HandleAclPacket> acl_channel_wait_{this};
  async::WaitMethod<EmulatorDevice, &EmulatorDevice::HandleIsoPacket> iso_channel_wait_{this};
};

}  // namespace bt_hci_virtual

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_EMULATOR_H_
