// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.rebind.test/cpp/fidl.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/context.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>

#include "fidl/fuchsia.rebind.test/cpp/markers.h"

namespace rebind_child {

const std::string kDriverName = "rebind-child";

class RebindChildServer : public fidl::Server<fuchsia_rebind_test::RebindChild> {};

class RebindChild : public fdf::DriverBase {
 public:
  RebindChild(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&RebindChild::Serve>(this)) {}

  zx::result<> Start() override {
    zx::result result = GetTopologicalPath();
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to get topological path", KV("status", result.status_string()));
      return result.take_error();
    }
    auto topological_path = std::move(*result);

    if (zx::result result = ExportToDevfs(topological_path); result.is_error()) {
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

  void Serve(fidl::ServerEnd<fuchsia_rebind_test::RebindChild> server) {
    ZX_ASSERT_MSG(!server_.has_value(), "Connection to rebind parent already established.");
    server_.emplace();
    fidl::BindServer(dispatcher(), std::move(server), &server_.value());
  }

  std::shared_ptr<compat::DeviceServer> device_server_;
  driver_devfs::Connector<fuchsia_rebind_test::RebindChild> devfs_connector_;
  std::optional<RebindChildServer> server_;
};

}  // namespace rebind_child

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<rebind_child::RebindChild>);
