// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.examples.gizmo/cpp/driver/wire.h>
#include <fidl/fuchsia.gizmo.protocol/cpp/wire.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>

namespace driver_transport {

// Protocol served to client components over devfs.
class TestProtocolServer : public fidl::WireServer<fuchsia_gizmo_protocol::TestingProtocol> {
 public:
  explicit TestProtocolServer() {}

  void GetValue(GetValueCompleter::Sync& completer) { completer.Reply(0x1234); }
};

class ChildDriverTransportDriver : public fdf::DriverBase {
 public:
  ChildDriverTransportDriver(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("transport-child", std::move(start_args), std::move(driver_dispatcher)),
        arena_(fdf::Arena('EXAM')),
        devfs_connector_(fit::bind_member<&ChildDriverTransportDriver::Serve>(this)) {}

  zx::result<> Start() override {
    parent_node_.Bind(std::move(node()));

    // Publish `fuchsia.gizmo.protocol.Service` to the outgoing directory.
    fuchsia_gizmo_protocol::Service::InstanceHandler handler({
        .testing = fit::bind_member<&ChildDriverTransportDriver::Serve>(this),
    });

    auto result = outgoing()->AddService<fuchsia_gizmo_protocol::Service>(std::move(handler));
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add service", KV("status", result.status_string()));
      return result.take_error();
    }

    // Connect to the `fuchsia.examples.gizmo.Service` provided by the parent.
    result = ConnectGizmoService();
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to connect client", KV("status", result.status_string()));
      return result.take_error();
    }

    client_.buffer(arena_)->GetHardwareId().ThenExactlyOnce(
        fit::bind_member<&ChildDriverTransportDriver::HardwareIdResult>(this));

    return zx::ok();
  }

  // Connect to the parent's offered service.
  zx::result<> ConnectGizmoService() {
    auto connect_result = incoming()->Connect<fuchsia_examples_gizmo::Service::Device>();
    if (connect_result.is_error()) {
      FDF_SLOG(ERROR, "Failed to connect gizmo device protocol.",
               KV("status", connect_result.status_string()));
      return connect_result.take_error();
    }
    client_ = fdf::WireClient(std::move(connect_result.value()), driver_dispatcher()->get());

    return zx::ok();
  }

  // Asynchronous GetHardwareId result callback.
  void HardwareIdResult(
      fdf::WireUnownedResult<fuchsia_examples_gizmo::Device::GetHardwareId>& result) {
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to request hardware ID.", KV("status", result.status_string()));
      node().reset();
      return;
    } else if (result->is_error()) {
      FDF_SLOG(ERROR, "Hardware ID request returned an error.",
               KV("status", result->error_value()));
      node().reset();
      return;
    }
    FDF_SLOG(INFO, "Transport client hardware.", KV("response", result.value().value()->response));

    client_.buffer(arena_)->GetFirmwareVersion().ThenExactlyOnce(
        fit::bind_member<&ChildDriverTransportDriver::FirmwareVersionResult>(this));
  }

  // Asynchronous GetFirmwareVersion result callback.
  void FirmwareVersionResult(
      fdf::WireUnownedResult<fuchsia_examples_gizmo::Device::GetFirmwareVersion>& result) {
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to request firmware version.", KV("status", result.status_string()));
      node().reset();
      return;
    } else if (result->is_error()) {
      FDF_SLOG(ERROR, "Firmware version request returned an error.",
               KV("status", result->error_value()));
      node().reset();
      return;
    }
    FDF_SLOG(INFO, "Transport client firmware.", KV("major", result.value().value()->major),
             KV("minor", result.value().value()->minor));

    if (zx::result result = AddChild(name()); result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      node().reset();
      return;
    }
  }

 private:
  // Start serving fuchsia.gizmo.protocol.TestingProtocol.
  void Serve(fidl::ServerEnd<fuchsia_gizmo_protocol::TestingProtocol> server) {
    auto server_impl = std::make_unique<TestProtocolServer>();
    fidl::BindServer(dispatcher(), std::move(server), std::move(server_impl));
  }

  zx::result<> AddChild(std::string_view node_name) {
    fidl::Arena arena;
    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, node_name)
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

  fdf::Arena arena_;
  fdf::WireClient<fuchsia_examples_gizmo::Device> client_;
  driver_devfs::Connector<fuchsia_gizmo_protocol::TestingProtocol> devfs_connector_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace driver_transport

FUCHSIA_DRIVER_EXPORT(driver_transport::ChildDriverTransportDriver);
