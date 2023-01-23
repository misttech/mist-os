// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This header has to come first, and we define our ZX_PROTOCOL, so that
// we don't have to edit protodefs.h to add this test protocol.
#include <bind/fuchsia/lifecycle/cpp/bind.h>
#define ZX_PROTOCOL_PARENT bind_fuchsia_lifecycle::BIND_PROTOCOL_PARENT

#include <fidl/fuchsia.device.fs/cpp/fidl.h>
#include <fidl/fuchsia.driver.compat/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.lifecycle.test/cpp/wire.h>
#include <fuchsia/lifecycle/test/cpp/banjo.h>
#include <lib/driver/compat/cpp/context.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace ft = fuchsia_lifecycle_test;

namespace {

class LifecycleDriver : public fdf::DriverBase, public fidl::WireServer<ft::Device> {
 public:
  LifecycleDriver(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("lifeycle-driver", std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&LifecycleDriver::Serve>(this)) {}

  zx::result<> Start() override {
    FDF_LOG(INFO, "Starting lifecycle driver");

    // Get our parent banjo symbol.
    auto parent_symbol = fdf::GetSymbol<compat::device_t*>(symbols(), compat::kDeviceSymbol);
    if (parent_symbol->proto_ops.id != ZX_PROTOCOL_PARENT) {
      FDF_LOG(ERROR, "Didn't find PARENT banjo protocol, found protocol id: %d",
              parent_symbol->proto_ops.id);
      return zx::error(ZX_ERR_NOT_FOUND);
    }

    parent_protocol_t proto = {};
    proto.ctx = parent_symbol->context;
    proto.ops = reinterpret_cast<const parent_protocol_ops_t*>(parent_symbol->proto_ops.ops);
    parent_client_ = ddk::ParentProtocolClient(&proto);
    if (!parent_client_.is_valid()) {
      FDF_LOG(ERROR, "Failed to create parent client");
      return zx::error(ZX_ERR_INTERNAL);
    }

    // Serve our Service.
    ft::Service::InstanceHandler handler({
        .device = fit::bind_member<&LifecycleDriver::Serve>(this),
    });

    auto result = context().outgoing()->AddService<ft::Service>(std::move(handler));
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add Demo service", KV("status", result.status_string()));
      return result.take_error();
    }

    // Create our compat context, and serve our device when it's created.
    compat::Context::ConnectAndCreate(
        &context(), dispatcher(), [this](zx::result<std::shared_ptr<compat::Context>> context) {
          if (!context.is_ok()) {
            FDF_LOG(ERROR, "Call to Context::ConnectAndCreate failed: %s", context.status_string());
            node().reset();
            return;
          }
          const auto kDeviceName = "lifecycle-device";

          // Export to devfs.
          zx::result connection =
              this->context().incoming()->Connect<fuchsia_device_fs::Exporter>();
          if (connection.is_error()) {
            FDF_SLOG(ERROR, "Failed to connect to fuchsia_device_fs::Exporter",
                     KV("status", connection.status_string()));
            node().reset();
            return;
          }
          fidl::WireSyncClient devfs_exporter{std::move(connection.value())};

          zx::result connector = devfs_connector_.Bind(dispatcher());
          if (connector.is_error()) {
            FDF_SLOG(ERROR, "Failed to bind devfs_connector: %s",
                     KV("status", connector.status_string()));
            node().reset();
            return;
          }
          fidl::WireResult export_result = devfs_exporter->ExportV2(
              std::move(connector.value()),
              fidl::StringView::FromExternal(context.value()->TopologicalPath(kDeviceName)),
              fidl::StringView(), fuchsia_device_fs::ExportOptions());
          if (!export_result.ok()) {
            FDF_SLOG(ERROR, "Failed to export to devfs: %s",
                     KV("status", export_result.status_string()));
            node().reset();
            return;
          }
          if (export_result.value().is_error()) {
            FDF_SLOG(ERROR, "Failed to export to devfs: %s",
                     KV("status", zx_status_get_string(export_result.value().error_value())));
            node().reset();
            return;
          }
        });
    return zx::ok();
  }

  // fidl::WireServer<ft::Device>
  void Ping(PingCompleter::Sync& completer) override { completer.Reply(); }

  // fidl::WireServer<ft::Device>
  void GetString(GetStringCompleter::Sync& completer) override {
    char str[100];
    parent_client_.GetString(str, 100);
    completer.Reply(fidl::StringView::FromExternal(std::string(str)));
  }

  // fidl::WireServer<ft::Device>
  void Stop(StopCompleter::Sync& completer) override {
    stop_completer_ = completer.ToAsync();
    // Resetting the node handle will result in the driver being stopped.
    node().reset();
  }

  void PrepareStop(PrepareStopContext* context) override {
    ZX_ASSERT(stop_completer_);
    ZX_ASSERT(stop_called_ == false);
    stop_completer_->Reply();
    context->complete(context, ZX_OK);
  }

  void Stop() override { stop_called_ = true; }

 private:
  void Serve(fidl::ServerEnd<ft::Device> device) {
    fidl::BindServer(dispatcher(), std::move(device), this);
  }

  driver_devfs::Connector<ft::Device> devfs_connector_;

  std::optional<StopCompleter::Async> stop_completer_;
  bool stop_called_ = false;

  ddk::ParentProtocolClient parent_client_;
};

}  // namespace

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<LifecycleDriver>);
