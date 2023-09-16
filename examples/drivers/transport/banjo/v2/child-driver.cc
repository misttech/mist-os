// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.fs/cpp/fidl.h>
#include <fuchsia/examples/gizmo/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace banjo_transport {

class ChildBanjoTransportDriver : public fdf::DriverBase {
 public:
  ChildBanjoTransportDriver(fdf::DriverStartArgs start_args,
                            fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("transport-child", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    parent_node_.Bind(std::move(node()));

    // Connect to the `fuchsia.examples.gizmo.Misc` protocol provided by the parent.
    zx::result client = compat::ConnectBanjo<ddk::MiscProtocolClient>(symbols());
    if (client.is_error()) {
      FDF_SLOG(ERROR, "Failed to connect client", KV("status", client.status_string()));
      return client.take_error();
    }
    client_ = *client;

    zx_status_t status = QueryParent();
    if (status != ZX_OK) {
      parent_node_ = {};
      return zx::error(status);
    }

    zx::result result = AddChild();
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      return result.take_error();
    }

    return zx::ok();
  }

  zx_status_t QueryParent() {
    uint32_t response;
    zx_status_t status = client_.GetHardwareId(&response);
    if (status != ZX_OK) {
      return status;
    }
    FDF_LOG(INFO, "Transport client hardware: %X", response);

    uint32_t major_version;
    uint32_t minor_version;
    status = client_.GetFirmwareVersion(&major_version, &minor_version);
    if (status != ZX_OK) {
      return status;
    }
    FDF_LOG(INFO, "Transport client firmware: %d.%d", major_version, minor_version);
    return ZX_OK;
  }

 private:
  zx::result<> AddChild() {
    zx::result devfs_client = fidl::CreateEndpoints(&devfs_connector_);
    if (devfs_client.is_error()) {
      return devfs_client.take_error();
    }

    fidl::Arena arena;
    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(*devfs_client));

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, name())
                    .devfs_args(devfs.Build())
                    .Build();

    // Create endpoints of the `NodeController` for the node.
    zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed: %s", controller_endpoints.status_string());

    zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
    ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed: %s", node_endpoints.status_string());

    fidl::WireResult result = parent_node_->AddChild(args, std::move(controller_endpoints->server),
                                                     std::move(node_endpoints->server));
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      return zx::error(result.status());
    }
    controller_.Bind(std::move(controller_endpoints->client));
    node_.Bind(std::move(node_endpoints->client));

    return zx::ok();
  }

  ddk::MiscProtocolClient client_;

  fidl::ServerEnd<fuchsia_device_fs::Connector> devfs_connector_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace banjo_transport

FUCHSIA_DRIVER_EXPORT(banjo_transport::ChildBanjoTransportDriver);
