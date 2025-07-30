// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"

#include <lib/driver/compat/cpp/banjo_client.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/coordinator/engine-driver-client-banjo.h"
#include "src/graphics/display/drivers/coordinator/engine-driver-client-fidl.h"

namespace display_coordinator {

namespace {

constexpr fdf_arena_tag_t kArenaTag = 'DISP';

zx::result<std::unique_ptr<EngineDriverClient>> CreateFidlEngineDriverClient(
    fdf::Namespace& incoming) {
  zx::result<fdf::ClientEnd<fuchsia_hardware_display_engine::Engine>> connect_engine_client_result =
      incoming.Connect<fuchsia_hardware_display_engine::Service::Engine>();
  if (connect_engine_client_result.is_error()) {
    fdf::warn("Failed to connect to display engine FIDL client: {}", connect_engine_client_result);
    return connect_engine_client_result.take_error();
  }
  fdf::ClientEnd<fuchsia_hardware_display_engine::Engine> engine_client =
      std::move(connect_engine_client_result).value();

  if (!engine_client.is_valid()) {
    fdf::warn("Display engine FIDL device is invalid");
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result = fdf::WireCall(engine_client).buffer(arena)->IsAvailable();
  if (!result.ok()) {
    fdf::warn("Display engine FIDL device is not available: {}", result.FormatDescription());
    return zx::error(result.status());
  }

  fbl::AllocChecker alloc_checker;
  auto engine_driver_client =
      fbl::make_unique_checked<EngineDriverClientFidl>(&alloc_checker, std::move(engine_client));
  if (!alloc_checker.check()) {
    fdf::warn("Failed to allocate memory for EngineDriverClientFidl");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(engine_driver_client));
}

zx::result<std::unique_ptr<EngineDriverClient>> CreateBanjoEngineDriverClient(
    std::shared_ptr<fdf::Namespace> incoming) {
  zx::result<ddk::DisplayEngineProtocolClient> dc_result =
      compat::ConnectBanjo<ddk::DisplayEngineProtocolClient>(incoming);
  if (dc_result.is_error()) {
    fdf::warn("Failed to connect to Banjo server via the compat client: {}", dc_result);
    return dc_result.take_error();
  }
  ddk::DisplayEngineProtocolClient dc = std::move(dc_result).value();
  if (!dc.is_valid()) {
    fdf::warn("Failed to get Banjo display controller protocol");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  fbl::AllocChecker alloc_checker;
  auto engine_driver_client =
      fbl::make_unique_checked<EngineDriverClientBanjo>(&alloc_checker, std::move(dc));
  if (!alloc_checker.check()) {
    fdf::warn("Failed to allocate memory for EngineDriverClientBanjo");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(engine_driver_client));
}

}  // namespace

// static
zx::result<std::unique_ptr<EngineDriverClient>> EngineDriverClient::Create(
    std::shared_ptr<fdf::Namespace> incoming) {
  ZX_DEBUG_ASSERT(incoming != nullptr);

  // Attempt to connect to FIDL protocol.
  zx::result<std::unique_ptr<EngineDriverClient>> fidl_engine_driver_client_result =
      CreateFidlEngineDriverClient(*incoming);
  if (fidl_engine_driver_client_result.is_ok()) {
    fdf::info("Using the FIDL Engine driver client");
    return fidl_engine_driver_client_result.take_value();
  }
  fdf::warn("Failed to create FIDL Engine driver client: {}; fallback to banjo",
            fidl_engine_driver_client_result);

  // Fallback to Banjo protocol.
  zx::result<std::unique_ptr<EngineDriverClient>> banjo_engine_driver_client_result =
      CreateBanjoEngineDriverClient(incoming);
  if (banjo_engine_driver_client_result.is_error()) {
    fdf::error("Failed to create banjo Engine driver client: {}",
               banjo_engine_driver_client_result);
  }
  return banjo_engine_driver_client_result;
}

}  // namespace display_coordinator
