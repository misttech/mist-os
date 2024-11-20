// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-gpu-display/display-engine.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/defer.h>
#include <lib/image-format/image_format.h>
#include <lib/stdcompat/span.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zircon-internal/align.h>
#include <lib/zx/bti.h>
#include <lib/zx/pmt.h>
#include <lib/zx/result.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <algorithm>
#include <cinttypes>
#include <cstdint>
#include <cstring>
#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/virtio-gpu-display/virtio-gpu-device.h"
#include "src/graphics/display/drivers/virtio-gpu-display/virtio-pci-device.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"
#include "src/graphics/lib/virtio/virtio-abi.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace virtio_display {

namespace {

constexpr uint32_t kRefreshRateHz = 30;
constexpr display::DisplayId kDisplayId{1};

}  // namespace

using imported_image_t = struct imported_image {
  uint32_t resource_id;
  zx::pmt pmt;
};

void DisplayEngine::OnCoordinatorConnected() {
  const uint32_t width = current_display_.scanout_info.geometry.width;
  const uint32_t height = current_display_.scanout_info.geometry.height;

  const int64_t pixel_clock_hz = int64_t{width} * height * kRefreshRateHz;
  ZX_DEBUG_ASSERT(pixel_clock_hz >= 0);
  ZX_DEBUG_ASSERT(pixel_clock_hz <= display::kMaxPixelClockHz);

  const display::DisplayTiming timing = {
      .horizontal_active_px = static_cast<int32_t>(width),
      .horizontal_front_porch_px = 0,
      .horizontal_sync_width_px = 0,
      .horizontal_back_porch_px = 0,
      .vertical_active_lines = static_cast<int32_t>(height),
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
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .preferred_modes_list = &banjo_display_mode,
      .preferred_modes_count = 1,
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = kSupportedFormats.data(),
      .pixel_formats_count = kSupportedFormats.size(),
  };

  coordinator_events_.OnDisplayAdded(banjo_display_info);
}

zx::result<DisplayEngine::BufferInfo> DisplayEngine::GetAllocatedBufferInfoForImage(
    display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index,
    const display::ImageMetadata& image_metadata) const {
  const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& client =
      buffer_collections_.at(driver_buffer_collection_id);
  fidl::WireResult check_result = client->CheckAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    FDF_LOG(ERROR, "CheckBuffersAllocated IPC failed: %s", check_result.status_string());
    return zx::error(check_result.status());
  }
  const auto& check_response = check_result.value();
  if (check_response.is_error()) {
    if (check_response.error_value() == fuchsia_sysmem2::Error::kPending) {
      return zx::error(ZX_ERR_SHOULD_WAIT);
    }
    const auto error_value = sysmem::V1CopyFromV2Error(check_response.error_value());
    FDF_LOG(ERROR, "CheckBuffersAllocated returned error: %s", zx_status_get_string(error_value));
    return zx::error(error_value);
  }

  auto wait_result = client->WaitForAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    FDF_LOG(ERROR, "WaitForBuffersAllocated IPC failed: %s", wait_result.status_string());
    return zx::error(wait_result.status());
  }
  auto& wait_response = wait_result.value();
  if (wait_response.is_error()) {
    if (wait_response.error_value() == fuchsia_sysmem2::Error::kPending) {
      return zx::error(ZX_ERR_SHOULD_WAIT);
    }
    const auto error_value = sysmem::V1CopyFromV2Error(wait_response.error_value());

    FDF_LOG(ERROR, "WaitForBuffersAllocated returned error: %s", zx_status_get_string(error_value));
    return zx::error(error_value);
  }
  fuchsia_sysmem2::wire::BufferCollectionInfo& collection_info =
      wait_response->buffer_collection_info();

  if (!collection_info.settings().has_image_format_constraints()) {
    FDF_LOG(ERROR, "Bad image format constraints");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (index >= collection_info.buffers().count()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  ZX_DEBUG_ASSERT(collection_info.settings().image_format_constraints().pixel_format() ==
                  fuchsia_images2::wire::PixelFormat::kB8G8R8A8);
  ZX_DEBUG_ASSERT(
      collection_info.settings().image_format_constraints().has_pixel_format_modifier());
  ZX_DEBUG_ASSERT(collection_info.settings().image_format_constraints().pixel_format_modifier() ==
                  fuchsia_images2::wire::PixelFormatModifier::kLinear);

  const auto& format_constraints = collection_info.settings().image_format_constraints();
  uint32_t minimum_row_bytes;
  if (!ImageFormatMinimumRowBytes(format_constraints, image_metadata.width(), &minimum_row_bytes)) {
    FDF_LOG(ERROR, "Invalid image width %" PRId32 " for collection", image_metadata.width());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(BufferInfo{
      .vmo = std::move(collection_info.buffers().at(index).vmo()),
      .offset = collection_info.buffers().at(index).vmo_usable_start(),
      .bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(
          PixelFormatAndModifierFromConstraints(fidl::ToNatural(format_constraints))),
      .bytes_per_row = minimum_row_bytes,
      .pixel_format = format_constraints.pixel_format(),
  });
}

zx::result<> DisplayEngine::ImportBufferCollection(
    display::DriverBufferCollectionId driver_buffer_collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) {
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    FDF_LOG(ERROR, "Buffer Collection (id=%lu) already exists",
            driver_buffer_collection_id.value());
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_.is_valid(), "sysmem allocator is not initialized");

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

  fidl::Arena arena;
  auto bind_result = sysmem_->BindSharedCollection(
      fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena)
          .token(std::move(buffer_collection_token))
          .buffer_collection_request(std::move(collection_server_endpoint))
          .Build());
  if (!bind_result.ok()) {
    FDF_LOG(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
            bind_result.status_string());
    return zx::error(ZX_ERR_INTERNAL);
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));
  return zx::ok();
}

