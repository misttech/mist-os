// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/tests/test_driver.h>

zx::result<> TestDriver::Start() {
  node_client_.Bind(std::move(node()), dispatcher());
  return zx::ok();
}

zx::result<> TestDriver::ServeDriverService() {
  zx::result result =
      context().outgoing()->AddService<fuchsia_driver_component_test::DriverService>(
          GetInstanceHandlerDriver());
  if (result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> TestDriver::ServeZirconService() {
  zx::result result =
      context().outgoing()->AddService<fuchsia_driver_component_test::ZirconService>(
          GetInstanceHandlerZircon());
  if (result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> TestDriver::ValidateIncomingDriverService() {
  zx::result driver_connect_result =
      context().incoming()->Connect<fuchsia_driver_component_test::DriverService::Device>();
  if (driver_connect_result.is_error()) {
    FDF_LOG(ERROR, "Couldn't connect to DriverService.");
    return driver_connect_result.take_error();
  }

  fdf::Arena arena('DRVR');
  fdf::WireUnownedResult wire_result =
      fdf::WireCall(driver_connect_result.value()).buffer(arena)->DriverMethod();
  if (!wire_result.ok()) {
    FDF_LOG(ERROR, "Failed to call DriverMethod %s", wire_result.status_string());
    return zx::error(wire_result.status());
  }

  if (wire_result->is_error()) {
    FDF_LOG(ERROR, "DriverMethod error %s",
            zx_status_get_string(wire_result.value().error_value()));
    return wire_result.value().take_error();
  }

  return zx::ok();
}

zx::result<> TestDriver::ValidateIncomingZirconService() {
  zx::result zircon_connect_result =
      context().incoming()->Connect<fuchsia_driver_component_test::ZirconService::Device>();
  if (zircon_connect_result.is_error()) {
    FDF_LOG(ERROR, "Couldn't connect to ZirconService.");
    return zircon_connect_result.take_error();
  }

  fidl::WireResult wire_result = fidl::WireCall(zircon_connect_result.value())->ZirconMethod();
  if (!wire_result.ok()) {
    FDF_LOG(ERROR, "Failed to call ZirconMethod %s", wire_result.status_string());
    return zx::error(wire_result.status());
  }

  if (wire_result->is_error()) {
    FDF_LOG(ERROR, "ZirconMethod error %s",
            zx_status_get_string(wire_result.value().error_value()));
    return wire_result.value().take_error();
  }

  return zx::ok();
}

void TestDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  // Delay the completion to simulate an async workload.
  async::PostDelayedTask(
      dispatcher(), [completer = std::move(completer)]() mutable { completer(zx::ok()); },
      zx::sec(1));
}

void TestDriver::CreateChildNodeSync() {
  auto node_controller = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT(node_controller.is_ok());

  fidl::Arena arena;
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, "child").Build();
  auto result = node_client_.sync()->AddChild(args, std::move(node_controller->server), {});
  ZX_ASSERT(result.ok());
  ZX_ASSERT(result->is_ok());
  sync_added_child_ = true;
}

void TestDriver::CreateChildNodeAsync() {
  auto node_controller = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT(node_controller.is_ok());
  fidl::Arena arena;
  auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, "child").Build();

  node_client_->AddChild(args, std::move(node_controller->server), {})
      .Then([this](fidl::WireUnownedResult<fuchsia_driver_framework::Node::AddChild>& result) {
        ZX_ASSERT_MSG(result.ok(), "%s", result.FormatDescription().c_str());
        ZX_ASSERT_MSG(result->is_ok(), "%s", result.FormatDescription().c_str());
        async_added_child_ = true;
      });
}

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<TestDriver>);
