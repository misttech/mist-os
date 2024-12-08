// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/goldfish-display/display-engine.h"

#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/fidl.h>
#include <fidl/fuchsia.math/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/time.h>
#include <lib/async/cpp/wait.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/image-format/image_format.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>
#include <zircon/status.h>

#include <algorithm>
#include <memory>
#include <numeric>
#include <vector>

#include <bind/fuchsia/goldfish/platform/sysmem/heap/cpp/bind.h>
#include <bind/fuchsia/sysmem/heap/cpp/bind.h>
#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/goldfish-display/render_control.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"

namespace goldfish {
namespace {

const char* kTag = "goldfish-display";

constexpr display::DisplayId kPrimaryDisplayId(1);

constexpr fuchsia_images2_pixel_format_enum_value_t kPixelFormats[] = {
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
};

constexpr uint32_t FB_WIDTH = 1;
constexpr uint32_t FB_HEIGHT = 2;
constexpr uint32_t FB_FPS = 5;

constexpr uint32_t GL_RGBA = 0x1908;
constexpr uint32_t GL_BGRA_EXT = 0x80E1;

}  // namespace

DisplayEngine::DisplayEngine(fidl::ClientEnd<fuchsia_hardware_goldfish::ControlDevice> control,
                             fidl::ClientEnd<fuchsia_hardware_goldfish_pipe::GoldfishPipe> pipe,
                             fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_allocator,
                             std::unique_ptr<RenderControl> render_control,
                             async_dispatcher_t* display_event_dispatcher)
    : control_(std::move(control)),
      pipe_(std::move(pipe)),
      sysmem_allocator_client_(std::move(sysmem_allocator)),
      rc_(std::move(render_control)),
      display_event_dispatcher_(std::move(display_event_dispatcher)) {
  ZX_DEBUG_ASSERT(control_.is_valid());
  ZX_DEBUG_ASSERT(pipe_.is_valid());
  ZX_DEBUG_ASSERT(sysmem_allocator_client_.is_valid());
  ZX_DEBUG_ASSERT(rc_ != nullptr);
}

DisplayEngine::~DisplayEngine() {}

zx::result<> DisplayEngine::Initialize() {
  fbl::AutoLock lock(&lock_);

  // Create primary display device.
  static constexpr int32_t kFallbackWidthPx = 1024;
  static constexpr int32_t kFallbackHeightPx = 768;
  static constexpr int32_t kFallbackRefreshRateHz = 60;
  primary_display_device_ = DisplayState{
      .width_px = rc_->GetFbParam(FB_WIDTH, kFallbackWidthPx),
      .height_px = rc_->GetFbParam(FB_HEIGHT, kFallbackHeightPx),
      .refresh_rate_hz = rc_->GetFbParam(FB_FPS, kFallbackRefreshRateHz),
  };

  // Set up display and set up flush task for each device.
  zx_status_t status = SetupPrimaryDisplay();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to set up the primary display: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = async::PostTask(display_event_dispatcher_,
                           [this] { FlushPrimaryDisplay(display_event_dispatcher_); });
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to post display flush task on the display event loop: %s",
            zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void DisplayEngine::DisplayEngineSetListener(
    const display_engine_listener_protocol_t* engine_listener) {
  const int32_t width = primary_display_device_.width_px;
  const int32_t height = primary_display_device_.height_px;
  const int32_t refresh_rate_hz = primary_display_device_.refresh_rate_hz;

  const int64_t pixel_clock_hz = int64_t{width} * height * refresh_rate_hz;
  ZX_DEBUG_ASSERT(pixel_clock_hz >= 0);
  ZX_DEBUG_ASSERT(pixel_clock_hz <= display::kMaxPixelClockHz);

  const display::DisplayTiming timing = {
      .horizontal_active_px = width,
      .horizontal_front_porch_px = 0,
      .horizontal_sync_width_px = 0,
      .horizontal_back_porch_px = 0,
      .vertical_active_lines = height,
      .vertical_front_porch_lines = 0,
      .vertical_sync_width_lines = 0,
      .vertical_back_porch_lines = 0,
      .pixel_clock_frequency_hz = pixel_clock_hz,
      .fields_per_frame = display::FieldsPerFrame::kProgressive,
      .hsync_polarity = display::SyncPolarity::kNegative,
      .vsync_polarity = display::SyncPolarity::kNegative,
      .vblank_alternates = false,
      .pixel_repetition = 0,
  };

  const display_mode_t banjo_display_mode = display::ToBanjoDisplayMode(timing);

  const raw_display_info_t banjo_display_info = {
      .display_id = display::ToBanjoDisplayId(kPrimaryDisplayId),
      .preferred_modes_list = &banjo_display_mode,
      .preferred_modes_count = 1,
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = kPixelFormats,
      .pixel_formats_count = sizeof(kPixelFormats) / sizeof(kPixelFormats[0]),
  };

  {
    fbl::AutoLock lock(&flush_lock_);
    engine_listener_ = ddk::DisplayEngineListenerProtocolClient(engine_listener);
    engine_listener_.OnDisplayAdded(&banjo_display_info);
  }
}

void DisplayEngine::DisplayEngineUnsetListener() {
  fbl::AutoLock lock(&flush_lock_);
  engine_listener_ = ddk::DisplayEngineListenerProtocolClient();
}

namespace {

uint32_t GetColorBufferFormatFromSysmemPixelFormat(
    const fuchsia_images2::PixelFormat& pixel_format) {
  switch (pixel_format) {
    case fuchsia_images2::PixelFormat::kR8G8B8A8:
      return GL_RGBA;
    case fuchsia_images2::PixelFormat::kB8G8R8A8:
      return GL_BGRA_EXT;
    default:
      // This should not happen. The sysmem-negotiated pixel format must be supported.
      ZX_ASSERT_MSG(false, "Import unsupported image: %u", static_cast<uint32_t>(pixel_format));
  }
}

}  // namespace

zx::result<display::DriverImageId> DisplayEngine::ImportVmoImage(
    const image_metadata_t& image_metadata, const fuchsia_images2::PixelFormat& pixel_format,
    zx::vmo vmo, size_t offset) {
  auto color_buffer = std::make_unique<ColorBuffer>();
  color_buffer->is_linear_format = image_metadata.tiling_type == IMAGE_TILING_TYPE_LINEAR;
  const uint32_t color_buffer_format = GetColorBufferFormatFromSysmemPixelFormat(pixel_format);

  fidl::Arena unused_arena;
  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(
      PixelFormatAndModifier(pixel_format,
                             /* ignored */
                             fuchsia_images2::PixelFormatModifier::kLinear));
  color_buffer->size = fbl::round_up(image_metadata.width * image_metadata.height * bytes_per_pixel,
                                     static_cast<uint32_t>(PAGE_SIZE));

  // Linear images must be pinned.
  color_buffer->pinned_vmo =
      rc_->pipe_io()->PinVmo(vmo, ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, offset, color_buffer->size);

  color_buffer->vmo = std::move(vmo);
  color_buffer->width = image_metadata.width;
  color_buffer->height = image_metadata.height;
  color_buffer->format = color_buffer_format;

  zx::result<HostColorBufferId> create_result =
      rc_->CreateColorBuffer(image_metadata.width, image_metadata.height, color_buffer_format);
  if (create_result.is_error()) {
    FDF_LOG(ERROR, "%s: failed to create color buffer: %s", kTag, create_result.status_string());
    return create_result.take_error();
  }
  color_buffer->host_color_buffer_id = create_result.value();

  const display::DriverImageId image_id(reinterpret_cast<uint64_t>(color_buffer.release()));
  return zx::ok(image_id);
}

zx_status_t DisplayEngine::DisplayEngineImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    FDF_LOG(ERROR, "Buffer Collection (id=%lu) already exists",
            driver_buffer_collection_id.value());
    return ZX_ERR_ALREADY_EXISTS;
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_allocator_client_.is_valid(), "sysmem allocator is not initialized");

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<::fuchsia_sysmem2::BufferCollection>::Create();

