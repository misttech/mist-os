// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/test.wlan.testcontroller/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/channel.h>

namespace wlan_testcontroller {

class TestController : public fdf::DriverBase,
                       public fidl::WireServer<test_wlan_testcontroller::TestController> {
  static constexpr std::string_view kDriverName = "wlan_testcontroller";

 public:
  TestController(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure)) {
  }

  zx::result<> Start() override {
    node_.Bind(std::move(node()));
    FDF_LOG(INFO, "TestController driver starting up...");

    fidl::Arena arena;

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    // By calling AddChild with devfs_args, the child driver will be discoverable through devfs.
    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, kDriverName)
                    .devfs_args(devfs.Build())
                    .Build();

    zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    ZX_ASSERT(controller_endpoints.is_ok());

    auto result = node_->AddChild(args, std::move(controller_endpoints->server), {});
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
      return zx::error(result.status());
    }

    return zx::ok();
  }

  void CreateFullmac(CreateFullmacRequestView request,
                     CreateFullmacCompleter::Sync& completer) override {
    FDF_LOG(INFO, "CreateFullmac");
    completer.ReplySuccess();
  }

 private:
  // The node client. This lets TestController and related classes add child nodes, which is the
  // DFv2 equivalent of calling device_add().
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;

  // Holds bindings to TestController, which all bind to this class
  fidl::ServerBindingGroup<test_wlan_testcontroller::TestController> bindings_;

  // devfs_connector_ lets the class serve the TestController protocol over devfs.
  driver_devfs::Connector<test_wlan_testcontroller::TestController> devfs_connector_;
};

}  // namespace wlan_testcontroller

FUCHSIA_DRIVER_EXPORT(wlan_testcontroller::TestController);