zx::result<> DisplayEngine::ReleaseBufferCollection(
    display::DriverBufferCollectionId driver_buffer_collection_id) {
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    FDF_LOG(ERROR, "Cannot release buffer collection %lu: buffer collection doesn't exist",
            driver_buffer_collection_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  buffer_collections_.erase(driver_buffer_collection_id);
  return zx::ok();
}

zx::result<display::DriverImageId> DisplayEngine::ImportImage(
    const display::ImageMetadata& image_metadata,
    display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) {
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  zx::result<BufferInfo> buffer_info_result =
      GetAllocatedBufferInfoForImage(driver_buffer_collection_id, index, image_metadata);
  if (!buffer_info_result.is_ok()) {
    return buffer_info_result.take_error();
  }
  BufferInfo& buffer_info = buffer_info_result.value();
  return Import(std::move(buffer_info.vmo), image_metadata, buffer_info.offset,
                buffer_info.bytes_per_pixel, buffer_info.bytes_per_row, buffer_info.pixel_format);
}

zx::result<display::DriverImageId> DisplayEngine::Import(
    zx::vmo vmo, const display::ImageMetadata& image_metadata, size_t offset, uint32_t pixel_size,
    uint32_t row_bytes, fuchsia_images2::wire::PixelFormat pixel_format) {
  if (image_metadata.tiling_type() != display::ImageTilingType::kLinear) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fbl::AllocChecker ac;
  auto import_data = fbl::make_unique_checked<imported_image_t>(&ac);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  unsigned size = ZX_ROUNDUP(row_bytes * image_metadata.height(), zx_system_get_page_size());
  zx_paddr_t paddr;
  zx_status_t status = gpu_device_->bti().pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, vmo, offset,
                                              size, &paddr, 1, &import_data->pmt);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to pin VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx::result<uint32_t> create_resource_result =
      gpu_device_->Create2DResource(row_bytes / pixel_size, image_metadata.height(), pixel_format);
  if (create_resource_result.is_error()) {
    FDF_LOG(ERROR, "Failed to allocate 2D resource: %s", create_resource_result.status_string());
    return create_resource_result.take_error();
  }
  import_data->resource_id = create_resource_result.value();

  zx::result<> attach_result =
      gpu_device_->AttachResourceBacking(import_data->resource_id, paddr, size);
  if (attach_result.is_error()) {
    FDF_LOG(ERROR, "Failed to attach resource backing store: %s", attach_result.status_string());
    return attach_result.take_error();
  }

  display::DriverImageId image_id(reinterpret_cast<uint64_t>(import_data.release()));
  return zx::ok(image_id);
}