  fidl::Arena arena;
  auto bind_result = sysmem_allocator_client_->BindSharedCollection(
      fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena)
          .buffer_collection_request(std::move(collection_server_endpoint))
          .token(
              fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>(std::move(collection_token)))
          .Build());
  if (!bind_result.ok()) {
    FDF_LOG(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
            bind_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::SyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayEngineReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    FDF_LOG(ERROR, "Cannot release buffer collection %lu: buffer collection doesn't exist",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  buffer_collections_.erase(driver_buffer_collection_id);
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayEngineImportImage(const image_metadata_t* image_metadata,
                                                    uint64_t banjo_driver_buffer_collection_id,
                                                    uint32_t index, uint64_t* out_image_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  const fidl::SyncClient<fuchsia_sysmem2::BufferCollection>& collection_client = it->second;
  fidl::Result check_result = collection_client->CheckAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (check_result.is_error()) {
    const auto& error = check_result.error_value();
    if (error.is_domain_error() && error.domain_error() == fuchsia_sysmem2::Error::kPending) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return ZX_ERR_UNAVAILABLE;
  }

  fidl::Result wait_result = collection_client->WaitForAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (wait_result.is_error()) {
    const auto& error = wait_result.error_value();
    if (error.is_domain_error() && error.domain_error() == fuchsia_sysmem2::Error::kPending) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return ZX_ERR_UNAVAILABLE;
  }
  auto& wait_response = wait_result.value();
  auto& collection_info = wait_response.buffer_collection_info();

  zx::vmo vmo;
  if (index < collection_info->buffers()->size()) {
    vmo = std::move(collection_info->buffers()->at(index).vmo().value());
    ZX_DEBUG_ASSERT(!collection_info->buffers()->at(index).vmo()->is_valid());
  }

  if (!vmo.is_valid()) {
    FDF_LOG(ERROR, "%s: invalid index", kTag);
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!collection_info->settings()->image_format_constraints()) {
    FDF_LOG(ERROR, "Buffer collection doesn't have valid image format constraints");
    return ZX_ERR_NOT_SUPPORTED;
  }

  uint64_t offset = collection_info->buffers()->at(index).vmo_usable_start().value();
  if (collection_info->settings()->buffer_settings()->heap().value().heap_type() !=
      bind_fuchsia_goldfish_platform_sysmem_heap::HEAP_TYPE_DEVICE_LOCAL) {
    const auto& pixel_format =
        collection_info->settings()->image_format_constraints()->pixel_format();
    zx::result<display::DriverImageId> import_vmo_result =
        ImportVmoImage(*image_metadata, pixel_format.value(), std::move(vmo), offset);
    if (import_vmo_result.is_ok()) {
      *out_image_handle = display::ToBanjoDriverImageId(import_vmo_result.value());
      return ZX_OK;
    }
    return import_vmo_result.error_value();
  }

  if (offset != 0) {
    FDF_LOG(ERROR, "VMO offset (%lu) not supported for Goldfish device local color buffers",
            offset);
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto color_buffer = std::make_unique<ColorBuffer>();
  color_buffer->is_linear_format = image_metadata->tiling_type == IMAGE_TILING_TYPE_LINEAR;
  color_buffer->vmo = std::move(vmo);
  const display::DriverImageId image_id(reinterpret_cast<uint64_t>(color_buffer.release()));
  *out_image_handle = display::ToBanjoDriverImageId(image_id);
  return ZX_OK;
}

void DisplayEngine::DisplayEngineReleaseImage(uint64_t image_handle) {
  auto color_buffer = reinterpret_cast<ColorBuffer*>(image_handle);

  // Color buffer is owned by image in the linear case.
  if (color_buffer->is_linear_format) {
    rc_->CloseColorBuffer(color_buffer->host_color_buffer_id);
  }

  async::PostTask(display_event_dispatcher_, [this, color_buffer] {
    if (primary_display_device_.incoming_config.has_value() &&
        primary_display_device_.incoming_config->color_buffer == color_buffer) {
      primary_display_device_.incoming_config = std::nullopt;
    }
    delete color_buffer;
  });
}

config_check_result_t DisplayEngine::DisplayEngineCheckConfiguration(
    const display_config_t* display_configs, size_t display_count,
    layer_composition_operations_t* out_layer_composition_operations_list,
    size_t layer_composition_operations_count, size_t* out_layer_composition_operations_actual) {
  if (out_layer_composition_operations_actual != nullptr) {
    *out_layer_composition_operations_actual = 0;
  }

  if (display_count == 0) {
    return CONFIG_CHECK_RESULT_OK;
  }

  int total_layer_count = std::accumulate(
      display_configs, display_configs + display_count, 0,
      [](int total, const display_config_t& config) { return total += config.layer_count; });
  ZX_DEBUG_ASSERT(layer_composition_operations_count >= static_cast<size_t>(total_layer_count));
  if (out_layer_composition_operations_actual != nullptr) {
    *out_layer_composition_operations_actual = total_layer_count;
  }
  cpp20::span<layer_composition_operations_t> layer_composition_operations(
      out_layer_composition_operations_list, total_layer_count);

  config_check_result_t check_result = CONFIG_CHECK_RESULT_OK;
  int layer_composition_operations_offset = 0;
  for (unsigned i = 0; i < display_count; i++) {
    const size_t layer_count = display_configs[i].layer_count;
    cpp20::span<layer_composition_operations_t> current_display_layer_composition_operations =
        layer_composition_operations.subspan(layer_composition_operations_offset, layer_count);
    layer_composition_operations_offset += layer_count;

    const display::DisplayId display_id = display::ToDisplayId(display_configs[i].display_id);
    if (layer_count > 0) {
      ZX_DEBUG_ASSERT(display_id == kPrimaryDisplayId);

      if (display_configs[i].cc_flags != 0) {
        // Color Correction is not supported, but we will pretend we do.
        // TODO(https://fxbug.dev/42111684): Returning error will cause blank screen if scenic
        // requests color correction. For now, lets pretend we support it, until a proper fix is
        // done (either from scenic or from core display)
        FDF_LOG(WARNING, "%s: Color Correction not support. No error reported", __func__);
      }

      const layer_t& layer0 = display_configs[i].layer_list[0];
      if (layer0.image_source.width == 0 || layer0.image_source.height == 0) {
        // Solid color fill layers are not yet supported.
        current_display_layer_composition_operations[0] |= LAYER_COMPOSITION_OPERATIONS_USE_IMAGE;
      } else {
        // Scaling is allowed if destination frame match display and
        // source frame match image.
        const rect_u_t display_area = {
            .x = 0,
            .y = 0,
            .width = static_cast<uint32_t>(primary_display_device_.width_px),
            .height = static_cast<uint32_t>(primary_display_device_.height_px),
        };
        const rect_u_t image_area = {
            .x = 0,
            .y = 0,
            .width = layer0.image_metadata.width,
            .height = layer0.image_metadata.height,
        };
        if (memcmp(&layer0.display_destination, &display_area, sizeof(rect_u_t)) != 0) {
          // TODO(https://fxbug.dev/42111727): Need to provide proper flag to indicate driver only
          // accepts full screen dest frame.
          current_display_layer_composition_operations[0] |=
              LAYER_COMPOSITION_OPERATIONS_FRAME_SCALE;
          check_result = CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG;
        }
        if (memcmp(&layer0.image_source, &image_area, sizeof(rect_u_t)) != 0) {
          current_display_layer_composition_operations[0] |= LAYER_COMPOSITION_OPERATIONS_SRC_FRAME;
          check_result = CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG;
        }

        if (layer0.alpha_mode != ALPHA_DISABLE) {
          // Alpha is not supported.
          current_display_layer_composition_operations[0] |= LAYER_COMPOSITION_OPERATIONS_ALPHA;
          check_result = CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG;
        }

        if (layer0.image_source_transformation != COORDINATE_TRANSFORMATION_IDENTITY) {
          // Transformation is not supported.
          current_display_layer_composition_operations[0] |= LAYER_COMPOSITION_OPERATIONS_TRANSFORM;
          check_result = CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG;
        }
      }
      // If there is more than one layer, the rest need to be merged into the base layer.
      if (layer_count > 1) {
        current_display_layer_composition_operations[0] |= LAYER_COMPOSITION_OPERATIONS_MERGE_BASE;
        for (unsigned j = 1; j < layer_count; j++) {
          current_display_layer_composition_operations[j] |= LAYER_COMPOSITION_OPERATIONS_MERGE_SRC;
        }
        check_result = CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG;
      }
    }
  }

  return check_result;
}

zx_status_t DisplayEngine::PresentPrimaryDisplayConfig(const DisplayConfig& display_config) {
  ColorBuffer* color_buffer = display_config.color_buffer;
  if (!color_buffer) {
    return ZX_OK;
  }

  zx::eventpair event_display, event_sync_device;
  zx_status_t status = zx::eventpair::create(0u, &event_display, &event_sync_device);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: zx_eventpair_create failed: %d", kTag, status);
    return status;
  }

  // Set up async wait for the goldfish sync event. The zx::eventpair will be
  // stored in the async wait callback, which will be destroyed only when the
  // event is signaled or the wait is cancelled.
  primary_display_device_.pending_config_waits.emplace_back(event_display.get(),
                                                            ZX_EVENTPAIR_SIGNALED, 0);
  async::WaitOnce& wait = primary_display_device_.pending_config_waits.back();

  wait.Begin(
      display_event_dispatcher_,
      [this, event = std::move(event_display), pending_config_stamp = display_config.config_stamp](
          async_dispatcher_t* dispatcher, async::WaitOnce* current_wait, zx_status_t status,
          const zx_packet_signal_t*) {
        TRACE_DURATION("gfx", "DisplayEngine::SyncEventHandler", "config_stamp",
                       pending_config_stamp.value());
        if (status == ZX_ERR_CANCELED) {
          FDF_LOG(INFO, "Wait for config stamp %lu cancelled.", pending_config_stamp.value());
          return;
        }
        ZX_DEBUG_ASSERT_MSG(status == ZX_OK, "Invalid wait status: %d\n", status);

        // When the eventpair in |current_wait| is signalled, all the pending waits
        // that are queued earlier than that eventpair will be removed from the list
        // and the async WaitOnce will be cancelled.
        // Note that the cancelled waits will return early and will not reach here.
        ZX_DEBUG_ASSERT(std::any_of(primary_display_device_.pending_config_waits.begin(),
                                    primary_display_device_.pending_config_waits.end(),
                                    [current_wait](const async::WaitOnce& wait) {
                                      return wait.object() == current_wait->object();
                                    }));
        // Remove all the pending waits that are queued earlier than the current
        // wait, and the current wait itself. In WaitOnce, the callback is moved to
        // stack before current wait is removed, so it's safe to remove any item in
        // the list.
        for (auto it = primary_display_device_.pending_config_waits.begin();
             it != primary_display_device_.pending_config_waits.end();) {
          if (it->object() == current_wait->object()) {
            primary_display_device_.pending_config_waits.erase(it);
            break;
          }
          it = primary_display_device_.pending_config_waits.erase(it);
        }
        primary_display_device_.latest_config_stamp =
            std::max(primary_display_device_.latest_config_stamp, pending_config_stamp);
      });

  // Update host-writeable display buffers before presenting.
  if (color_buffer->pinned_vmo.region_count() > 0) {
    auto status = rc_->UpdateColorBuffer(
        color_buffer->host_color_buffer_id, color_buffer->pinned_vmo, color_buffer->width,
        color_buffer->height, color_buffer->format, color_buffer->size);
    if (status.is_error() || status.value()) {
      FDF_LOG(ERROR, "%s : color buffer update failed: %d:%u", kTag, status.status_value(),
              status.value_or(0));
      return status.is_error() ? status.status_value() : ZX_ERR_INTERNAL;
    }
  }

  // Present the buffer.
  {
    zx_status_t status = rc_->FbPost(color_buffer->host_color_buffer_id);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to call render control command FbPost: %s",
              zx_status_get_string(status));
      return status;
    }

    fbl::AutoLock lock(&lock_);
    fidl::WireResult result = control_->CreateSyncFence(std::move(event_sync_device));
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to call FIDL CreateSyncFence: %s",
              result.error().FormatDescription().c_str());
      return status;
    }
    if (result.value().is_error()) {
      FDF_LOG(ERROR, "Failed to create SyncFence: %s",
              zx_status_get_string(result.value().error_value()));
      return result.value().error_value();
    }
  }

  return ZX_OK;
}

