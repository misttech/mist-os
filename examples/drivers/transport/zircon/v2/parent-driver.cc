// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.examples.gizmo/cpp/wire.h>
#include <fidl/fuchsia.gizmo.protocol/cpp/wire.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <bind/fuchsia/examples/gizmo/cpp/bind.h>

namespace zircon_transport {

// Protocol served to child driver components over the Zircon transport.
class ZirconTransportServer : public fidl::WireServer<fuchsia_examples_gizmo::Device> {
 public:
  explicit ZirconTransportServer() {}

  void GetHardwareId(GetHardwareIdCompleter::Sync& completer) override {
    completer.ReplySuccess(0x1234ABCD);
  }
  void GetFirmwareVersion(GetFirmwareVersionCompleter::Sync& completer) override {
    completer.ReplySuccess(0x0, 0x1);
  }
};

// Protocol served to client components over devfs.
class TestProtocolServer : public fidl::WireServer<fuchsia_gizmo_protocol::TestingProtocol> {
 public:
  explicit TestProtocolServer() {}

  void GetValue(GetValueCompleter::Sync& completer) { completer.Reply(0x1234); }
};

class ParentZirconTransportDriver : public fdf::DriverBase {
 public:
  ParentZirconTransportDriver(fdf::DriverStartArgs start_args,
                              fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("transport-parent", std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&ParentZirconTransportDriver::Serve>(this)) {}

  zx::result<> Start() override {
    node_.Bind(std::move(node()));

    // Publish `fuchsia.examples.gizmo.Service` to the outgoing directory.
    auto protocol_handler =
        [this](fidl::ServerEnd<fuchsia_examples_gizmo::Device> server_end) -> void {
      auto server_impl = std::make_unique<ZirconTransportServer>();
      fidl::BindServer(dispatcher(), std::move(server_end), std::move(server_impl));
    };
    fuchsia_examples_gizmo::Service::InstanceHandler handler(
        {.device = std::move(protocol_handler)});

    auto result = outgoing()->AddService<fuchsia_examples_gizmo::Service>(std::move(handler));
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add service", KV("status", result.status_string()));
      return result.take_error();
    }

    if (zx::result result = ExportService(name()); result.is_error()) {
      FDF_SLOG(ERROR, "Failed to export to services", KV("status", result.status_string()));
      return result.take_error();
    }

    result = AddChild(name());
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add child node", KV("status", result.status_string()));
      return result.take_error();
    }

    return zx::ok();
  }

  // Add a child device node and offer the service capabilities.
  zx::result<> AddChild(std::string_view node_name) {
    fidl::Arena arena;
    std::vector<fuchsia_component_decl::wire::Offer> offers;
    // Offer `fuchsia.examples.gizmo.Service` to the driver that binds to the node.
    auto service_offer = fuchsia_component_decl::wire::OfferService::Builder(arena)
                             .source_name(arena, fuchsia_examples_gizmo::Service::Name)
                             .target_name(arena, fuchsia_examples_gizmo::Service::Name)
                             .Build();
    offers.push_back(fuchsia_component_decl::wire::Offer::WithService(arena, service_offer));

    auto properties = fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty>(arena, 1);
    properties[0] = fdf::MakeProperty(arena, bind_fuchsia_examples_gizmo::DEVICE,
                                      bind_fuchsia_examples_gizmo::DEVICE_ZIRCONTRANSPORT);

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, node_name)
                    .offers(offers)
                    .properties(properties)
                    .devfs_args(devfs.Build())
                    .Build();

    // Create endpoints of the `NodeController` for the node.
    auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    if (endpoints.is_error()) {
      FDF_SLOG(ERROR, "Failed to create endpoint", KV("status", endpoints.status_string()));
      return zx::error(endpoints.status_value());
    }
    auto result = node_->AddChild(args, std::move(endpoints->server), {});
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      return zx::error(result.status());
    }
    controller_.Bind(std::move(endpoints->client));

    return zx::ok();
  }

  // Publish offered services for client components.
  zx::result<> ExportService(std::string_view node_name) {
    // Publish `fuchsia.gizmo.protocol.Service` to the outgoing directory.
    fuchsia_gizmo_protocol::Service::InstanceHandler handler({
        .testing = fit::bind_member<&ParentZirconTransportDriver::Serve>(this),
    });

    auto result = outgoing()->AddService<fuchsia_gizmo_protocol::Service>(std::move(handler));
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add service", KV("status", result.status_string()));
      return result.take_error();
    }
    return zx::ok();
  }

 private:
  // Start serving fuchsia.gizmo.protocol.TestingProtocol.
  void Serve(fidl::ServerEnd<fuchsia_gizmo_protocol::TestingProtocol> server) {
    auto server_impl = std::make_unique<TestProtocolServer>();
    fidl::BindServer(dispatcher(), std::move(server), std::move(server_impl));
  }

  driver_devfs::Connector<fuchsia_gizmo_protocol::TestingProtocol> devfs_connector_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace zircon_transport

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<zircon_transport::ParentZirconTransportDriver>);
