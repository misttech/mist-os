// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.devfs.test/cpp/wire.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace ft = fuchsia_devfs_test;

namespace {

class RootDriver : public fdf::DriverBase, public fidl::WireServer<ft::Device> {
  static constexpr std::string_view name = "root";

 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(name, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&RootDriver::Serve>(this)) {}

  zx::result<> Start() override {
    // Export to devfs.
    zx::result connection = this->context().incoming()->Connect<fuchsia_device_fs::Exporter>();
    if (connection.is_error()) {
      FDF_SLOG(ERROR, "Failed to connect to fuchsia_device_fs::Exporter",
               KV("status", connection.status_string()));
      return connection.take_error();
    }
    fidl::WireSyncClient devfs_exporter{std::move(connection.value())};

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      FDF_SLOG(ERROR, "Failed to bind devfs_connector: %s",
               KV("status", connector.status_string()));
      return connector.take_error();
    }
    fidl::WireResult export_result =
        devfs_exporter->Export(std::move(connector.value()),
                               fidl::StringView::FromExternal("root-device"), fidl::StringView());
    if (!export_result.ok()) {
      FDF_SLOG(ERROR, "Failed to export to devfs: %s", KV("status", export_result.status_string()));
      return zx::error(export_result.status());
    }
    if (export_result.value().is_error()) {
      FDF_SLOG(ERROR, "Failed to export to devfs: %s",
               KV("status", zx_status_get_string(export_result.value().error_value())));
      return export_result.value().take_error();
    }
    return zx::ok();
  }

 private:
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
  driver_devfs::Connector<ft::Device> devfs_connector_;
};

}  // namespace

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<RootDriver>);
