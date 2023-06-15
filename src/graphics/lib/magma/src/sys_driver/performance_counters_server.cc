// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "performance_counters_server.h"

zx::result<> PerformanceCountersServer::Create(
    fidl::WireSyncClient<fuchsia_driver_framework::Node>& node_client) {
  zx_status_t status = zx::event::create(0, &event_);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("gpu-performance-counters");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, "gpu-performance-counters")
                  .devfs_args(devfs.Build())
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed: %s", controller_endpoints.status_string());
  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed: %s", node_endpoints.status_string());

  fidl::WireResult result = node_client->AddChild(args, std::move(controller_endpoints->server),
                                                  std::move(node_endpoints->server));
  node_controller_.Bind(std::move(controller_endpoints->client));
  node_.Bind(std::move(node_endpoints->client));
  return zx::ok();
}

zx_koid_t PerformanceCountersServer::GetEventKoid() {
  zx_info_handle_basic_t info;
  if (event_.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr) != ZX_OK)
    return 0;
  return info.koid;
}

void PerformanceCountersServer::GetPerformanceCountToken(
    GetPerformanceCountTokenCompleter::Sync& completer) {
  zx::event event_duplicate;
  zx_status_t status = event_.duplicate(ZX_RIGHT_SAME_RIGHTS, &event_duplicate);
  if (status != ZX_OK) {
    completer.Close(status);
  } else {
    completer.Reply(std::move(event_duplicate));
  }
}
