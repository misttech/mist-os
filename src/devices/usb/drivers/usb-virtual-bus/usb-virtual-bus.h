// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_BUS_H_
#define SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_BUS_H_

#include <fidl/fuchsia.hardware.usb.virtual.bus/cpp/fidl.h>
#include <fuchsia/hardware/usb/bus/cpp/banjo.h>
#include <fuchsia/hardware/usb/dci/cpp/banjo.h>
#include <fuchsia/hardware/usb/hci/cpp/banjo.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <memory>
#include <optional>

#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-device.h"
#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-endpoint.h"
#include "src/devices/usb/drivers/usb-virtual-bus/usb-virtual-host.h"

namespace usb_virtual_bus {

class UsbVirtualDevice;
class UsbVirtualHost;

// This is the main class for the USB virtual bus.
class UsbVirtualBus : public fdf::DriverBase,
                      public fidl::Server<fuchsia_hardware_usb_virtual_bus::Bus> {
 private:
  static constexpr std::string kName = "usb-virtual-bus";

 public:
  UsbVirtualBus(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kName, std::move(start_args), std::move(dispatcher)),
        devfs_connector_(fit::bind_member<&UsbVirtualBus::Serve>(this)) {
    for (uint8_t i = 0; i < USB_MAX_EPS; i++) {
      eps_[i].Init(this, i);
    }
  }

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  void SetBusInterface(const usb_bus_interface_protocol_t* bus_intf);
  zx::result<> SetDciInterface(
      fidl::ClientEnd<fuchsia_hardware_usb_dci::UsbDciInterface> client_end);

  // Public for unit tests.
  void SetConnected(bool connected);

  UsbVirtualEp& ep(uint8_t index) { return eps_[index]; }

  void FinishConnect();
  async_dispatcher_t* async_dispatcher() { return dispatcher(); }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(UsbVirtualBus);

  friend class UsbVirtualEp;

  void Serve(fidl::ServerEnd<fuchsia_hardware_usb_virtual_bus::Bus> request);
  template <typename T>
  zx::result<std::unique_ptr<T>> CreateChild();
  template <typename T>
  zx::result<> RemoveChild(std::unique_ptr<T>& child);
  zx_status_t Disable();

  // fuchsia_hardware_usb_virtual_bus::Bus Methods
  void Enable(EnableCompleter::Sync& completer) override;
  void Disable(DisableCompleter::Sync& completer) override;
  void Connect(ConnectCompleter::Sync& completer) override;
  void Disconnect(DisconnectCompleter::Sync& completer) override;

  fdf::OwnedChildNode child_;
  driver_devfs::Connector<fuchsia_hardware_usb_virtual_bus::Bus> devfs_connector_;
  fidl::ServerBindingGroup<fuchsia_hardware_usb_virtual_bus::Bus> bindings_;

  // Reference to class that implements the virtual device controller protocol.
  std::unique_ptr<UsbVirtualDevice> device_;
  // Reference to class that implements the virtual host controller protocol.
  std::unique_ptr<UsbVirtualHost> host_;

  // Callbacks to the USB peripheral driver.
  fidl::Client<fuchsia_hardware_usb_dci::UsbDciInterface> dci_intf_;
  // Callbacks to the USB bus driver. Needs to be handled on a separate thread due
  // to differences in threading models for Banjo and FIDL.
  fdf::SynchronizedDispatcher bus_intf_dispatcher_;
  ddk::UsbBusInterfaceProtocolClient bus_intf_;

  UsbVirtualEp eps_[USB_MAX_EPS];

  enum ConnectedState : uint8_t {
    kDisconnected = 0,
    kConnecting = 1,
    kConnected = 2,
  };
  ConnectedState connected_ = ConnectedState::kDisconnected;
};

}  // namespace usb_virtual_bus

#endif  // SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_BUS_H_
