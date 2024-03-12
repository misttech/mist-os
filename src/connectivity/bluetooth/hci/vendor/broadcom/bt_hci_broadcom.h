// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_

#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/executor.h>
#include <lib/async/cpp/wait.h>

#include <ddktl/device.h>

#include "packets.h"

namespace bt_hci_broadcom {

class BtHciBroadcom;

using BtHciBroadcomType = ddk::Device<BtHciBroadcom, ddk::Initializable, ddk::Unbindable,
                                      ddk::Messageable<fuchsia_hardware_bluetooth::Vendor>::Mixin>;

class BtHciBroadcom : public BtHciBroadcomType,
                      public fidl::WireServer<fuchsia_hardware_bluetooth::Hci> {
 public:
  // |dispatcher| will be used for the initialization thread if non-null.
  explicit BtHciBroadcom(zx_device_t* parent, async_dispatcher_t* dispatcher);

  // Static bind function for the ZIRCON_DRIVER() declaration. Binds this device and passes
  // ownership to the driver manager.
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Static bind function that accepts a dispatcher for the initialization thread. This is useful
  // for testing.
  static zx_status_t Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher);

  // DDK mixins:
  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

 private:
  static constexpr size_t kMacAddrLen = 6;

  static const std::unordered_map<uint16_t, std::string> kFirmwareMap;

  // fuchsia_hardware_bluetooth::Vendor protocol interface implementations.
  void GetFeatures(GetFeaturesCompleter::Sync& completer) override;
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;

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

  // Truly private, internal helper methods:
  zx_status_t ConnectToHciFidlProtocol();
  zx_status_t ConnectToSerialFidlProtocol();

  static void EncodeSetAclPriorityCommand(
      fuchsia_hardware_bluetooth::wire::BtVendorSetAclPriorityParams params, void* out_buffer);

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

  fpromise::promise<> Initialize();

  void OnInitializeComplete(zx_status_t status);

  zx_status_t Bind();

  uint32_t serial_pid_;
  zx::channel command_channel_;
  // true if underlying transport is UART
  bool is_uart_;
  std::optional<ddk::InitTxn> init_txn_;

  // Only present in production. Created during initialization.
  std::optional<async::Loop> loop_;
  // In production, this is |loop_|'s dispatcher. In tests, this is the test dispatcher.
  async_dispatcher_t* dispatcher_;
  // The executor for |dispatcher_|, created during initialization.
  std::optional<async::Executor> executor_;

  fidl::WireClient<fuchsia_hardware_bluetooth::Hci> hci_client_;
  fdf::WireSyncClient<fuchsia_hardware_serialimpl::Device> serial_client_;
};

}  // namespace bt_hci_broadcom

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_BT_HCI_BROADCOM_H_
