// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
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

class HciEventHandler
    : public fidl::WireSyncEventHandler<fuchsia_hardware_bluetooth::HciTransport> {
 public:
  HciEventHandler(fit::function<void(std::vector<uint8_t>&)> on_receive_callback);

  void OnReceive(fuchsia_hardware_bluetooth::wire::ReceivedPacket*) override;
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_hardware_bluetooth::HciTransport> metadata) override {}

 private:
  fit::function<void(std::vector<uint8_t>&)> on_receive_callback_;
};

class BtHciBroadcom final
    : public fdf::DriverBase,
      public fidl::WireAsyncEventHandler<fuchsia_driver_framework::NodeController>,
      public fidl::WireAsyncEventHandler<fuchsia_driver_framework::Node>,
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
  void GetFeatures(GetFeaturesCompleter::Sync& completer) override;
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;
  void OpenHciTransport(OpenHciTransportCompleter::Sync& completer) override;
  void OpenSnoop(OpenSnoopCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);
  // Truly private, internal helper methods:
  zx_status_t ConnectToHciTransportFidlProtocol();
  zx_status_t ConnectToSerialFidlProtocol();

  static void EncodeSetAclPriorityCommand(
      fuchsia_hardware_bluetooth::wire::VendorSetAclPriorityParams params, void* out_buffer);

  void OnReceivePacket(std::vector<uint8_t>& packet);
  fpromise::promise<std::vector<uint8_t>, zx_status_t> SendCommand(const void* command,
                                                                   size_t length);

  // Waits for the next event from |hci_transport_client_|'s fidl channel.
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
  // true if underlying transport is UART
  bool is_uart_;

  std::optional<fdf::StartCompleter> start_completer_;

  std::optional<async::Executor> executor_;

  std::vector<uint8_t> event_receive_buffer_;
  HciEventHandler hci_event_handler_;
  fidl::WireSyncClient<fuchsia_hardware_bluetooth::HciTransport> hci_transport_client_;
  fidl::ClientEnd<fuchsia_hardware_bluetooth::HciTransport> hci_transport_client_end_;

  fdf::WireSyncClient<fuchsia_hardware_serialimpl::Device> serial_client_;

  fidl::WireClient<fuchsia_driver_framework::Node> node_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> child_node_;

  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Vendor> vendor_binding_group_;
  driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor> devfs_connector_;
};

}  // namespace bt_hci_broadcom

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_
