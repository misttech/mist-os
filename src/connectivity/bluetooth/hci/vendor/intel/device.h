// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_

#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <fuchsia/hardware/bt/hci/c/banjo.h>
#include <fuchsia/hardware/bt/hci/cpp/banjo.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <optional>

#include "fidl/fuchsia.hardware.bluetooth/cpp/markers.h"
#include "vendor_hci.h"

namespace bt_hci_intel {

class Device : public fdf::DriverBase,
               public fidl::WireAsyncEventHandler<fuchsia_driver_framework::NodeController>,
               public fidl::WireAsyncEventHandler<fuchsia_driver_framework::Node>,
               public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor>,
               public fidl::WireServer<fuchsia_hardware_bluetooth::Hci> {
 public:
  Device(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  // Load the firmware and complete device initialization.
  // If |secure| is true, use the "secure" firmware method.
  zx_status_t Init(bool secure);

  zx_status_t AddNode();

  // fdf::DriverBase overrides
  void Start(fdf::StartCompleter completer) override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {}
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::Node> metadata) override {}

  zx_status_t BtHciOpenCommandChannel(zx::channel in);
  zx_status_t BtHciOpenAclDataChannel(zx::channel in);
  zx_status_t BtHciOpenScoChannel(zx::channel in);
  void BtHciConfigureSco(sco_coding_format_t coding_format, sco_encoding_t encoding,
                         sco_sample_rate_t sample_rate, bt_hci_configure_sco_callback callback,
                         void* cookie);
  void BtHciResetSco(bt_hci_reset_sco_callback callback, void* cookie);
  zx_status_t BtHciOpenIsoDataChannel(zx::channel in);
  zx_status_t BtHciOpenSnoopChannel(zx::channel in);

 private:
  // fuchsia_hardware_bluetooth::Hci protocol interface implementations
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

  // fuchsia_hardware_bluetooth::Vendor protocol interface implementations
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;
  void OpenHciTransport(OpenHciTransportCompleter::Sync& completer) override {}
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);

  zx_status_t LoadSecureFirmware();
  zx_status_t LoadLegacyFirmware();

  // Informs the device manager that device initialization has failed,
  // which will unbind the device, and leaves an error on the kernel log
  // prepended with |note|.
  // Returns |status|.
  zx_status_t InitFailed(zx_status_t status, const char* note);

  // Maps the firmware refrenced by |name| into memory.
  // Returns the vmo that the firmware is loaded into or ZX_HANDLE_INVALID if it
  // could not be loaded.
  // Closing this handle will invalidate |fw_addr|, which
  // receives a pointer to the memory.
  // |fw_size| receives the size of the firmware if valid.
  zx_handle_t MapFirmware(const char* name, uintptr_t* fw_addr, size_t* fw_size);

  driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor> devfs_connector_;

  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Hci> hci_binding_group_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Vendor> vendor_binding_group_;

  fidl::WireClient<fuchsia_driver_framework::Node> node_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> child_node_;

  zx::channel cmd_;
  zx::channel acl_;
  ddk::BtHciProtocolClient hci_;
  bool secure_{false};
  bool firmware_loaded_{false};
  bool legacy_firmware_loading_{false};  // true to use legacy way to load firmware
};

}  // namespace bt_hci_intel

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_
