// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_

#include <fidl/fuchsia.driver.framework/cpp/wire_messaging.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/sync/cpp/completion.h>

#include <mutex>
#include <optional>

#include "fidl/fuchsia.hardware.bluetooth/cpp/markers.h"
#include "hci_event_handler.h"
#include "vendor_hci.h"

namespace bt_hci_intel {

class Device : public fdf::DriverBase, public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor> {
 public:
  Device(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  // Load the firmware and complete device initialization.
  // If |secure| is true, use the "secure" firmware method.
  zx_status_t Init(bool secure);

  zx_status_t AddNode();

  // fdf::DriverBase overrides
  void Start(fdf::StartCompleter completer) override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

 private:
  // fuchsia_hardware_bluetooth::Vendor protocol interface implementations
  void GetFeatures(GetFeaturesCompleter::Sync &completer) override;
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync &completer) override;
  void OpenHci(OpenHciCompleter::Sync &completer) override;
  void OpenHciTransport(OpenHciTransportCompleter::Sync &completer) override;
  void OpenSnoop(OpenSnoopCompleter::Sync &completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync &completer) override;

  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);

  zx_status_t LoadSecureFirmware();
  zx_status_t LoadLegacyFirmware();

  // Informs the device manager that device initialization has failed,
  // which will unbind the device, and leaves an error on the kernel log
  // prepended with |note|.
  // Returns |status|.
  zx_status_t InitFailed(zx_status_t status, const char *note);

  // Maps the firmware refrenced by |name| into memory.
  // Returns the vmo that the firmware is loaded into or ZX_HANDLE_INVALID if it
  // could not be loaded.
  // Closing this handle will invalidate |fw_addr|, which
  // receives a pointer to the memory.
  // |fw_size| receives the size of the firmware if valid.
  zx_handle_t MapFirmware(const char *name, uintptr_t *fw_addr, size_t *fw_size);

  driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor> devfs_connector_;

  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Vendor> vendor_binding_group_;

  fidl::WireClient<fuchsia_driver_framework::NodeController> child_node_controller_;
  fidl::WireClient<fuchsia_driver_framework::Node> child_node_;

  fdf::Dispatcher hci_client_dispatcher_;
  HciEventHandler hci_event_handler_;
  fidl::SharedClient<fuchsia_hardware_bluetooth::HciTransport> hci_transport_client_;

  bool secure_{false};
  bool firmware_loaded_{false};
  bool legacy_firmware_loading_{false};  // true to use legacy way to load firmware
};

}  // namespace bt_hci_intel

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_INTEL_DEVICE_H_
