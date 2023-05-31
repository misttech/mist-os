// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_LIB_USB_ENDPOINT_INCLUDE_USB_ENDPOINT_USB_ENDPOINT_CLIENT_H_
#define SRC_DEVICES_USB_LIB_USB_ENDPOINT_INCLUDE_USB_ENDPOINT_USB_ENDPOINT_CLIENT_H_

#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>
#include <lib/ddk/debug.h>

#include <queue>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <usb/request-fidl.h>

namespace usb_endpoint {

namespace internal {

// UsbEndpointBase is self contained helps manage common functionality for the client side of
// `fuchsia_hardware_usb_endpoint::Endpoint` without needing any references outside. UsbEndpointBase
// should only be inherited by UsbEndpoint and should not be used independently. One of the largest
// uses of UsbEndpointBase is managing mapped VMOs.
class UsbEndpointBase {
 public:
  explicit UsbEndpointBase(usb::EndpointType ep_type) : ep_type_(ep_type) {}
  // Upon destruction, UsbEndpointBase ensures that all allocated requests have been freed and
  // unmaps VMOs.
  ~UsbEndpointBase();

  // Only allow access to client_. Generally this should only be used to call GetInfo,
  // QueueRequests, and CancelAll, where RegisterVmos and UnregisterVmos will be called accordingly
  // by AddRequests and DeleteRequest.
  fidl::SharedClient<fuchsia_hardware_usb_endpoint::Endpoint>& operator->() { return client_; }

  // Helper functions that manage access to the request pool. Buffer regions of a request will be
  // mapped upon addition to the pool. If mapping upon addition is not desired, one may use
  // fuchsia_hardware_usb_request::Buffer::Tag::kData types or manage its own requests (i.e. not
  // using a pool). Note that all functions specified in UsbEndpoint expect that the requests have
  // been previously mapped and the mapped addresses are saved and managed by UsbEndpoint.
  size_t AddRequests(size_t req_count, size_t size, fuchsia_hardware_usb_request::Buffer::Tag type);
  std::optional<usb::FidlRequest> GetRequest() { return free_reqs_.Get(); }
  void PutRequest(usb::FidlRequest&& request) { free_reqs_.Put(std::move(request)); }
  bool RequestsFull() { return free_reqs_.Full(); }
  bool RequestsEmpty() { return free_reqs_.Empty(); }
  // Helper function that deletes a request from the pool. If this function is not called when
  // deleting a request from the pool, it will stay mapped (and registered) until the endpoint is
  // destructed.
  zx_status_t DeleteRequest(usb::FidlRequest&& request) __TA_REQUIRES(mutex_);

  fbl::Mutex mutex_;
  const std::function<zx::result<std::optional<usb::MappedVmo>>(
      const fuchsia_hardware_usb_request::Buffer& buffer)>
      GetMapped = [&](const fuchsia_hardware_usb_request::Buffer& buffer) {
        fbl::AutoLock _(&mutex_);
        return get_mapped(buffer);
      };
  const std::function<zx::result<std::optional<usb::MappedVmo>>(
      const fuchsia_hardware_usb_request::Buffer& buffer)>
      GetMappedLocked = [&](const fuchsia_hardware_usb_request::Buffer& buffer)
                            __TA_REQUIRES(mutex_) { return get_mapped(buffer); };
  std::optional<zx_vaddr_t> GetMappedAddr(const fuchsia_hardware_usb_request::Request& request,
                                          size_t idx) {
    fbl::AutoLock _(&mutex_);
    auto mapped = get_mapped(*request.data()->at(idx).buffer());
    return mapped.is_ok() ? std::make_optional<zx_vaddr_t>(mapped->addr) : std::nullopt;
  }

 protected:
  // client_: protected so UsbEndpoint can access it in Init()
  fidl::SharedClient<fuchsia_hardware_usb_endpoint::Endpoint> client_;

 private:
  // Registers vmo_count VMOs with size vmo_size. Maps these VMOs and inserts corresponding requests
  // into the free_reqs_ pool. Returns the number of VMOs successfully registered. Called by
  // AddRequests.
  size_t RegisterVmos(size_t vmo_count, size_t vmo_size);

  // Unmaps a buffer region.
  zx_status_t Unmap(const fuchsia_hardware_usb_request::BufferRegion& buffer) __TA_REQUIRES(mutex_);

  // Gets mapped address
  zx::result<std::optional<usb::MappedVmo>> get_mapped(
      const fuchsia_hardware_usb_request::Buffer& buffer) __TA_REQUIRES(mutex_);

  const usb::EndpointType ep_type_;

  // free_reqs_: Free request pool with buffer field filled out for VMO and VMO_IDs. Other fields
  // should be taken as uninitialized and may contain remnants of its previous lifetime.
  usb::FidlRequestPool free_reqs_;

