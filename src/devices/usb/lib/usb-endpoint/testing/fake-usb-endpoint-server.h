// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_LIB_USB_ENDPOINT_TESTING_FAKE_USB_ENDPOINT_SERVER_H_
#define SRC_DEVICES_USB_LIB_USB_ENDPOINT_TESTING_FAKE_USB_ENDPOINT_SERVER_H_

#include <fidl/fuchsia.hardware.usb.endpoint/cpp/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <queue>

#include <fbl/auto_lock.h>
#include <zxtest/zxtest.h>

#include "zircon/system/ulib/async-default/include/lib/async/default.h"

namespace fake_usb_endpoint {

// FakeEndpoint generally should not be used unless accessed from FakeUsbFidlProvider, but may be
// overridden for specific use-cases.
class FakeEndpoint : public fidl::Server<fuchsia_hardware_usb_endpoint::Endpoint> {
 public:
  ~FakeEndpoint() {
    EXPECT_TRUE(expected_get_info_.empty());
    EXPECT_TRUE(requests_.empty());
    EXPECT_TRUE(completions_.empty());
  }

  virtual void Connect(async_dispatcher_t* dispatcher,
                       fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server) {
    binding_ref_.emplace(fidl::BindServer(dispatcher, std::move(server), this));
  }

  // RequestComplete: responds to the next request. If there are any requests in the request queue,
  // respond to that. If not, save this response and respond with the next incoming request.
  void RequestComplete(zx_status_t status, size_t actual) {
    fbl::AutoLock _(&lock_);
    auto completion = RequestCompleteLocked(status, actual);
    if (completion.has_value()) {
      ASSERT_TRUE(binding_ref_);
      std::vector<fuchsia_hardware_usb_endpoint::Completion> completions;
      completions.emplace_back(std::move(completion.value()));
      EXPECT_TRUE(fidl::SendEvent(*binding_ref_)->OnCompletion(std::move(completions)).is_ok());
    }
  }

  // GetInfo: responds according to previous calls of ExpectGetInfo() and returns
  //  * error status: if previous call of ExpectedGetInfo() indicated that the status to return is
  //                  not ZX_OK
  //  * info: if previous call of ExpectedGetInfo() indicated that the status to return is ZX_OK,
  //          returns the info from ExpectedGetInfo()
  void GetInfo(GetInfoCompleter::Sync& completer) override {
    EXPECT_FALSE(expected_get_info_.empty());
    if (expected_get_info_.front().first != ZX_OK) {
      completer.Reply(fit::as_error(expected_get_info_.front().first));
      expected_get_info_.pop();
      return;
    }

    completer.Reply(fit::ok(std::move(expected_get_info_.front().second)));
    expected_get_info_.pop();
  }
  // QueueRequests: adds requests to a queue, which will be replied to when RequestComplete() is
  // called or if there is already a completion saved from before.
  void QueueRequests(QueueRequestsRequest& request,
                     QueueRequestsCompleter::Sync& completer) override {
    fbl::AutoLock _(&lock_);
    // Add request to queue.
    requests_.insert(requests_.end(), std::make_move_iterator(request.req().begin()),
                     std::make_move_iterator(request.req().end()));

    // Reply if there is a completion saved for it already.
    std::vector<fuchsia_hardware_usb_endpoint::Completion> completions;
    while (!completions_.empty()) {
      auto completion =
          RequestCompleteLocked(completions_.front().first, completions_.front().second);
      if (!completion.has_value()) {
        break;
      }
      completions_.pop();
      completions.emplace_back(std::move(completion.value()));
    }
    if (completions.empty()) {
      return;
    }
    ASSERT_TRUE(binding_ref_);
    EXPECT_TRUE(fidl::SendEvent(*binding_ref_)->OnCompletion(std::move(completions)).is_ok());
  }
  // CancelAll: succeeds without checking anything.
  void CancelAll(CancelAllCompleter::Sync& completer) override { completer.Reply(fit::ok()); }
  // RegisterVmos: succeeds without checking anything.
  void RegisterVmos(RegisterVmosRequest& request, RegisterVmosCompleter::Sync& completer) override {
    std::vector<fuchsia_hardware_usb_endpoint::VmoHandle> ret;
    for (const auto& vmo_id : request.vmo_ids()) {
      zx::vmo vmo;
      auto status = zx::vmo::create(*vmo_id.size(), 0, &vmo);
      if (status != ZX_OK) {
        continue;
      }
      ret.emplace_back(std::move(
          fuchsia_hardware_usb_endpoint::VmoHandle().id(*vmo_id.id()).vmo(std::move(vmo))));
    }
    completer.Reply(std::move(ret));
  }
  // UnregisterVmos: succeeds without checking anything.
  void UnregisterVmos(UnregisterVmosRequest& request,
                      UnregisterVmosCompleter::Sync& completer) override {
    completer.Reply({{}, {}});
  }

