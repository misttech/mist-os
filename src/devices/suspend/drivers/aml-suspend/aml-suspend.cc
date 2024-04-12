// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-suspend.h"

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <zircon/errors.h>
#include <zircon/syscalls-next.h>

#include "fidl/fuchsia.hardware.suspend/cpp/markers.h"
#include "fidl/fuchsia.hardware.suspend/cpp/wire_types.h"
#include "fidl/fuchsia.kernel/cpp/markers.h"
#include "lib/driver/component/cpp/prepare_stop_completer.h"
#include "lib/driver/incoming/cpp/namespace.h"
#include "lib/fidl/cpp/wire/arena.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/vector_view.h"
#include "lib/fidl/cpp/wire/wire_messaging_declarations.h"
#include "lib/zx/time.h"
#include "zircon/status.h"

namespace suspend {

namespace {

constexpr char kDeviceName[] = "aml-suspend-device";
// LINT.IfChange
constexpr zx::duration kDebugSuspendDuration = zx::sec(5);
// LINT.ThenChange(//src/testing/end_to_end/honeydew/honeydew/interfaces/affordances/system_power_state_controller.py)

}  // namespace

zx::result<zx::resource> AmlSuspend::GetCpuResource() {
  zx::result resource = incoming()->Connect<fuchsia_kernel::CpuResource>();
  if (resource.is_error()) {
    return resource.take_error();
  }

  fidl::WireResult result = fidl::WireCall(resource.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }

  return zx::ok(std::move(result.value().resource));
}

zx::result<> AmlSuspend::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Error creating devfs node");
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("suspend");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .devfs_args(devfs.Build())
                  .Build();

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create endpoints: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints.client));
  parent_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

zx::result<> AmlSuspend::Start() {
  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_hardware_suspend::Suspender>(
      suspend_bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Suspender service %s", result.status_string());
    return result.take_error();
  }

  zx::result resource = GetCpuResource();
  if (!resource.is_ok()) {
    FDF_LOG(ERROR, "Failed to get CPU Resource: %s", resource.status_string());
    return resource.take_error();
  }

  cpu_resource_ = std::move(resource.value());

  zx::result create_devfs_node_result = CreateDevfsNode();
  if (create_devfs_node_result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs %s", create_devfs_node_result.status_string());
    return create_devfs_node_result.take_error();
  }

  FDF_LOG(INFO, "Started Amlogic Suspend Driver");

  return zx::ok();
}

void AmlSuspend::Stop() {}

void AmlSuspend::PrepareStop(fdf::PrepareStopCompleter completer) { completer(zx::ok()); }

void AmlSuspend::GetSuspendStates(GetSuspendStatesCompleter::Sync& completer) {
  fidl::Arena arena;

  auto suspend_to_idle =
      fuchsia_hardware_suspend::wire::SuspendState::Builder(arena).resume_latency(0).Build();
  std::vector<fuchsia_hardware_suspend::wire::SuspendState> suspend_states = {
      std::move(suspend_to_idle)};

  auto resp = fuchsia_hardware_suspend::wire::SuspenderGetSuspendStatesResponse::Builder(arena)
                  .suspend_states(std::move(suspend_states))
                  .Build();

  completer.ReplySuccess(resp);
}

void AmlSuspend::Suspend(SuspendRequestView request, SuspendCompleter::Sync& completer) {
  fidl::Arena arena;

  auto resp = fuchsia_hardware_suspend::wire::SuspenderSuspendResponse::Builder(arena)
                  .suspend_duration(0)
                  .suspend_overhead(0)
                  .Build();

  if (!request->has_state_index() || request->state_index() != 0) {
    // This driver only supports one suspend state for now.
    FDF_LOG(ERROR, "Invalid argument to suspend");
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx_status_t result =
      zx_system_suspend_enter(cpu_resource_.get(), zx::deadline_after(kDebugSuspendDuration).get());

  if (result != ZX_OK) {
    FDF_LOG(ERROR, "zx_system_suspend_enter failed: %s", zx_status_get_string(result));
    completer.ReplyError(result);
  } else {
    completer.ReplySuccess(resp);
  }
}

void AmlSuspend::Serve(fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> request) {
  suspend_bindings_.AddBinding(dispatcher(), std::move(request), this, fidl::kIgnoreBindingClosure);
}

}  // namespace suspend

FUCHSIA_DRIVER_EXPORT(suspend::AmlSuspend);
