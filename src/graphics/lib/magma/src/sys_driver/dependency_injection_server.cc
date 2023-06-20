// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dependency_injection_server.h"

namespace msd {
zx::result<> DependencyInjectionServer::Create(
    fidl::WireSyncClient<fuchsia_driver_framework::Node>& node_client) {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("gpu-dependency-injection");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, "gpu-dependency-injection")
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

void DependencyInjectionServer::SetMemoryPressureProvider(
    fuchsia_gpu_magma::wire::DependencyInjectionSetMemoryPressureProviderRequest* request,
    SetMemoryPressureProviderCompleter::Sync& completer) {
  if (pressure_server_) {
    return;
  }
  auto endpoints = fidl::CreateEndpoints<fuchsia_memorypressure::Watcher>();
  if (!endpoints.is_ok()) {
    MAGMA_LOG(WARNING, "Failed to create fidl Endpoints");
    return;
  }
  pressure_server_ = fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                      std::move(endpoints->server), this);

  fidl::WireSyncClient provider{std::move(request->provider)};
  // TODO(fxbug.dev/97955) Consider handling the error instead of ignoring it.
  (void)provider->RegisterWatcher(std::move(endpoints->client));
}

void DependencyInjectionServer::OnLevelChanged(OnLevelChangedRequestView request,
                                               OnLevelChangedCompleter::Sync& completer) {
  owner_->SetMemoryPressureLevel(GetMagmaLevel(request->level));
  completer.Reply();
}

// static
MagmaMemoryPressureLevel DependencyInjectionServer::GetMagmaLevel(
    fuchsia_memorypressure::wire::Level level) {
  switch (level) {
    case fuchsia_memorypressure::wire::Level::kNormal:
      return MAGMA_MEMORY_PRESSURE_LEVEL_NORMAL;
    case fuchsia_memorypressure::wire::Level::kWarning:
      return MAGMA_MEMORY_PRESSURE_LEVEL_WARNING;
    case fuchsia_memorypressure::wire::Level::kCritical:
      return MAGMA_MEMORY_PRESSURE_LEVEL_CRITICAL;
    default:
      return MAGMA_MEMORY_PRESSURE_LEVEL_NORMAL;
  }
}

}  // namespace msd
