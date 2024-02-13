// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/device.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/test/cpp/bind.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace {

class RootDriver : public fdf::DriverBase {
 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("root", std::move(start_args), std::move(driver_dispatcher)) {}

  void Start(fdf::StartCompleter completer) override {
    node_.Bind(std::move(node()), dispatcher());
    start_completer_.emplace(std::move(completer));
    child_.Begin(incoming(), outgoing(), node_name(), "v1",
                 fit::bind_member<&RootDriver::CompatServerInitialized>(this),
                 compat::ForwardMetadata::None(), get_banjo_config());
  }

  void CompleteStart(zx::result<> result) {
    ZX_ASSERT(start_completer_.has_value());
    start_completer_.value()(result);
    start_completer_.reset();
  }

  void CompatServerInitialized(zx::result<> compat_result) {
    if (compat_result.is_error()) {
      return CompleteStart(compat_result.take_error());
    }

    // Set the properties of the node that a driver will bind to.
    fdf::NodeProperty property =
        fdf::MakeProperty(1 /* BIND_PROTOCOL */, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD);

    auto offers = child_.CreateOffers2();

    fdf::NodeAddArgs args({.name = "v1", .properties = {{property}}, .offers2 = offers});

    // Create endpoints of the `NodeController` for the node.
    auto endpoints = fidl::CreateEndpoints<fdf::NodeController>();
    if (endpoints.is_error()) {
      return CompleteStart(endpoints.take_error());
    }

    node_->AddChild({std::move(args), std::move(endpoints->server), {}})
        .Then([this, client = std::move(endpoints->client)](
                  fidl::Result<fdf::Node::AddChild>& add_result) mutable {
          if (add_result.is_error()) {
            FDF_LOG(ERROR, "Failed to AddChild: %s",
                    add_result.error_value().FormatDescription().c_str());
            node_.AsyncTeardown();
            return;
          }

          controller_.Bind(std::move(client), dispatcher());
          CompleteStart(zx::ok());
        });
  }

 private:
  compat::DeviceServer::BanjoConfig get_banjo_config() {
    compat::DeviceServer::BanjoConfig config{0};
    config.callbacks[0] = []() {
      return compat::DeviceServer::GenericProtocol{.ops = nullptr, .ctx = nullptr};
    };
    return config;
  }

  fidl::SharedClient<fdf::Node> node_;
  fidl::SharedClient<fdf::NodeController> controller_;

  std::optional<fdf::StartCompleter> start_completer_;

  compat::AsyncInitializedDeviceServer child_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(RootDriver);
