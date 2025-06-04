// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dependency_injection_server.h"

namespace msd::internal {
zx::result<> DependencyInjectionServer::Create(
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent) {
  zx::result connector = devfs_connector_.Bind(dispatcher_);
  if (connector.is_error()) {
    return connector.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs{
      {.connector{std::move(connector.value())}, .class_name{"gpu-dependency-injection"}}};

  zx::result child =
      fdf::AddOwnedChild(parent, *fdf::Logger::GlobalInstance(), "gpu-dependency-injection", devfs);
  if (child.is_error()) {
    MAGMA_LOG(ERROR, "Failed to add child: %s", child.status_string());
    return child.take_error();
  }
  child_ = std::move(child.value());

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
  pressure_server_ = fidl::BindServer(dispatcher_, std::move(endpoints->server), this);

  fidl::WireSyncClient provider{std::move(request->provider)};
  // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
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
      return msd::MAGMA_MEMORY_PRESSURE_LEVEL_NORMAL;
    case fuchsia_memorypressure::wire::Level::kWarning:
      return msd::MAGMA_MEMORY_PRESSURE_LEVEL_WARNING;
    case fuchsia_memorypressure::wire::Level::kCritical:
      return msd::MAGMA_MEMORY_PRESSURE_LEVEL_CRITICAL;
    default:
      return msd::MAGMA_MEMORY_PRESSURE_LEVEL_NORMAL;
  }
}

}  // namespace msd::internal
