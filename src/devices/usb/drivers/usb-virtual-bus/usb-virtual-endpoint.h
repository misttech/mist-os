// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_ENDPOINT_H_
#define SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_ENDPOINT_H_

#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>

#include <usb/request-cpp.h>
#include <usb/request-fidl.h>

namespace usb_virtual_bus {

static constexpr uint8_t IN_EP_START = 17;
// For mapping b_endpoint_address value to/from index in range 0 - 31.
// OUT endpoints are in range 1 - 15, IN endpoints are in range 17 - 31.
static inline uint8_t EpAddressToIndex(uint8_t addr) {
  return static_cast<uint8_t>(((addr) & 0xF) | (((addr) & 0x80) >> 3));
}

using Request = usb::BorrowedRequest<void>;
using RequestQueue = usb::BorrowedRequestQueue<void>;
using RequestVariant = std::variant<Request, usb::FidlRequest>;

class UsbVirtualEp;
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
  void RegisterVmos(RegisterVmosRequest& request, RegisterVmosCompleter::Sync& completer) override;
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

class UsbVirtualBus;
// This struct represents an endpoint on the virtual device.
class UsbVirtualEp {
 public:
  ~UsbVirtualEp() {
    ZX_ASSERT(host_reqs.empty());
    ZX_ASSERT(device_reqs.is_empty());
  }

  void Init(UsbVirtualBus* bus, uint8_t index) {
    bus_ = bus;
    index_ = index;
  }

  void QueueRequest(RequestVariant request);
  zx::result<> CancelAll();
  void RequestComplete(zx_status_t status, size_t actual, RequestVariant request);

  bool is_control() const { return index_ == 0; }

  std::queue<RequestVariant> host_reqs;
  RequestQueue device_reqs;
  uint16_t max_packet_size = 0;
  bool stalled = false;

 private:
  UsbVirtualBus* bus_;
  uint8_t index_;
};

}  // namespace usb_virtual_bus

#endif  // SRC_DEVICES_USB_DRIVERS_USB_VIRTUAL_BUS_USB_VIRTUAL_ENDPOINT_H_
