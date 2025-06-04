// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "performance_counters_server.h"

namespace msd::internal {
zx::result<> PerformanceCountersServer::Create(
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent) {
  zx_status_t status = zx::event::create(0, &event_);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  zx::result connector = devfs_connector_.Bind(dispatcher_);
  if (connector.is_error()) {
    return connector.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs{
      {.connector{std::move(connector.value())}, .class_name{"gpu-performance-counters"}}};

  zx::result child =
      fdf::AddOwnedChild(parent, *fdf::Logger::GlobalInstance(), "gpu-performance-counters", devfs);
  if (child.is_error()) {
    MAGMA_LOG(ERROR, "Failed to add child: %s", child.status_string());
    return child.take_error();
  }
  child_ = std::move(child.value());

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

}  // namespace msd::internal
