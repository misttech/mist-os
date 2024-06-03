// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/fidl.h>
#include <fidl/fuchsia.io/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/executor.h>
#include <lib/async/cpp/wait.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/sync/cpp/completion.h>

#include "packets.h"

namespace bt_hci_broadcom {

class BtHciBroadcom final
    : public fdf::DriverBase,
      public fidl::WireAsyncEventHandler<fuchsia_driver_framework::NodeController>,
      public fidl::WireAsyncEventHandler<fuchsia_driver_framework::Node>,
      public fidl::WireServer<fuchsia_hardware_bluetooth::Hci>,
      public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor> {
 public:
  explicit BtHciBroadcom(fdf::DriverStartArgs start_args,
                         fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  void Start(fdf::StartCompleter completer) override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::Node> metadata) override {}

 private:
  static constexpr size_t kMacAddrLen = 6;

  static const std::unordered_map<uint16_t, std::string> kFirmwareMap;

  // fuchsia_hardware_bluetooth::Vendor protocol interface implementations.
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;
  void OpenHciTransport(OpenHciTransportCompleter::Sync& completer) override {}

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fuchsia_hardware_bluetooth::Hci protocol interface implementations.
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

  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);
  // Truly private, internal helper methods:
  zx_status_t ConnectToHciFidlProtocol();
  zx_status_t ConnectToSerialFidlProtocol();

  static void EncodeSetAclPriorityCommand(
      fuchsia_hardware_bluetooth::wire::VendorSetAclPriorityParams params, void* out_buffer);

  fpromise::promise<std::vector<uint8_t>, zx_status_t> SendCommand(const void* command,
                                                                   size_t length);

  // Waits for a "readable" signal from the command channel and reads the next event.
  fpromise::promise<std::vector<uint8_t>, zx_status_t> ReadEvent();

  fpromise::promise<void, zx_status_t> SetBaudRate(uint32_t baud_rate);

  fpromise::promise<void, zx_status_t> SetBdaddr(const std::array<uint8_t, kMacAddrLen>& bdaddr);

  fpromise::result<std::array<uint8_t, kMacAddrLen>, zx_status_t> GetBdaddrFromBootloader();

  fpromise::promise<> LogControllerFallbackBdaddr();

  fpromise::promise<void, zx_status_t> LoadFirmware();

  fpromise::promise<void, zx_status_t> SendVmoAsCommands(zx::vmo vmo, size_t size, size_t offset);

  fpromise::promise<void, zx_status_t> Initialize();

  fpromise::promise<void, zx_status_t> OnInitializeComplete(zx_status_t status);

  fpromise::promise<void, zx_status_t> AddNode();

  void CompleteStart(zx_status_t status);

  zx_status_t Bind();

  uint32_t serial_pid_;
  zx::channel command_channel_;
  // true if underlying transport is UART
  bool is_uart_;

  std::optional<fdf::StartCompleter> start_completer_;

  std::optional<async::Executor> executor_;

  fidl::WireClient<fuchsia_hardware_bluetooth::Hci> hci_client_;
  fdf::WireSyncClient<fuchsia_hardware_serialimpl::Device> serial_client_;

  fidl::WireClient<fuchsia_driver_framework::Node> node_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> child_node_;

  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Hci> hci_server_bindings_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Vendor> vendor_binding_group_;
  driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor> devfs_connector_;
};

}  // namespace bt_hci_broadcom

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_