  // ExpectGetInfo
  //  * status: status to return on GetInfo()
  //  * info: if status is ZX_OK, return this info.
  virtual void ExpectGetInfo(zx_status_t status, fuchsia_hardware_usb_endpoint::EndpointInfo info) {
    expected_get_info_.emplace(status, std::move(info));
  }

 private:
  std::optional<fuchsia_hardware_usb_endpoint::Completion> RequestCompleteLocked(zx_status_t status,
                                                                                 size_t actual)
      __TA_REQUIRES(lock_) {
    if (requests_.empty()) {
      // Save completion for next incoming request.
      completions_.emplace(status, actual);
      return std::nullopt;
    }

    // Respond to the next request in the queue.
    auto completion = std::move(fuchsia_hardware_usb_endpoint::Completion()
                                    .request(std::move(requests_.front()))
                                    .status(status)
                                    .transfer_size(actual));
    requests_.erase(requests_.begin());
    return std::move(completion);
  }

  std::optional<fidl::ServerBindingRef<fuchsia_hardware_usb_endpoint::Endpoint>> binding_ref_;

  fbl::Mutex lock_;
  std::queue<std::pair<zx_status_t, fuchsia_hardware_usb_endpoint::EndpointInfo>>
      expected_get_info_;
  std::vector<fuchsia_hardware_usb_request::Request> requests_ __TA_GUARDED(lock_);
  std::queue<std::pair<zx_status_t, size_t>> completions_ __TA_GUARDED(lock_);
};

// FakeUsbFidlProvider is, as its name suggests, a fake USB FIDL server for testing.
//
// ProtocolType must be one of fuchsia_hardware_usb_dci::UsbDci,
// fuchsia_usb_hardware_function::UsbFunction, or fuchsia_hardware_usb::Usb. In other words,
// ProtocolType is expected to have one function to override--void
// ConnectToEndpoint(ConnectToEndpointRequest& request, ConnectToEndpointCompleter::Sync&
// completer).
//
// fuchsia_hardware_usb_hci::UsbHci may also use this fake USB FIDL server, but will
// have to override the ConnectToEndpoint and write a new ExpectConnectToEndpoint method to
// accommodate device_id.
//
// It provides connections to several FakeEndpoints as requested. FakeEndpointType must be
// FakeEndpoint or an inherited class of FakeEndpoint, defaulting to FakeEndpoint if not
// specified.
template <typename ProtocolType, typename FakeEndpointType = FakeEndpoint>
class FakeUsbFidlProvider : public fidl::Server<ProtocolType> {
 public:
  explicit FakeUsbFidlProvider(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}
  ~FakeUsbFidlProvider() { EXPECT_TRUE(expected_connect_to_endpoint_.empty()); }

  virtual void ExpectConnectToEndpoint(uint8_t ep_addr) {
    expected_connect_to_endpoint_.push(ep_addr);
  }

  FakeEndpointType& fake_endpoint(uint8_t ep_addr) { return fake_endpoints_[ep_addr]; }

 private:
  void ConnectToEndpoint(
      fidl::Request<typename ProtocolType::ConnectToEndpoint>& request,
      typename fidl::internal::NaturalCompleter<typename ProtocolType::ConnectToEndpoint>::Sync&
          completer) override {
    EXPECT_FALSE(expected_connect_to_endpoint_.empty());

    auto expected = expected_connect_to_endpoint_.front();
    expected_connect_to_endpoint_.pop();
    EXPECT_EQ(expected, request.ep_addr());

    fake_endpoints_[expected].Connect(dispatcher_, std::move(request.ep()));
    completer.Reply(fit::ok());
  }

  async_dispatcher_t* dispatcher_;

  std::queue<uint8_t> expected_connect_to_endpoint_;

  std::map<uint8_t, FakeEndpointType> fake_endpoints_;
};

}  // namespace fake_usb_endpoint

#endif  // SRC_DEVICES_USB_LIB_USB_ENDPOINT_TESTING_FAKE_USB_ENDPOINT_SERVER_H_
