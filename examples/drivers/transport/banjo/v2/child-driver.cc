// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.fs/cpp/fidl.h>
#include <fuchsia/examples/gizmo/cpp/banjo.h>
#include <lib/driver/async-helpers/cpp/task_group.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace banjo_transport {

class ChildBanjoTransportDriver : public fdf::DriverBase {
 public:
  ChildBanjoTransportDriver(fdf::DriverStartArgs start_args,
                            fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("transport-child", std::move(start_args), std::move(driver_dispatcher)) {}

  void Start(fdf::StartCompleter completer) override {
    parent_node_.Bind(std::move(node()));
    start_completer_.emplace(std::move(completer));

    // Connect to the `fuchsia.examples.gizmo.Misc` protocol provided by the parent.
    auto async_task = compat::ConnectBanjo<ddk::MiscProtocolClient>(
        incoming(), [this](zx::result<ddk::MiscProtocolClient> client) {
          if (client.is_error()) {
            FDF_SLOG(ERROR, "Failed to connect client", KV("status", client.status_string()));
            CompleteStart(client.take_error());
            return;
          }
          client_ = *client;

          zx_status_t status = QueryParent();
          if (status != ZX_OK) {
            parent_node_ = {};
            CompleteStart(zx::error(status));
            return;
          }

          QueryTopologicalPath();
        });
    task_group_.AddTask(std::move(async_task));
  }

  void QueryTopologicalPath() {
    auto compat = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
    ZX_ASSERT(compat.is_ok());
    compat_.Bind(std::move(compat.value()), dispatcher());

    compat_->GetTopologicalPath().Then(
        [this](fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetTopologicalPath>&
                   result) mutable {
          if (!result.ok()) {
            FDF_LOG(ERROR, "Failed to get child topo path: %s", result.FormatDescription().c_str());
            CompleteStart(zx::error(result.error().status()));
            return;
          }

          std::string expected = "sys/test/transport-parent";
          std::string actual = std::string(result->path.get());
          if (actual != expected) {
            FDF_LOG(INFO, "Unexpected child topo path: %s", actual.c_str());
            CompleteStart(zx::error(ZX_ERR_INTERNAL));
            return;
          }

          zx::result add_result = AddChild();
          if (add_result.is_error()) {
            FDF_SLOG(ERROR, "Failed to add child", KV("status", add_result.status_string()));
            CompleteStart(add_result.take_error());
            return;
          }

          CompleteStart(zx::ok());
        });
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

  void CompleteStart(zx::result<> result) {
    ZX_ASSERT(start_completer_.has_value());
    start_completer_.value()(result);
    start_completer_.reset();
  }

  std::optional<fdf::StartCompleter> start_completer_;

  fidl::WireClient<fuchsia_driver_compat::Device> compat_;

  ddk::MiscProtocolClient client_;

  fidl::ServerEnd<fuchsia_device_fs::Connector> devfs_connector_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  fdf::async_helpers::TaskGroup task_group_;
};

}  // namespace banjo_transport

FUCHSIA_DRIVER_EXPORT(banjo_transport::ChildBanjoTransportDriver);