void DisplayEngine::DisplayEngineApplyConfiguration(const display_config_t* display_configs,
                                                    size_t display_count,
                                                    const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(banjo_config_stamp != nullptr);
  display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);
  display::DriverImageId driver_image_id = display::kInvalidDriverImageId;

  cpp20::span<const display_config_t> display_configs_span(display_configs, display_count);
  for (const display_config& display_config : display_configs_span) {
    if (display::ToDisplayId(display_config.display_id) == kPrimaryDisplayId) {
      if (display_config.layer_count) {
        driver_image_id = display::ToDriverImageId(display_config.layer_list[0].image_handle);
      }
      break;
    }
  }

  if (driver_image_id == display::kInvalidDriverImageId) {
    // The display doesn't have any active layers right now. For layers that
    // previously existed, we should cancel waiting events on the pending
    // color buffer and remove references to both pending and current color
    // buffers.
    async::PostTask(display_event_dispatcher_, [this, config_stamp] {
      primary_display_device_.pending_config_waits.clear();
      primary_display_device_.incoming_config = std::nullopt;
      primary_display_device_.latest_config_stamp =
          std::max(primary_display_device_.latest_config_stamp, config_stamp);
    });
    return;
  }

  ColorBuffer* color_buffer =
      reinterpret_cast<ColorBuffer*>(display::ToBanjoDriverImageId(driver_image_id));
  ZX_DEBUG_ASSERT(color_buffer != nullptr);
  if (color_buffer->host_color_buffer_id == kInvalidHostColorBufferId) {
    zx::vmo vmo;

    zx_status_t status = color_buffer->vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to duplicate vmo: %s", zx_status_get_string(status));
      return;
    }

    {
      fbl::AutoLock lock(&lock_);

      fidl::WireResult result = control_->GetBufferHandle(std::move(vmo));
      if (!result.ok()) {
        FDF_LOG(ERROR, "Failed to call FIDL GetBufferHandle: %s",
                result.error().FormatDescription().c_str());
        return;
      }
      if (result.value().res != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get ColorBuffer handle: %s",
                zx_status_get_string(result.value().res));
        return;
      }
      if (result.value().type != fuchsia_hardware_goldfish::BufferHandleType::kColorBuffer) {
        FDF_LOG(ERROR, "Buffer handle type invalid. Expected ColorBuffer, actual %" PRIu32,
                static_cast<uint32_t>(result.value().type));
        return;
      }

      uint32_t render_control_encoded_color_buffer_id = result.value().id;
      color_buffer->host_color_buffer_id =
          ToHostColorBufferId(render_control_encoded_color_buffer_id);

      // Color buffers are in vulkan-only mode by default as that avoids
      // unnecessary copies on the host in some cases. The color buffer
      // needs to be moved out of vulkan-only mode before being used for
      // presentation.
      if (color_buffer->host_color_buffer_id != kInvalidHostColorBufferId) {
        static constexpr uint32_t kVulkanGlSharedMode = 0;
        zx::result<RenderControl::RcResult> status = rc_->SetColorBufferVulkanMode(
            color_buffer->host_color_buffer_id, /*mode=*/kVulkanGlSharedMode);
        if (status.is_error()) {
          FDF_LOG(ERROR, "Failed to call render control SetColorBufferVulkanMode: %s",
                  status.status_string());
        }
        if (status.value() != 0) {
          FDF_LOG(ERROR, "Render control host failed to set vulkan mode: %d", status.value());
        }
      }
    }
  }

  async::PostTask(display_event_dispatcher_, [this, config_stamp, color_buffer] {
    primary_display_device_.incoming_config = {
        .color_buffer = color_buffer,
        .config_stamp = config_stamp,
    };
  });
}

