// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_HOST_H_
#define SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_HOST_H_

#include <fidl/fuchsia.hardware.usb.hci/cpp/fidl.h>
#include <fuchsia/hardware/usb/hci/cpp/banjo.h>
#include <lib/ddk/device.h>

#include <ddktl/device.h>
#include <fbl/macros.h>
#include <usb/descriptors.h>
#include <usb/request-fidl.h>

namespace usb_virtual_bus {

class UsbVirtualBus;
class UsbVirtualHost;
using UsbVirtualHostType = ddk::Device<UsbVirtualHost>;

class UsbVirtualEp;

// This class implements the virtual USB host controller protocol.
class UsbVirtualHost : public UsbVirtualHostType,
                       public ddk::UsbHciProtocol<UsbVirtualHost, ddk::base_protocol>,
                       public fidl::Server<fuchsia_hardware_usb_hci::UsbHci> {
 public:
  ~UsbVirtualHost();
  class UsbEpServer : public fidl::Server<fuchsia_hardware_usb_endpoint::Endpoint> {
   public:
    UsbEpServer(UsbVirtualEp* ep, async_dispatcher_t* dispatcher,
                fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server)
        : ep_(ep),
          binding_(dispatcher, std::move(server), this,
                   fit::bind_member(this, &UsbEpServer::OnFidlClosed)) {}
    void OnFidlClosed(fidl::UnbindInfo);

    // fuchsia_hardware_usb_new.Endpoint protocol implementation.
    void GetInfo(GetInfoCompleter::Sync& completer) override {
      completer.Reply(fit::as_error(ZX_ERR_NOT_SUPPORTED));
    }
    void RegisterVmos(RegisterVmosRequest& request,
                      RegisterVmosCompleter::Sync& completer) override;
    void UnregisterVmos(UnregisterVmosRequest& request,
                        UnregisterVmosCompleter::Sync& completer) override;
    void QueueRequests(QueueRequestsRequest& request,
                       QueueRequestsCompleter::Sync& completer) override;
    void CancelAll(CancelAllCompleter::Sync& completer) override;

    void RequestComplete(zx_status_t status, size_t actual, usb::FidlRequest request);
    zx::result<std::optional<usb::MappedVmo>> GetMapped(
        const fuchsia_hardware_usb_request::Buffer& buffer) {
      if (buffer.Which() == fuchsia_hardware_usb_request::Buffer::Tag::kData) {
        return zx::ok(std::nullopt);
      }
      return zx::ok(registered_vmos_.at(buffer.vmo_id().value()));
    }
    usb::MappedVmo& registered_vmo(uint64_t i) { return registered_vmos_[i]; }

   private:
    UsbVirtualEp* ep_;
    // binding_ must be created, used, and destroyed on bus_->device_dispatcher_. There is no check
    // for this, but `RequestComplete` in this class will only be called when it supports FIDL and
    // that will happen on device_dispatcher_. When we've moved away from Banjo, it maybe worth
    // putting this in async_patterns::DispatcherBound.
    fidl::ServerBinding<fuchsia_hardware_usb_endpoint::Endpoint> binding_;

    // completions_: Holds on to request completions that are completed, but have not been replied
    // to due to  defer_completion == true.
    std::vector<fuchsia_hardware_usb_endpoint::Completion> completions_;

    // registered_vmos_: All pre-registered VMOs registered through RegisterVmos(). Mapping from
    // vmo_id to usb::MappedVmo.
    std::map<uint64_t, usb::MappedVmo> registered_vmos_;
  };

  explicit UsbVirtualHost(zx_device_t* parent, UsbVirtualBus* bus)
      : UsbVirtualHostType(parent), bus_(bus) {}

  // Device protocol implementation.
  void DdkRelease();

  // USB host controller protocol implementation.
  void UsbHciRequestQueue(usb_request_t* usb_request,
                          const usb_request_complete_callback_t* complete_cb);
  void UsbHciSetBusInterface(const usb_bus_interface_protocol_t* bus_intf);
  size_t UsbHciGetMaxDeviceCount();
  zx_status_t UsbHciEnableEndpoint(uint32_t device_id, const usb_endpoint_descriptor_t* ep_desc,
                                   const usb_ss_ep_comp_descriptor_t* ss_com_desc, bool enable);
  uint64_t UsbHciGetCurrentFrame();
  zx_status_t UsbHciConfigureHub(uint32_t device_id, usb_speed_t speed,
                                 const usb_hub_descriptor_t* desc, bool multi_tt);
  zx_status_t UsbHciHubDeviceAdded(uint32_t device_id, uint32_t port, usb_speed_t speed);
  zx_status_t UsbHciHubDeviceRemoved(uint32_t device_id, uint32_t port);
  zx_status_t UsbHciHubDeviceReset(uint32_t device_id, uint32_t port);
  zx_status_t UsbHciResetEndpoint(uint32_t device_id, uint8_t ep_address);
  zx_status_t UsbHciResetDevice(uint32_t hub_address, uint32_t device_id);
  size_t UsbHciGetMaxTransferSize(uint32_t device_id, uint8_t ep_address);
  zx_status_t UsbHciCancelAll(uint32_t device_id, uint8_t ep_address);
  size_t UsbHciGetRequestSize();

  // fuchsia_hardware_usb_hci.UsbHci protocol implementation.
  void ConnectToEndpoint(ConnectToEndpointRequest& request,
                         ConnectToEndpointCompleter::Sync& completer) override;

  UsbEpServer* ep(uint8_t index) {
    ZX_ASSERT(eps_[index].has_value());
    return &*eps_[index];
  }

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(UsbVirtualHost);

  UsbVirtualBus* bus_;
  std::optional<UsbEpServer> eps_[USB_MAX_EPS];
};

}  // namespace usb_virtual_bus

#endif  // SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_HOST_H_
