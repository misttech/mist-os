// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.fs/cpp/fidl.h>
#include <fidl/fuchsia.driver.compat/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.demo/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/outgoing_directory.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <zircon/errors.h>

#include <unordered_map>

namespace {

// Connect to parent device node using fuchsia.driver.compat.Service
zx::result<fidl::ClientEnd<fuchsia_driver_compat::Device>> ConnectToParentDevice(
    fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir, std::string_view name) {
  auto result = component::OpenServiceAt<fuchsia_driver_compat::Service>(svc_dir, name);
  if (result.is_error()) {
    return result.take_error();
  }
  return result.value().connect_device();
}

// Return the topological path of the parent device node.
zx::result<std::string> GetTopologicalPath(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir) {
  auto parent_client = ConnectToParentDevice(svc_dir, "default");
  if (parent_client.is_error()) {
    return parent_client.take_error();
  }
  fidl::SyncClient parent{std::move(parent_client.value())};

  auto result = parent->GetTopologicalPath();
  if (result.is_error()) {
    const auto& error = result.error_value();
    return zx::error(error.status());
  }

  return zx::ok(result->path());
}

}  // namespace

namespace demo_number {

const std::string kDriverName = "demo_number";

// This class manages a single connection for the `fuchsia.hardware.demo/Demo` protocol.
class DemoNumberConnection : public fidl::Server<fuchsia_hardware_demo::Demo> {
 public:
  using OnUnbindFn = fit::function<void(fidl::UnbindInfo)>;

  DemoNumberConnection(async_dispatcher_t* dispatcher,
                       fidl::ServerEnd<fuchsia_hardware_demo::Demo> server, OnUnbindFn on_unbind)
      : binding_(dispatcher, std::move(server), this, std::move(on_unbind)) {}

 private:
  void GetNumber(GetNumberCompleter::Sync& completer) override {
    completer.Reply(current_number);
    current_number += 1;
  }

  uint32_t current_number = 0;
  const fidl::ServerBinding<fuchsia_hardware_demo::Demo> binding_;
};

// This class represents the driver instance.
class DemoNumber : public fdf::DriverBase {
 public:
  DemoNumber(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&DemoNumber::Connect>(this)) {}

  // Called by the driver framework to initialize the driver instance.
  zx::result<> Start() override {
    fuchsia_hardware_demo::Service::InstanceHandler handler({
        .demo = fit::bind_member<&DemoNumber::Connect>(this),
    });

    if (zx::result result =
            context().outgoing()->AddService<fuchsia_hardware_demo::Service>(std::move(handler));
        result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add Demo service", KV("status", result.status_string()));
      return result.take_error();
    }

    // Construct a devfs path that matches the device nodes topological path
    auto path_result = GetTopologicalPath(context().incoming()->svc_dir());
    if (path_result.is_error()) {
      FDF_SLOG(ERROR, "Failed to get topological path", KV("status", path_result.status_string()));
      return path_result.take_error();
    }
    auto devfs_path = path_result.value().append("/").append(kDriverName);
    FDF_LOG(INFO, "Exporting device to: %s", devfs_path.c_str());

    zx::result connection = context().incoming()->Connect<fuchsia_device_fs::Exporter>();
    if (connection.is_error()) {
      return connection.take_error();
    }
    fidl::WireSyncClient devfs_exporter{std::move(connection.value())};

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }
    fidl::WireResult result = devfs_exporter->ExportV2(
        std::move(connector.value()), fidl::StringView::FromExternal(devfs_path),
        fidl::StringView(), fuchsia_device_fs::ExportOptions());
    if (!result.ok()) {
      return zx::error(result.status());
    }
    if (result.value().is_error()) {
      return result.value().take_error();
    }

    return zx::ok();
  }

  // Called by the driver framework before the driver instance is destroyed.
  void Stop() override { FDF_LOG(INFO, "Driver unloaded: %s", kDriverName.c_str()); }

 private:
  // Bind a connection request to a DemoNumberConnection.
  void Connect(fidl::ServerEnd<fuchsia_hardware_demo::Demo> request) {
    const zx_handle_t key = request.channel().get();
    auto [it, inserted] = servers_.try_emplace(
        key, dispatcher(), std::move(request), [this, key](fidl::UnbindInfo info) {
          FDF_LOG(INFO, "Client connection unbound: %s", info.FormatDescription().c_str());
          size_t erase_count = servers_.erase(key);
          // TODO(fxbug.dev/118019): Use FATAL log when its supported.
          ZX_ASSERT_MSG(erase_count == 1,
                        "Failed to erase connection with key: %d, erased %ld connections", key,
                        erase_count);
        });
    // TODO(fxbug.dev/118019): Use FATAL log when its supported.
    ZX_ASSERT_MSG(inserted, "Failed to insert connection with key: %d", key);
  }

  driver_devfs::Connector<fuchsia_hardware_demo::Demo> devfs_connector_;
  std::unordered_map<zx_handle_t, DemoNumberConnection> servers_;
};

}  // namespace demo_number

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<demo_number::DemoNumber>);