zx_status_t DisplayEngine::DisplayEngineSetBufferCollectionConstraints(
    const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::SyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  auto constraints =
      fuchsia_sysmem2::BufferCollectionConstraints()
          .usage(fuchsia_sysmem2::BufferUsage().display(fuchsia_sysmem2::kDisplayUsageLayer))
          .buffer_memory_constraints(
              fuchsia_sysmem2::BufferMemoryConstraints()
                  .min_size_bytes(0)
                  .max_size_bytes(0xFFFFFFFF)
                  .physically_contiguous_required(true)
                  .secure_required(false)
                  .ram_domain_supported(true)
                  .cpu_domain_supported(true)
                  .inaccessible_domain_supported(true)
                  .permitted_heaps(std::vector{
                      fuchsia_sysmem2::Heap()
                          .heap_type(bind_fuchsia_sysmem_heap::HEAP_TYPE_SYSTEM_RAM)
                          .id(0),
                      fuchsia_sysmem2::Heap()
                          .heap_type(
                              bind_fuchsia_goldfish_platform_sysmem_heap::HEAP_TYPE_DEVICE_LOCAL)
                          .id(0),

                  }));
  std::vector<fuchsia_sysmem2::ImageFormatConstraints> image_constraints_vec;
  for (uint32_t i = 0; i < 4; i++) {
    auto image_constraints =
        fuchsia_sysmem2::ImageFormatConstraints()
            .pixel_format(i & 0b01 ? fuchsia_images2::PixelFormat::kR8G8B8A8
                                   : fuchsia_images2::PixelFormat::kB8G8R8A8)

            .pixel_format_modifier(
                i & 0b10 ? fuchsia_images2::PixelFormatModifier::kLinear
                         : fuchsia_images2::PixelFormatModifier::kGoogleGoldfishOptimal)
            .color_spaces(std::vector{fuchsia_images2::ColorSpace::kSrgb})
            .min_size(fuchsia_math::SizeU().width(0).height(0))
            .max_size(fuchsia_math::SizeU().width(0xFFFFFFFF).height(0xFFFFFFFF))
            .min_bytes_per_row(0)
            .max_bytes_per_row(0xFFFFFFFF)
            .max_width_times_height(0xFFFFFFFF)
            .bytes_per_row_divisor(1)
            .start_offset_divisor(1)
            .display_rect_alignment(fuchsia_math::SizeU().width(1).height(1));
    image_constraints_vec.push_back(std::move(image_constraints));
  }
  constraints.image_format_constraints(std::move(image_constraints_vec));

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest request;
  request.constraints(std::move(constraints));
  auto set_result = collection->SetConstraints(std::move(request));
  if (set_result.is_error()) {
    FDF_LOG(ERROR, "%s: failed to set constraints", kTag);
    return set_result.error_value().status();
  }

  return ZX_OK;
}

