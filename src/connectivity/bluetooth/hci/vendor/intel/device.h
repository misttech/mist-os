// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_

#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <fuchsia/hardware/bt/hci/c/banjo.h>
#include <fuchsia/hardware/bt/hci/cpp/banjo.h>
#include <lib/ddk/driver.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <optional>

#include <ddktl/device.h>

#include "fidl/fuchsia.hardware.bluetooth/cpp/markers.h"
#include "vendor_hci.h"

namespace btintel {

class Device;

using DeviceType = ddk::Device<Device, ddk::Initializable, ddk::GetProtocolable, ddk::Unbindable>;

class Device : public DeviceType,
               public ddk::BtHciProtocol<Device, ddk::base_protocol>,
               public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor>,
               public fidl::WireServer<fuchsia_hardware_bluetooth::Hci> {
 public:
  Device(zx_device_t* device, bt_hci_protocol_t* hci, bool secure, bool legacy_firmware_loading);

  static zx_status_t bt_intel_bind(void* ctx, zx_device_t* device);

  // Bind the device, invisibly.
  zx_status_t Bind();

  // Load the firmware and complete device initialization.
  // if firmware is loaded, the device will be made visible.
  // otherwise the device will be removed and devhost will
  // unbind.
  // If |secure| is true, use the "secure" firmware method.
  zx_status_t LoadFirmware(ddk::InitTxn init_txn, bool secure);

  // ddk::Device methods
  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_proto);

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
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);

  zx_status_t LoadSecureFirmware(zx::channel* cmd, zx::channel* acl);
  zx_status_t LoadLegacyFirmware(zx::channel* cmd, zx::channel* acl);

  // Informs the device manager that device initialization has failed,
  // which will unbind the device, and leaves an error on the kernel log
  // prepended with |note|.
  // Returns |status|.
  zx_status_t InitFailed(ddk::InitTxn init_txn, zx_status_t status, const char* note);

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

  ddk::BtHciProtocolClient hci_;
  bool secure_;
  bool firmware_loaded_;
  bool legacy_firmware_loading_;  // true to use legacy way to load firmware
  std::thread init_thread_;
};

}  // namespace btintel

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_
