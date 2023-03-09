// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.rebind.test/cpp/fidl.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/context.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/legacy-bind-constants/legacy-bind-constants.h>
#include <zircon/errors.h>

#include <optional>
#include <string>
#include <unordered_map>

#include "lib/zx/result.h"

namespace rebind_parent {

const std::string kDriverName = "rebind-parent";
const std::string kChildNodeName = "rebind-child";

class RebindParentServer : public fidl::Server<fuchsia_rebind_test::RebindParent> {
 public:
  RebindParentServer(async_dispatcher_t* dispatcher,
                     std::shared_ptr<compat::DeviceServer> device_server,
                     fidl::WireSyncClient<fuchsia_driver_framework::Node> node, fdf::Logger* logger)
      : node_(std::move(node)), device_server_(std::move(device_server)), logger_(logger) {}

 private:
  void RemoveChild(RemoveChildCompleter::Sync& completer) override {
    if (!node_controller_.has_value()) {
      FDF_LOG(ERROR, "Child device does not exist");
      completer.Reply(fit::error(ZX_ERR_BAD_STATE));
      return;
    }

    auto status = (*node_controller_)->Remove();
    if (status.status() != ZX_OK) {
      FDF_SLOG(ERROR, "Failed to remove node", KV("status", status.status_string()));
      completer.Reply(fit::error(status.status()));
      return;
    }
    completer.Reply(fit::ok());
  }

  void AddChild(AddChildCompleter::Sync& completer) override {
    if (device_server_ == nullptr) {
      FDF_LOG(ERROR, "Device server is null");
      completer.Reply(fit::error(ZX_ERR_BAD_STATE));
      return;
    }
    fidl::Arena arena;
    // Offer `fuchsia.driver.compat.Service` to the driver that binds to the node.
    auto offers = device_server_->CreateOffers(arena);

    auto properties = fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty>(arena, 1);
    properties[0] = fdf::MakeProperty(arena, BIND_PROTOCOL, 1234);

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, kChildNodeName)
                    .offers(offers)
                    .properties(properties)
                    .Build();

    // Create endpoints of the `NodeController` for the node.
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    if (endpoints.is_error()) {
      FDF_SLOG(ERROR, "Failed to create endpoint", KV("status", endpoints.status_string()));
      completer.Reply(fit::error(endpoints.status_value()));
      return;
    }
    auto result = node_->AddChild(args, std::move(endpoints->server), {});
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      completer.Reply(fit::error(result.status()));
      return;
    }
    node_controller_.emplace(std::move(endpoints->client));
    completer.Reply(fit::ok());
  }

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  std::shared_ptr<compat::DeviceServer> device_server_;
  std::optional<fidl::WireSyncClient<fuchsia_driver_framework::NodeController>> node_controller_;
  fdf::Logger* logger_;
};

class RebindParent : public fdf::DriverBase {
 public:
  RebindParent(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&RebindParent::Serve>(this)) {}

  zx::result<> Start() override {
    zx::result result = GetTopologicalPath();
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to get topological path", KV("status", result.status_string()));
      return result.take_error();
    }
    auto toplogical_path = std::move(*result);

    device_server_ =
        std::make_shared<compat::DeviceServer>(std::string(name()), 0, toplogical_path);
    auto status = device_server_->Serve(dispatcher(), &context().outgoing()->component());
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to serve compat device server: %s", zx_status_get_string(status));
      return zx::error_result(status);
    }

    if (zx::result result = ExportToDevfs(toplogical_path); result.is_error()) {
      FDF_LOG(ERROR, "Failed to export device to devfs: %s", result.status_string());
      return result.take_error();
    }

    return zx::ok();
  }

 private:
  zx::result<std::string> GetTopologicalPath() {
    auto svc_dir = context().incoming()->svc_dir();
    auto result = component::OpenServiceAt<fuchsia_driver_compat::Service>(svc_dir, "default");
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to open service", KV("status", result.status_string()));
      return result.take_error();
    }
    auto parent_client = result.value().connect_device();
    fidl::SyncClient parent{std::move(parent_client.value())};

    auto getTopoRes = parent->GetTopologicalPath();
    if (getTopoRes.is_error()) {
      const auto& error = getTopoRes.error_value();
      FDF_SLOG(ERROR, "Failed to topological path of parent", KV("status", error.status_string()));
      return zx::error(error.status());
    }

    return zx::ok(getTopoRes->path().append("/").append(name()));
  }

  zx::result<> ExportToDevfs(const std::string& devfs_path) {
    zx::result connection = context().incoming()->Connect<fuchsia_device_fs::Exporter>();
    if (connection.is_error()) {
      FDF_SLOG(ERROR, "Failed to connect to devfs exporter",
               KV("status", connection.status_string()));
      return connection.take_error();
    }
    fidl::WireSyncClient devfs_exporter{std::move(connection.value())};

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      FDF_SLOG(ERROR, "Failed to bind devfs connector", KV("status", connector.status_string()));
      return connector.take_error();
    }
    fidl::WireResult result =
        devfs_exporter->Export(std::move(connector.value()),
                               fidl::StringView::FromExternal(devfs_path), fidl::StringView());
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to send FIDL request to export to devfs",
               KV("status", result.status_string()));
      return zx::error(result.status());
    }
    if (result.value().is_error()) {
      FDF_SLOG(ERROR, "Failed to export to devfs", KV("status", result.status_string()));
      return result.value().take_error();
    }
    return zx::ok();
  }

  void Serve(fidl::ServerEnd<fuchsia_rebind_test::RebindParent> server) {
    ZX_ASSERT_MSG(!server_.has_value(), "Connection to rebind parent already established.");
    auto node_client = fidl::WireSyncClient(std::move(node()));
    server_.emplace(dispatcher(), device_server_, std::move(node_client), &logger());
    fidl::BindServer(dispatcher(), std::move(server), &server_.value());
  }

  std::shared_ptr<compat::DeviceServer> device_server_;
  driver_devfs::Connector<fuchsia_rebind_test::RebindParent> devfs_connector_;
  std::optional<RebindParentServer> server_;
};

}  // namespace rebind_parent

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<rebind_parent::RebindParent>);
