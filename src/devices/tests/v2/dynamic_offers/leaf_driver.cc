// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.offers.test/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace ft = fuchsia_offers_test;

namespace {

class LeafDriver : public fdf::DriverBase {
 public:
  LeafDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("leaf", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    auto handshake = incoming()->Connect<ft::Service::Device>();
    if (handshake.is_error()) {
      return handshake.take_error();
    }
    handshake_.Bind(*std::move(handshake), dispatcher());

    auto waiter = incoming()->Connect<ft::Waiter>();
    if (waiter.is_error()) {
      return waiter.take_error();
    }
    waiter_.Bind(*std::move(waiter), dispatcher());

    AsyncCallDoThenAck();
    return zx::ok();
  }

 private:
  void AsyncCallDoThenAck() {
    auto callback = [this](fidl::Result<ft::Handshake::Do>& result) mutable {
      if (!result.is_ok()) {
        FDF_LOG(ERROR, "Handshake Do failed: %s", result.error_value().status_string());
        UnbindNode(result.error_value().status());
        return;
      }
      auto ack_result = waiter_->Ack();
      if (ack_result.is_error()) {
        FDF_LOG(ERROR, "Ack failed: %s", ack_result.error_value().status_string());
        UnbindNode(result.error_value().status());
      }
    };
    handshake_->Do().Then(std::move(callback));
  }

  void UnbindNode(const zx_status_t& status) {
    FDF_LOG(ERROR, "Failed to start leaf driver: %s", zx_status_get_string(status));
    node().reset();
  }

  fidl::SharedClient<ft::Handshake> handshake_;
  fidl::SharedClient<ft::Waiter> waiter_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(LeafDriver);
