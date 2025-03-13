// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.services.test/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace ft = fuchsia_services_test;

namespace {

class RootDriver : public fdf::DriverBase,
                   public fidl::WireServer<ft::ControlPlane>,
                   public fidl::WireServer<ft::DataPlane> {
 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("root", std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&RootDriver::Serve>(this)) {}

  zx::result<> Start() override {
    FDF_SLOG(INFO, "Starting up this here root driver");
    auto control = [this](fidl::ServerEnd<ft::ControlPlane> server_end) -> void {
      fidl::BindServer(dispatcher(), std::move(server_end), this);
    };
    auto data = [this](fidl::ServerEnd<ft::DataPlane> server_end) -> void {
      fidl::BindServer(dispatcher(), std::move(server_end), this);
    };
    ft::Device::InstanceHandler handler({.control = std::move(control), .data = std::move(data)});

    // Create a node for devfs.
    fidl::Arena arena;
    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    FDF_SLOG(INFO, "Adding args in root driver");
    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                     .connector(std::move(connector.value()))
                     .class_name("devfs_service_test");

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, name())
                    .devfs_args(devfs.Build())
                    .Build();

    // Create endpoints of the `NodeController` for the node.
    auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

    zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
    ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed: %s", node_endpoints.status_string());

    fidl::WireResult result = fidl::WireCall(node())->AddChild(
        args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      return zx::error(result.status());
    }
    FDF_SLOG(INFO, "Added child to root driver");
    controller_.Bind(std::move(controller_endpoints.client));
    node_.Bind(std::move(node_endpoints->client));

    // Normally we would add a service like this:
    // auto add_result = outgoing()->AddService<ft::Device>(std::move(handler));
    // if (add_result.is_error()) {
    //   FDF_LOG(ERROR, "Failed to add Device service: %s", result.status_string());
    //   return add_result.take_error();
    // }
    return zx::ok();
  }

 private:
  void Serve(fidl::ServerEnd<fuchsia_services_test::ControlPlane> server) {
    fidl::BindServer(dispatcher(), std::move(server), this);
  }

  // fidl::WireServer<ft::ControlPlane>
  void ControlDo(ControlDoCompleter::Sync& completer) override { completer.Reply(); }

  // fidl::WireServer<ft::DataPlane>
  void DataDo(DataDoCompleter::Sync& completer) override { completer.Reply(); }

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fuchsia_services_test::ControlPlane> devfs_connector_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(RootDriver);
