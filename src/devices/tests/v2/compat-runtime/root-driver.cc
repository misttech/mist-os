// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.compat.runtime.test/cpp/driver/fidl.h>
#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include <bind/fuchsia/test/cpp/bind.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace ft = fuchsia_compat_runtime_test;

namespace {

const std::string_view kChildName = "v1";

class RootDriver : public fdf::DriverBase, public fdf::Server<ft::Root> {
 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("root", std::move(start_args), std::move(driver_dispatcher)),
        node_(fidl::WireClient(std::move(node()), dispatcher())) {}

  static constexpr const char* Name() { return "root"; }

  zx::result<> Start() override {
    // Setup the outgoing directory.
    zx::result outgoing_result = outgoing()->AddService<ft::Service>(
        ft::Service::InstanceHandler({
            .root = bindings_.CreateHandler(this, driver_dispatcher()->get(),
                                            fidl::kIgnoreBindingClosure),
        }),
        kChildName);
    if (outgoing_result.is_error()) {
      FDF_LOG(ERROR, "Failed to add service %s", outgoing_result.status_string());
      return outgoing_result.take_error();
    }
    // Start the driver.
    auto result = AddChild();
    if (result.is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }

  // fdf::Server<ft::Root>
  void GetString(GetStringCompleter::Sync& completer) override {
    char str[100];
    strcpy(str, "hello world!");
    completer.Reply(std::string(str));
  }

 private:
  fit::result<fdf::NodeError> AddChild() {
    fidl::Arena arena;

    auto offer = fdf::MakeOffer2<ft::Service>(kChildName);

    // Set the properties of the node that a driver will bind to.
    auto property =
        fdf::MakeProperty(1 /*BIND_PROTOCOL */, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD);

    auto args = fdf::NodeAddArgs{{
        .name = std::string(kChildName),
        .properties = std::vector{std::move(property)},
        .offers2 = std::vector{std::move(offer)},
    }};

    // Create endpoints of the `NodeController` for the node.
    auto endpoints = fidl::CreateEndpoints<fdf::NodeController>();
    if (endpoints.is_error()) {
      return fit::error(fdf::NodeError::kInternal);
    }

    auto add_result = node_.sync()->AddChild(fidl::ToWire(arena, std::move(args)),
                                             std::move(endpoints->server), {});
    if (!add_result.ok()) {
      return fit::error(fdf::NodeError::kInternal);
    }
    if (add_result->is_error()) {
      return fit::error(add_result->error_value());
    }
    controller_.Bind(std::move(endpoints->client), dispatcher());
    return fit::ok();
  }

  fidl::WireClient<fdf::Node> node_;
  fidl::WireSharedClient<fdf::NodeController> controller_;

  fdf::ServerBindingGroup<ft::Root> bindings_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(RootDriver);
