// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.fs/cpp/fidl.h>
#include <fidl/fuchsia.driver.compat/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.demo/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <zircon/errors.h>

#include <unordered_map>

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

  ~DemoNumber() override { FDF_LOG(INFO, "Driver unloaded: %s", kDriverName.c_str()); }

  // Called by the driver framework to initialize the driver instance.
  zx::result<> Start() override {
    fuchsia_hardware_demo::Service::InstanceHandler handler({
        .demo = fit::bind_member<&DemoNumber::Connect>(this),
    });

    if (zx::result result =
            outgoing()->AddService<fuchsia_hardware_demo::Service>(std::move(handler));
        result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add Demo service", KV("status", result.status_string()));
      return result.take_error();
    }

    // Create a node for devfs.
    fidl::Arena arena;
    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

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

  std::unordered_map<zx_handle_t, DemoNumberConnection> servers_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fuchsia_hardware_demo::Demo> devfs_connector_;
};

}  // namespace demo_number

FUCHSIA_DRIVER_EXPORT(demo_number::DemoNumber);