zx::result<display::DriverCaptureImageId> DisplayEngine::ImportImageForCapture(
    display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

void DisplayEngine::ReleaseImage(display::DriverImageId driver_image_id) {
  delete reinterpret_cast<imported_image_t*>(driver_image_id.value());
}

config_check_result_t DisplayEngine::CheckConfiguration(
    uint64_t display_id, cpp20::span<const layer_t> layers,
    cpp20::span<client_composition_opcode_t> out_client_composition_opcodes,
    size_t* out_client_composition_opcodes_actual) {
  ZX_DEBUG_ASSERT(display::ToDisplayId(display_id) == kDisplayId);

  ZX_DEBUG_ASSERT(out_client_composition_opcodes.size() >= layers.size());
  ZX_DEBUG_ASSERT(!out_client_composition_opcodes_actual ||
                  *out_client_composition_opcodes_actual == layers.size());

  ZX_DEBUG_ASSERT(layers.size() == 1);

  const layer_t& layer = layers[0];
  const rect_u_t display_area = {
      .x = 0,
      .y = 0,
      .width = current_display_.scanout_info.geometry.width,
      .height = current_display_.scanout_info.geometry.height,
  };

  if (std::memcmp(&layer.display_destination, &display_area, sizeof(rect_u_t)) != 0) {
    // TODO(costan): Doesn't seem right?
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (std::memcmp(&layer.image_source, &layer.display_destination, sizeof(rect_u_t)) != 0) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_FRAME_SCALE;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (layer.image_metadata.width != layer.image_source.width ||
      layer.image_metadata.height != layer.image_source.height) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_SRC_FRAME;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (layer.alpha_mode != ALPHA_DISABLE) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_TRANSFORM;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (layer.image_source_transformation != COORDINATE_TRANSFORMATION_IDENTITY) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_TRANSFORM;
    return CONFIG_CHECK_RESULT_OK;
  }

  return CONFIG_CHECK_RESULT_OK;
}

void DisplayEngine::ApplyConfiguration(uint64_t display_id, cpp20::span<const layer_t> layers,
                                       config_stamp_t config_stamp) {
  ZX_DEBUG_ASSERT(layers.size() == 1);
  uint64_t handle = layers[0].image_handle;

  {
    fbl::AutoLock al(&flush_lock_);
    latest_fb_ = reinterpret_cast<imported_image_t*>(handle);
    latest_config_stamp_ = display::ToConfigStamp(config_stamp);
  }
}

zx::result<> DisplayEngine::SetBufferCollectionConstraints(
    const display::ImageBufferUsage& image_buffer_usage,
    display::DriverBufferCollectionId driver_buffer_collection_id) {
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR,
            "SetBufferCollectionConstraints: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  fidl::Arena arena;
  auto constraints = fuchsia_sysmem2::wire::BufferCollectionConstraints::Builder(arena);
  constraints.usage(fuchsia_sysmem2::wire::BufferUsage::Builder(arena)
                        .display(fuchsia_sysmem2::wire::kDisplayUsageLayer)
                        .Build());
  constraints.buffer_memory_constraints(
      fuchsia_sysmem2::wire::BufferMemoryConstraints::Builder(arena)
          .min_size_bytes(0)
          .max_size_bytes(std::numeric_limits<uint32_t>::max())
          .physically_contiguous_required(true)
          .secure_required(false)
          .ram_domain_supported(true)
          .cpu_domain_supported(true)
          .Build());

  constraints.image_format_constraints(
      std::vector{fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena)
                      .pixel_format(fuchsia_images2::wire::PixelFormat::kB8G8R8A8)
                      .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kLinear)
                      .color_spaces(std::vector{fuchsia_images2::wire::ColorSpace::kSrgb})
                      .bytes_per_row_divisor(4)
                      .Build()});

  zx_status_t status =
      it->second
          ->SetConstraints(
              fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena)
                  .constraints(constraints.Build())
                  .Build())
          .status();

  if (status != ZX_OK) {
    FDF_LOG(ERROR, "virtio::DisplayEngine: Failed to set constraints");
    return zx::error(status);
  }

  return zx::ok();
}

bool DisplayEngine::IsCaptureSupported() { return false; }

zx::result<> DisplayEngine::SetDisplayPower(display::DisplayId display_id, bool power_on) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> DisplayEngine::StartCapture(display::DriverCaptureImageId capture_image_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> DisplayEngine::ReleaseCapture(display::DriverCaptureImageId capture_image_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> DisplayEngine::SetMinimumRgb(uint8_t minimum_rgb) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

DisplayEngine::DisplayEngine(DisplayCoordinatorEventsInterface* coordinator_events,
                             fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client,
                             std::unique_ptr<VirtioGpuDevice> gpu_device)
    : sysmem_(std::move(sysmem_client)),
      coordinator_events_(*coordinator_events),
      gpu_device_(std::move(gpu_device)) {
  ZX_DEBUG_ASSERT(coordinator_events != nullptr);
  ZX_DEBUG_ASSERT(gpu_device_);
}

DisplayEngine::~DisplayEngine() = default;

// static
zx::result<std::unique_ptr<DisplayEngine>> DisplayEngine::Create(
    fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client, zx::bti bti,
    std::unique_ptr<virtio::Backend> backend,
    DisplayCoordinatorEventsInterface* coordinator_events) {
  zx::result<std::unique_ptr<VirtioPciDevice>> virtio_device_result =
      VirtioPciDevice::Create(std::move(bti), std::move(backend));
  if (!virtio_device_result.is_ok()) {
    // VirtioPciDevice::Create() logs on error.
    return virtio_device_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto gpu_device = fbl::make_unique_checked<VirtioGpuDevice>(
      &alloc_checker, std::move(virtio_device_result).value());
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for VirtioGpuDevice");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  auto display_engine = fbl::make_unique_checked<DisplayEngine>(
      &alloc_checker, coordinator_events, std::move(sysmem_client), std::move(gpu_device));
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for DisplayEngine");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t status = display_engine->Init();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize device");
    return zx::error(status);
  }

  return zx::ok(std::move(display_engine));
}