zx_status_t DisplayEngine::SetupPrimaryDisplay() {
  // On the host render control protocol, the "invalid" host display ID is used
  // to configure the primary display device.
  const HostDisplayId kPrimaryHostDisplayId = kInvalidHostDisplayId;
  zx::result<RenderControl::RcResult> status =
      rc_->SetDisplayPose(kPrimaryHostDisplayId, /*x=*/0, /*y=*/0, primary_display_device_.width_px,
                          primary_display_device_.height_px);
  if (status.is_error()) {
    FDF_LOG(ERROR, "Failed to call render control SetDisplayPose command: %s",
            status.status_string());
    return status.error_value();
  }
  if (status.value() != 0) {
    FDF_LOG(ERROR, "Render control host failed to set display pose: %d", status.value());
    return ZX_ERR_INTERNAL;
  }
  primary_display_device_.expected_next_flush = async::Now(display_event_dispatcher_);

  return ZX_OK;
}

void DisplayEngine::FlushPrimaryDisplay(async_dispatcher_t* dispatcher) {
  zx::duration period = zx::sec(1) / primary_display_device_.refresh_rate_hz;
  zx::time expected_next_flush = primary_display_device_.expected_next_flush + period;

  if (primary_display_device_.incoming_config.has_value()) {
    zx_status_t status = PresentPrimaryDisplayConfig(*primary_display_device_.incoming_config);
    ZX_DEBUG_ASSERT(status == ZX_OK || status == ZX_ERR_SHOULD_WAIT);
  }

  {
    fbl::AutoLock lock(&flush_lock_);

    if (engine_listener_.is_valid()) {
      zx::time now = async::Now(dispatcher);
      const uint64_t banjo_display_id = display::ToBanjoDisplayId(kPrimaryDisplayId);
      const config_stamp_t banjo_config_stamp =
          display::ToBanjoConfigStamp(primary_display_device_.latest_config_stamp);
      engine_listener_.OnDisplayVsync(banjo_display_id, now.get(), &banjo_config_stamp);
    }
  }

  // If we've already passed the |expected_next_flush| deadline, skip the
  // Vsync and adjust the deadline to the earliest next available frame.
  zx::time now = async::Now(dispatcher);
  if (now > expected_next_flush) {
    expected_next_flush +=
        period * (((now - expected_next_flush + period).get() - 1L) / period.get());
  }

  primary_display_device_.expected_next_flush = expected_next_flush;
  async::PostTaskForTime(
      dispatcher, [this, dispatcher] { FlushPrimaryDisplay(dispatcher); }, expected_next_flush);
}

void DisplayEngine::SetupPrimaryDisplayForTesting(int32_t width_px, int32_t height_px,
                                                  int32_t refresh_rate_hz) {
  primary_display_device_ = {
      .width_px = width_px,
      .height_px = height_px,
      .refresh_rate_hz = refresh_rate_hz,
  };
}

}  // namespace goldfish
