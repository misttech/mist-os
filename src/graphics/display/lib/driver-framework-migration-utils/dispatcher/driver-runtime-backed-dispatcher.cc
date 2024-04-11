// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/driver-runtime-backed-dispatcher.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <string_view>

#include <fbl/alloc_checker.h>

namespace display {

// static
zx::result<std::unique_ptr<Dispatcher>> DriverRuntimeBackedDispatcher::Create(
    std::string_view name, std::string_view scheduler_role) {
  ZX_DEBUG_ASSERT(!name.empty());

  zx::result<fdf::SynchronizedDispatcher> create_dispatcher_result =
      fdf::SynchronizedDispatcher::Create(
          fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, name, [](fdf_dispatcher_t*) {},
          scheduler_role);
  if (create_dispatcher_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create a synchronized dispatcher: %s",
            create_dispatcher_result.status_string());
    return create_dispatcher_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto dispatcher = fbl::make_unique_checked<DriverRuntimeBackedDispatcher>(
      &alloc_checker, std::move(create_dispatcher_result).value());
  if (!alloc_checker.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(dispatcher));
}

DriverRuntimeBackedDispatcher::DriverRuntimeBackedDispatcher(
    fdf::SynchronizedDispatcher fdf_dispatcher)
    : fdf_dispatcher_(std::move(fdf_dispatcher)) {
  ZX_DEBUG_ASSERT(fdf_dispatcher_.get() != nullptr);
}

DriverRuntimeBackedDispatcher::~DriverRuntimeBackedDispatcher() = default;

}  // namespace display