void DisplayEngine::virtio_gpu_flusher() {
  FDF_LOG(TRACE, "Entering VirtioGpuFlusher()");

  zx_time_t next_deadline = zx_clock_get_monotonic();
  zx_time_t period = ZX_SEC(1) / kRefreshRateHz;
  for (;;) {
    zx_nanosleep(next_deadline);

    bool fb_change;
    {
      fbl::AutoLock al(&flush_lock_);
      fb_change = displayed_fb_ != latest_fb_;
      displayed_fb_ = latest_fb_;
      displayed_config_stamp_ = latest_config_stamp_;
    }

    FDF_LOG(TRACE, "flushing");

    if (fb_change) {
      uint32_t resource_id =
          displayed_fb_ ? displayed_fb_->resource_id : virtio_abi::kInvalidResourceId;
      zx::result<> set_scanout_result = gpu_device_->SetScanoutProperties(
          current_display_.scanout_id, resource_id, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (set_scanout_result.is_error()) {
        FDF_LOG(ERROR, "Failed to set scanout: %s", set_scanout_result.status_string());
        continue;
      }
    }

    if (displayed_fb_) {
      zx::result<> transfer_result = gpu_device_->TransferToHost2D(
          displayed_fb_->resource_id, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (transfer_result.is_error()) {
        FDF_LOG(ERROR, "Failed to transfer resource: %s", transfer_result.status_string());
        continue;
      }

      zx::result<> flush_result = gpu_device_->FlushResource(
          displayed_fb_->resource_id, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (flush_result.is_error()) {
        FDF_LOG(ERROR, "Failed to flush resource: %s", flush_result.status_string());
        continue;
      }
    }

    {
      fbl::AutoLock al(&flush_lock_);
      coordinator_events_.OnDisplayVsync(kDisplayId, zx::time(next_deadline),
                                         displayed_config_stamp_);
    }
    next_deadline = zx_time_add_duration(next_deadline, period);
  }
}

zx_status_t DisplayEngine::Start() {
  FDF_LOG(TRACE, "Start()");

  // Get the display info and see if we find a valid pmode
  zx::result<fbl::Vector<DisplayInfo>> display_infos_result = gpu_device_->GetDisplayInfo();
  if (display_infos_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get display info: %s", display_infos_result.status_string());
    return display_infos_result.error_value();
  }

  const DisplayInfo* current_display = FirstValidDisplay(display_infos_result.value());
  if (current_display == nullptr) {
    FDF_LOG(ERROR, "Failed to find a usable display");
    return ZX_ERR_NOT_FOUND;
  }
  current_display_ = *current_display;

  FDF_LOG(INFO,
          "Found display at (%" PRIu32 ", %" PRIu32 ") size %" PRIu32 "x%" PRIu32
          ", flags 0x%08" PRIx32,
          current_display_.scanout_info.geometry.placement_x,
          current_display_.scanout_info.geometry.placement_y,
          current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height, current_display_.scanout_info.flags);

  // Set the mouse cursor position to (0,0); the result is not critical.
  zx::result<uint32_t> move_cursor_result =
      gpu_device_->SetCursorPosition(current_display_.scanout_id, 0, 0, 0);
  if (move_cursor_result.is_error()) {
    FDF_LOG(WARNING, "Failed to move cursor: %s", move_cursor_result.status_string());
  }

  // Run a worker thread to shove in flush events
  auto virtio_gpu_flusher_entry = [](void* arg) {
    static_cast<DisplayEngine*>(arg)->virtio_gpu_flusher();
    return 0;
  };
  thrd_create(&flush_thread_, virtio_gpu_flusher_entry, this);
  thrd_detach(flush_thread_);

  FDF_LOG(TRACE, "Start() completed");
  return ZX_OK;
}

const DisplayInfo* DisplayEngine::FirstValidDisplay(cpp20::span<const DisplayInfo> display_infos) {
  return display_infos.empty() ? nullptr : &display_infos.front();
}

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

zx_status_t DisplayEngine::Init() {
  FDF_LOG(TRACE, "Init()");

  auto pid = GetKoid(zx_process_self());
  std::string debug_name = fxl::StringPrintf("virtio-gpu-display[%lu]", pid);
  fidl::Arena arena;
  auto set_debug_status = sysmem_->SetDebugClientInfo(
      fuchsia_sysmem2::wire::AllocatorSetDebugClientInfoRequest::Builder(arena)
          .name(fidl::StringView::FromExternal(debug_name))
          .id(pid)
          .Build());
  if (!set_debug_status.ok()) {
    FDF_LOG(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
    return set_debug_status.error().status();
  }

  return ZX_OK;
}

}  // namespace virtio_display
