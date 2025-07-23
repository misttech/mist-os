// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_DEVICE_H_
#define SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_DEVICE_H_

#include <fidl/fuchsia.driver.framework/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.usb.dci/cpp/fidl.h>
#include <fuchsia/hardware/usb/dci/cpp/banjo.h>
#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/test/platform/cpp/bind.h>
#include <fbl/macros.h>

namespace usb_virtual_bus {

class UsbVirtualBus;

// This class implements the virtual USB device controller protocol.
class UsbVirtualDevice : public ddk::UsbDciProtocol<UsbVirtualDevice>,
                         public fidl::Server<fuchsia_hardware_usb_dci::UsbDci> {
 public:
  using Service = fuchsia_hardware_usb_dci::UsbDciService;
  static constexpr std::string kName = "usb-virtual-device";
  static std::vector<fuchsia_driver_framework::NodeProperty> GetProperties() {
    return {fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_VIRTUAL_DEVICE)};
  }

  explicit UsbVirtualDevice(UsbVirtualBus* bus) : bus_(bus) {}

  // USB device controller protocol implementation.
  void UsbDciRequestQueue(usb_request_t* usb_request,
                          const usb_request_complete_callback_t* complete_cb);
  zx_status_t UsbDciSetInterface(const usb_dci_interface_protocol_t* interface);
  zx_status_t UsbDciConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                             const usb_ss_ep_comp_descriptor_t* ss_comp_desc);
  zx_status_t UsbDciDisableEp(uint8_t ep_address);
  zx_status_t UsbDciEpSetStall(uint8_t ep_address);
  zx_status_t UsbDciEpClearStall(uint8_t ep_address);
  zx_status_t UsbDciCancelAll(uint8_t endpoint);
  size_t UsbDciGetRequestSize();

  fuchsia_hardware_usb_dci::UsbDciService::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_usb_dci::UsbDciService::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }
  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig banjo_config;
    banjo_config.callbacks[ZX_PROTOCOL_USB_DCI] = banjo_server_.callback();
    return banjo_config;
  }
  compat::SyncInitializedDeviceServer& compat_server() { return compat_server_; }
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController>& controller() {
    return controller_;
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(UsbVirtualDevice);

  // fuchsia_hardware_usb.UsbDci protocol implementation.
  void ConnectToEndpoint(ConnectToEndpointRequest& request,
                         ConnectToEndpointCompleter::Sync& completer) override;
  void SetInterface(SetInterfaceRequest& request, SetInterfaceCompleter::Sync& completer) override;
  void StartController(StartControllerCompleter::Sync& completer) override;
  void StopController(StopControllerCompleter::Sync& completer) override;
  void ConfigureEndpoint(ConfigureEndpointRequest& request,
                         ConfigureEndpointCompleter::Sync& completer) override;
  void DisableEndpoint(DisableEndpointRequest& request,
                       DisableEndpointCompleter::Sync& completer) override;
  void EndpointSetStall(EndpointSetStallRequest& request,
                        EndpointSetStallCompleter::Sync& completer) override;
  void EndpointClearStall(EndpointClearStallRequest& request,
                          EndpointClearStallCompleter::Sync& completer) override;
  void CancelAll(CancelAllRequest& request, CancelAllCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_usb_dci::UsbDci> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  UsbVirtualBus* bus_;

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  compat::SyncInitializedDeviceServer compat_server_;
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_USB_DCI, this, &usb_dci_protocol_ops_};
  fidl::ServerBindingGroup<fuchsia_hardware_usb_dci::UsbDci> bindings_;
};

}  // namespace usb_virtual_bus

#endif  // SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_DEVICE_H_
