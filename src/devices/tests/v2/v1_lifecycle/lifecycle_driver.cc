// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This header has to come first, and we define our ZX_PROTOCOL, so that
// we don't have to edit protodefs.h to add this test protocol.
#include <bind/fuchsia/lifecycle/cpp/bind.h>
#define ZX_PROTOCOL_PARENT bind_fuchsia_lifecycle::BIND_PROTOCOL_PARENT

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.lifecycle.test/cpp/wire.h>
#include <fuchsia/lifecycle/test/cpp/banjo.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/internal/symbols.h>
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
      : DriverBase("lifecycle-driver", std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&LifecycleDriver::Serve>(this)) {}

  zx::result<> Start() override {
    FDF_LOG(INFO, "Starting lifecycle driver");

    // Get our parent banjo symbol.
    auto parent_symbol =
        fdf_internal::GetSymbol<compat::device_t*>(symbols(), compat::kDeviceSymbol);
    if (!parent_symbol) {
      FDF_LOG(ERROR, "Failed to find parent symbol");
      return zx::error(ZX_ERR_NOT_FOUND);
    }
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

    return ExportToDevfs();
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

  void PrepareStop(fdf::PrepareStopCompleter completer) override {
    ZX_ASSERT(stop_completer_);
    ZX_ASSERT(stop_called_ == false);
    stop_completer_->Reply();
    completer(zx::ok());
  }

  void Stop() override { stop_called_ = true; }

 private:
  zx::result<> ExportToDevfs() {
    // Create a node for devfs.
    fidl::Arena arena;
    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, "lifecycle-device")
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

  void Serve(fidl::ServerEnd<ft::Device> device) {
    fidl::BindServer(dispatcher(), std::move(device), this);
  }

  driver_devfs::Connector<ft::Device> devfs_connector_;

  std::optional<StopCompleter::Async> stop_completer_;
  bool stop_called_ = false;

  ddk::ParentProtocolClient parent_client_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<LifecycleDriver>);
