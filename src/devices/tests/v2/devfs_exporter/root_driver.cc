// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.devfs.test/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace ft = fuchsia_devfs_test;

namespace {

class RootDriver : public fdf::DriverBase, public fidl::WireServer<ft::Device> {
  static constexpr std::string_view name = "root-device";

 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(name, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&RootDriver::Serve>(this)) {}

  zx::result<> Start() override { return CreateDevfsNode(); }

 private:
  zx::result<> CreateDevfsNode() {
    fidl::Arena arena;
    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, name)
                    .devfs_args(devfs.Build())
                    .Build();

    // Create endpoints of the `NodeController` for the node.
    zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                  controller_endpoints.status_string());

    zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
    ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create endpoints: %s",
                  node_endpoints.status_string());

    fidl::WireResult result = fidl::WireCall(node())->AddChild(
        args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      return zx::error(result.status());
    }
    controller_.Bind(std::move(controller_endpoints->client));
    node_.Bind(std::move(node_endpoints->client));
    return zx::ok();
  }

  void Serve(fidl::ServerEnd<ft::Device> server) {
    bindings_.AddBinding(dispatcher(), std::move(server), this, fidl::kIgnoreBindingClosure);
  }

  void UnbindNode(zx_status_t status) {
    FDF_LOG(ERROR, "Failed to start root driver: %s", zx_status_get_string(status));
    node().reset();
  }

  // fidl::WireServer<ft::Device>
  void Ping(PingCompleter::Sync& completer) override { completer.Reply(); }

  fidl::ServerBindingGroup<ft::Device> bindings_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<ft::Device> devfs_connector_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(RootDriver);