  // vmo_mapped_addrs_: maps buffer_id or VMO handle to mapped virtual address.
  std::map<uint64_t, usb::MappedVmo> vmo_mapped_addrs_ __TA_GUARDED(mutex_);

  // Internal buffer_id counter used to produce unique buffer_ids for `RegisterVmos`.
  std::atomic_uint32_t buffer_id_ = 0;
};

}  // namespace internal

// UsbEndpoint helps manage common functionality for the client side of
// `fuchsia_hardware_usb_endpoint::Endpoint`. Most notably, UsbEndpoint binds a client to make calls
// such as `QueueRequest` and `RegisterVmos` and implements the corresponding
// `fidl::AsyncEventHandler<fuchsia_hardware_usb_endpoint::Endpoint>` required to handle
// `OnCompletion` events. UsbEndpoint is templated on `DeviceType` which should have a `void
// (fuchsia_hardware_usb_endpoint::Completion)` function, which will be called for each completion
// event received. All other common functionality implemented by UsbEndpoint are described in detail
// in the `UsbEndpointBase` class, which UsbEndpoint inherits from.
//
// Example Usage:
//   class SampleDeviceType {
//    public:
//    private:
//     void RequestComplete(fuchsia_hardware_usb_endpoint::Completion completion);
//
//     usb_endpoint::UsbEndpoint<SampleDeviceType> ep_{usb_endpoint::EndpointType::BULK, this,
//                                                     std::mem_fn(&SampleDeviceType::RequestComplete)};
//   };
template <class DeviceType>
class UsbEndpoint : public internal::UsbEndpointBase,
                    public fidl::AsyncEventHandler<fuchsia_hardware_usb_endpoint::Endpoint> {
 public:
  using OnCompletionFuncType =
      std::__mem_fn<void (DeviceType::*)(fuchsia_hardware_usb_endpoint::Completion)>;

  UsbEndpoint(usb::EndpointType ep_type, DeviceType* device, OnCompletionFuncType on_completion)
      : internal::UsbEndpointBase(ep_type), device_(device), on_completion_(on_completion) {}

  // Init is templated on `ProtocolType`, which declares `ConnectToEndpoint(uint8_t ep_addr,
  // fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint>)`--either `fuchsia_hardware_usb::Usb`
  // or `fuchsia_hardware_usb_function::UsbFunction`. Init creates a connection between the server
  // side endpoint and binds the client side to this.
  template <typename ProtocolType>
  zx_status_t Init(uint8_t ep_addr, fidl::ClientEnd<ProtocolType>& client,
                   async_dispatcher_t* dispatcher);

  // fidl::AsyncEventHandler implementation.
  // OnCompletion: handles completed requests by calling on_completion_ for each request completed.
  void OnCompletion(
      fidl::Event<fuchsia_hardware_usb_endpoint::Endpoint::OnCompletion>& event) override {
    for (auto& completion : event.completion()) {
      on_completion_(device_, std::move(completion));
    }
  }

  void on_fidl_error(fidl::UnbindInfo error) override {
    zxlogf(ERROR, "on_fidl_error: %s", error.FormatDescription().c_str());
  }

 private:
  // device_: pointer to device implementing on_completion_. Should not and will not outlive
  // UsbEndpoint if UsbEndpoint is declared as a member of device_ as in the example above.
  DeviceType* device_;
  // on_completion_: member function of device_ that is called for each request completed.
  OnCompletionFuncType on_completion_;
};

template <class DeviceType>
template <typename ProtocolType>
zx_status_t UsbEndpoint<DeviceType>::Init(uint8_t ep_addr, fidl::ClientEnd<ProtocolType>& client,
                                          async_dispatcher_t* dispatcher) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_usb_endpoint::Endpoint>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Creating endpoint error: %s", zx_status_get_string(endpoints.status_value()));
    return endpoints.status_value();
  }
  auto result = fidl::Call(client)->ConnectToEndpoint({ep_addr, std::move(endpoints->server)});
  if (result.is_error()) {
    zxlogf(ERROR, "ConnectToEndpoint failed =: %s",
           result.error_value().FormatDescription().c_str());
    return result.error_value().domain_error();
  }
  client_.Bind(std::move(endpoints->client), dispatcher, this);
  if (!client_.is_valid()) {
    zxlogf(ERROR, "Could not bind to endpoint!");
    return ZX_ERR_CONNECTION_REFUSED;
  }

  return ZX_OK;
}

}  // namespace usb_endpoint

#endif  // SRC_DEVICES_USB_LIB_USB_ENDPOINT_INCLUDE_USB_ENDPOINT_USB_ENDPOINT_CLIENT_H_
