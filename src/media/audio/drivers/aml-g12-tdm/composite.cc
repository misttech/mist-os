// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace audio::aml_g12 {

// TODO(b/300991607): Use clock-gate, clock-pll, and gpio-init services.

zx::result<> Driver::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(server_->dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("audio-composite");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDriverName)
                  .devfs_args(devfs.Build())
                  .Build();

  // Create endpoints of the `NodeController` for the node.
  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Controller end point creation failed: %s",
                controller_endpoints.status_string());

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Node end point creation failed: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_SLOG(ERROR, "Call to add child failed", KV("status", result.status_string()));
    return zx::error(result.status());
  }
  if (!result->is_ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("error", result.FormatDescription().c_str()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  controller_.Bind(std::move(controller_endpoints->client));
  node_.Bind(std::move(node_endpoints->client));

  return zx::ok();
}

zx::result<> Driver::Start() {
  server_ = std::make_unique<Server>(dispatcher());

  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_hardware_audio::Composite>(
      bindings_.CreateHandler(server_.get(), dispatcher(), fidl::kIgnoreBindingClosure),
      kDriverName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
    return result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs %s", result.status_string());
    return result.take_error();
  }

  FDF_SLOG(INFO, "Started");

  return zx::ok();
}

}  // namespace audio::aml_g12

FUCHSIA_DRIVER_EXPORT(audio::aml_g12::Driver);
