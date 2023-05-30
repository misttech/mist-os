// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_LIB_USB_ENDPOINT_INCLUDE_USB_ENDPOINT_USB_ENDPOINT_H_
#define SRC_DEVICES_USB_LIB_USB_ENDPOINT_INCLUDE_USB_ENDPOINT_USB_ENDPOINT_H_

#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>

#include <usb/request-cpp.h>
#include <usb/request-fidl.h>

namespace usb_endpoint {

using RequestVariant = std::variant<usb::BorrowedRequest<void>, usb::FidlRequest>;

// UsbEndpoint is a wrapper around fidl::Server<fuchsia_hardware_usb_endpoint::Endpoint> that
// implements common functionality surrounding registering and unregistering VMOs, completing
// requests, etc.
class UsbEndpoint : public fidl::Server<fuchsia_hardware_usb_endpoint::Endpoint> {
 public:
  UsbEndpoint(const zx::bti& bti, uint8_t ep_addr) : bti_(bti), ep_addr_(ep_addr) {}

  // Connects to the UsbEndpoint server.
  void Connect(async_dispatcher_t* dispatcher,
               fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server_end);

  // fuchsia_hardware_usb_new.Endpoint protocol implementation.
  void RegisterVmos(RegisterVmosRequest& request, RegisterVmosCompleter::Sync& completer) final;
  void UnregisterVmos(UnregisterVmosRequest& request,
                      UnregisterVmosCompleter::Sync& completer) final;

  // Completes a request.
  void RequestComplete(zx_status_t status, size_t actual, RequestVariant request);

  // Gets all the iterators for a request.
  zx::result<std::vector<ddk::PhysIter>> get_iter(RequestVariant& req, size_t max_length) const;

  const zx::bti& bti() { return bti_; }
  uint8_t ep_addr() const { return ep_addr_; }

 protected:
  // OnUnbound: May be overwritten. If not overwritten, unregisters VMOs.
  virtual void OnUnbound(fidl::UnbindInfo info,
                         fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server_end);

 private:
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_usb_endpoint::Endpoint>> binding_ref_;
  const zx::bti& bti_;
  uint8_t ep_addr_;

  // completions_: Holds on to request completions that are completed, but have not been replied to
  // due to  defer_completion == true.
  std::vector<fuchsia_hardware_usb_endpoint::Completion> completions_;

  struct RegisteredVmo {
    zx_handle_t pmt;
    uint64_t* phys_list;
    size_t phys_count;
  };
  // registered_vmos_: All pre-registered VMOs registered through RegisterVmos(). Mapping from
  // vmo_id to RegisteredVmo.
  std::map<uint64_t, RegisteredVmo> registered_vmos_;
};

}  // namespace usb_endpoint

#endif  // SRC_DEVICES_USB_LIB_USB_ENDPOINT_INCLUDE_USB_ENDPOINT_USB_ENDPOINT_H_
