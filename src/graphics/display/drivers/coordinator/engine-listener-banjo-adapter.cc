// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/engine-listener-banjo-adapter.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>

#include <cstdint>
#include <memory>
#include <utility>

#include "src/graphics/display/drivers/coordinator/added-display-info.h"
#include "src/graphics/display/drivers/coordinator/engine-listener.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/driver-utils/post-task.h"

namespace display_coordinator {

EngineListenerBanjoAdapter::EngineListenerBanjoAdapter(
    EngineListener* engine_listener, fdf::UnownedSynchronizedDispatcher dispatcher)
    : engine_listener_(*engine_listener), dispatcher_(std::move(dispatcher)) {
  ZX_DEBUG_ASSERT(engine_listener != nullptr);
}

void EngineListenerBanjoAdapter::DisplayEngineListenerOnDisplayAdded(
    const raw_display_info_t* banjo_display_info) {
  ZX_DEBUG_ASSERT(banjo_display_info != nullptr);

  zx::result<std::unique_ptr<AddedDisplayInfo>> added_display_info_result =
      AddedDisplayInfo::Create(*banjo_display_info);
  if (added_display_info_result.is_error()) {
    // AddedDisplayInfo::Create() has already logged the error.
    return;
  }
  std::unique_ptr<AddedDisplayInfo> added_display_info =
      std::move(added_display_info_result).value();

  zx::result<> post_task_result = display::PostTask<kDisplayTaskTargetSize>(
      *dispatcher_->async_dispatcher(),
      [&engine_listener = engine_listener_,
       added_display_info = std::move(added_display_info)]() mutable {
        // `engine_listener` is guaranteed to be valid because the listener
        // outlives the dispatcher it's scheduled on.
        engine_listener.OnDisplayAdded(std::move(added_display_info));
      });
  if (post_task_result.is_error()) {
    fdf::error("Failed to dispatch OnDisplayAdded task: {}", post_task_result);
  }
}

void EngineListenerBanjoAdapter::DisplayEngineListenerOnDisplayRemoved(uint64_t banjo_display_id) {
  display::DisplayId display_id(banjo_display_id);
  zx::result<> post_task_result = display::PostTask<kDisplayTaskTargetSize>(
      *dispatcher_->async_dispatcher(), [&engine_listener = engine_listener_, display_id]() {
        // `engine_listener` is guaranteed to be valid because the listener
        // outlives the dispatcher it's scheduled on.
        engine_listener.OnDisplayRemoved(display_id);
      });
  if (post_task_result.is_error()) {
    fdf::error("Failed to dispatch OnDisplayRemoved task: {}", post_task_result);
  }
}

void EngineListenerBanjoAdapter::DisplayEngineListenerOnDisplayVsync(
    uint64_t banjo_display_id, zx_instant_mono_t banjo_timestamp,
    const config_stamp_t* banjo_config_stamp_ptr) {
  ZX_DEBUG_ASSERT(banjo_display_id != INVALID_DISPLAY_ID);
  ZX_DEBUG_ASSERT(banjo_config_stamp_ptr != nullptr);

  display::DisplayId display_id = display::DisplayId(banjo_display_id);
  zx::time_monotonic timestamp = zx::time_monotonic(banjo_timestamp);
  display::DriverConfigStamp driver_config_stamp(*banjo_config_stamp_ptr);
  if (driver_config_stamp == display::kInvalidDriverConfigStamp) {
    fdf::error("Dropping VSync with invalid DriverConfigStamp");
    return;
  }

  zx::result<> post_task_result = display::PostTask<kDisplayTaskTargetSize>(
      *dispatcher_->async_dispatcher(),
      [&engine_listener = engine_listener_, display_id, timestamp, driver_config_stamp]() {
        // `engine_listener` is guaranteed to be valid because the listener
        // outlives the dispatcher it's scheduled on.
        engine_listener.OnDisplayVsync(display_id, timestamp, driver_config_stamp);
      });
  if (post_task_result.is_error()) {
    fdf::error("Failed to dispatch OnDisplayVsync task: {}", post_task_result);
  }
}

void EngineListenerBanjoAdapter::DisplayEngineListenerOnCaptureComplete() {
  zx::result<> post_task_result = display::PostTask<kDisplayTaskTargetSize>(
      *dispatcher_->async_dispatcher(), [&engine_listener = engine_listener_]() {
        // `engine_listener` is guaranteed to be valid because the listener
        // outlives the dispatcher it's scheduled on.
        engine_listener.OnCaptureComplete();
      });
  if (post_task_result.is_error()) {
    fdf::error("Failed to dispatch OnCaptureComplete task: {}", post_task_result);
  }
}

display_engine_listener_protocol_t EngineListenerBanjoAdapter::GetProtocol() {
  return {
      .ops = &display_engine_listener_protocol_ops_,
      .ctx = this,
  };
}

}  // namespace display_coordinator
