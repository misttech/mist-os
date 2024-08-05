// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_USB_DCI_INTERFACE_SERVER_H_
#define SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_USB_DCI_INTERFACE_SERVER_H_

#include <fidl/fuchsia.hardware.usb.dci/cpp/wire.h>
#include <lib/async/dispatcher.h>

namespace usb_peripheral {

class UsbPeripheral;
class UsbDciInterfaceServer : public fidl::WireServer<fuchsia_hardware_usb_dci::UsbDciInterface> {
 public:
  explicit UsbDciInterfaceServer(UsbPeripheral* drv) : drv_{drv} {}

  // fuchsia_hardware_usb_dci::UsbDciInterface protocol implementation.
  void Control(ControlRequestView req, ControlCompleter::Sync& completer) override;
  void SetConnected(SetConnectedRequestView req, SetConnectedCompleter::Sync& completer) override;
  void SetSpeed(SetSpeedRequestView req, SetSpeedCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_usb_dci::UsbDciInterface> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fidl::ServerBindingGroup<fuchsia_hardware_usb_dci::UsbDciInterface>& bindings() {
    return bindings_;
  }

 private:
  // Pointer to UsbPeripheral driver. This class must not outlive the driver instance.
  UsbPeripheral* drv_;

  fidl::ServerBindingGroup<fuchsia_hardware_usb_dci::UsbDciInterface> bindings_;
};

}  // namespace usb_peripheral

#endif  // SRC_DEVICES_USB_DRIVERS_USB_PERIPHERAL_USB_DCI_INTERFACE_SERVER_H_
