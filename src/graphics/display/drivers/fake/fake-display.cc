// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-display.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <array>
#include <atomic>
#include <cinttypes>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <iterator>
#include <limits>
#include <string>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/preferred-scanout-image-type.h"
#include "src/graphics/display/drivers/fake/image-info.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace fake_display {

namespace {
// List of supported pixel formats
constexpr fuchsia_images2_pixel_format_enum_value_t kSupportedPixelFormats[] = {
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
};

// Arbitrary dimensions - the same as sherlock.
constexpr int32_t kWidth = 1280;
constexpr int32_t kHeight = 800;

constexpr display::DisplayId kDisplayId(1);

constexpr int32_t kRefreshRateFps = 60;

// Arbitrary slowdown for testing purposes
// TODO(payamm): Randomizing the delay value is more value
constexpr int64_t kNumOfVsyncsForCapture = 5;  // 5 * 16ms = 80ms

display_mode_t CreateBanjoDisplayMode() {
  static constexpr int64_t kPixelClockFrequencyHz = int64_t{kWidth} * kHeight * kRefreshRateFps;
  static_assert(kPixelClockFrequencyHz >= 0);
  static_assert(kPixelClockFrequencyHz <= display::kMaxPixelClockHz);

  static constexpr display::DisplayTiming kDisplayTiming = {
      .horizontal_active_px = kWidth,
      .horizontal_front_porch_px = 0,
      .horizontal_sync_width_px = 0,
      .horizontal_back_porch_px = 0,
      .vertical_active_lines = kHeight,
      .vertical_front_porch_lines = 0,
      .vertical_sync_width_lines = 0,
      .vertical_back_porch_lines = 0,
      .pixel_clock_frequency_hz = kPixelClockFrequencyHz,
      .fields_per_frame = display::FieldsPerFrame::kProgressive,
      .hsync_polarity = display::SyncPolarity::kNegative,
      .vsync_polarity = display::SyncPolarity::kNegative,
      .vblank_alternates = false,
      .pixel_repetition = 0,
  };

  return display::ToBanjoDisplayMode(kDisplayTiming);
}

// `banjo_display_mode` must outlive the returned value.
raw_display_info_t CreateRawDisplayInfo(const display_mode_t* banjo_display_mode) {
  raw_display_info_t args = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .preferred_modes_list = banjo_display_mode,
      .preferred_modes_count = 1,
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = kSupportedPixelFormats,
      .pixel_formats_count = std::size(kSupportedPixelFormats),
  };
  return args;
}

}  // namespace

FakeDisplay::FakeDisplay(FakeDisplayDeviceConfig device_config,
                         fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_allocator,
                         inspect::Inspector inspector)
    : display_engine_banjo_protocol_({&display_engine_protocol_ops_, this}),
      device_config_(device_config),
      sysmem_(std::move(sysmem_allocator)),
      inspector_(std::move(inspector)) {}

FakeDisplay::~FakeDisplay() { Deinitialize(); }

zx_status_t FakeDisplay::DisplayEngineSetMinimumRgb(uint8_t minimum_rgb) {
  fbl::AutoLock lock(&capture_mutex_);

  clamp_rgb_value_ = minimum_rgb;
  return ZX_OK;
}

zx_status_t FakeDisplay::InitSysmemAllocatorClient() {
  std::string debug_name = fxl::StringPrintf("fake-display[%lu]", fsl::GetCurrentProcessKoid());
  fuchsia_sysmem2::AllocatorSetDebugClientInfoRequest request;
  request.name() = debug_name;
  request.id() = fsl::GetCurrentProcessKoid();
  auto set_debug_result = sysmem_->SetDebugClientInfo(std::move(request));
  if (!set_debug_result.is_ok()) {
    FDF_LOG(ERROR, "Cannot set sysmem allocator debug info: %s",
            set_debug_result.error_value().status_string());
    return set_debug_result.error_value().status();
  }
  return ZX_OK;
}

void FakeDisplay::DisplayEngineRegisterDisplayEngineListener(
    const display_engine_listener_protocol_t* engine_listener) {
  fbl::AutoLock engine_listener_lock(&engine_listener_mutex_);
  engine_listener_client_ = ddk::DisplayEngineListenerProtocolClient(engine_listener);

  const display_mode_t banjo_display_mode = CreateBanjoDisplayMode();
  const raw_display_info_t banjo_display_info = CreateRawDisplayInfo(&banjo_display_mode);
  engine_listener_client_.OnDisplayAdded(&banjo_display_info);
}

void FakeDisplay::DisplayEngineDeregisterDisplayEngineListener() {
  fbl::AutoLock engine_listener_lock(&engine_listener_mutex_);
  engine_listener_client_ = ddk::DisplayEngineListenerProtocolClient();
}

zx::result<display::DriverImageId> FakeDisplay::ImportVmoImageForTesting(zx::vmo vmo,
                                                                         size_t offset) {
  fbl::AllocChecker alloc_checker;
  fbl::AutoLock lock(&image_mutex_);

  display::DriverImageId driver_image_id = next_imported_display_driver_image_id_++;
  // Image metadata for testing only and may not reflect the actual image
  // buffer format.
  ImageMetadata display_image_metadata = {
      .pixel_format = fuchsia_images2::PixelFormat::kB8G8R8A8,
      .coherency_domain = fuchsia_sysmem2::CoherencyDomain::kCpu,
  };

  auto import_info = fbl::make_unique_checked<DisplayImageInfo>(
      &alloc_checker, driver_image_id, display_image_metadata, std::move(vmo));
  if (!alloc_checker.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  imported_images_.insert(std::move(import_info));
  return zx::ok(driver_image_id);
}

namespace {

bool IsAcceptableImageTilingType(uint32_t image_tiling_type) {
  return image_tiling_type == IMAGE_TILING_TYPE_PREFERRED_SCANOUT ||
         image_tiling_type == IMAGE_TILING_TYPE_LINEAR;
}

}  // namespace

zx_status_t FakeDisplay::DisplayEngineImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    FDF_LOG(ERROR, "Buffer Collection (id=%lu) already exists",
            driver_buffer_collection_id.value());
    return ZX_ERR_ALREADY_EXISTS;
  }

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

  fuchsia_sysmem2::AllocatorBindSharedCollectionRequest bind_request;
  bind_request.token() =
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>(std::move(collection_token));
  bind_request.buffer_collection_request() = std::move(collection_server_endpoint);
  auto bind_result = sysmem_->BindSharedCollection(std::move(bind_request));
  if (bind_result.is_error()) {
    FDF_LOG(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
            bind_result.error_value().status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::SyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t FakeDisplay::DisplayEngineReleaseBufferCollection(
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

zx_status_t FakeDisplay::DisplayEngineImportImage(const image_metadata_t* image_metadata,
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
  const fidl::SyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  fbl::AutoLock lock(&image_mutex_);
  if (!IsAcceptableImageTilingType(image_metadata->tiling_type)) {
    FDF_LOG(INFO, "ImportImage() will fail due to invalid Image tiling type %" PRIu32,
            image_metadata->tiling_type);
    return ZX_ERR_INVALID_ARGS;
  }

  auto check_result = collection->CheckAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (check_result.is_error()) {
    if (check_result.error_value().is_framework_error()) {
      return check_result.error_value().framework_error().status();
    }
    fuchsia_sysmem2::Error check_error = check_result.error_value().domain_error();
    if (check_error == fuchsia_sysmem2::Error::kPending) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return sysmem::V1CopyFromV2Error(check_error);
  }

  auto wait_result = collection->WaitForAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (wait_result.is_error()) {
    if (wait_result.error_value().is_framework_error()) {
      return wait_result.error_value().framework_error().status();
    }
    fuchsia_sysmem2::Error wait_error = wait_result.error_value().domain_error();
    return sysmem::V1CopyFromV2Error(wait_error);
  }
  auto& wait_response = wait_result.value();
  auto& collection_info = wait_response.buffer_collection_info();

  fbl::Vector<zx::vmo> vmos;
  for (auto& buffer : *collection_info->buffers()) {
    vmos.push_back(std::move(buffer.vmo().value()));
  }

  if (!collection_info->settings()->image_format_constraints().has_value() ||
      index >= vmos.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // TODO(https://fxbug.dev/42079320): When capture is enabled
  // (IsCaptureSupported() is true), we should perform a check to ensure that
  // the display images should not be of "inaccessible" coherency domain.

  display::DriverImageId driver_image_id = next_imported_display_driver_image_id_++;
  ImageMetadata display_image_metadata = {
      .pixel_format =
          collection_info->settings()->image_format_constraints()->pixel_format().value(),
      .coherency_domain =
          collection_info->settings()->buffer_settings()->coherency_domain().value(),
  };

  fbl::AllocChecker alloc_checker;
  auto import_info = fbl::make_unique_checked<DisplayImageInfo>(
      &alloc_checker, driver_image_id, std::move(display_image_metadata), std::move(vmos[index]));
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out_image_handle = display::ToBanjoDriverImageId(driver_image_id);
  imported_images_.insert(std::move(import_info));
  return ZX_OK;
}

void FakeDisplay::DisplayEngineReleaseImage(uint64_t image_handle) {
  fbl::AutoLock lock(&image_mutex_);
  display::DriverImageId driver_image_id = display::ToDriverImageId(image_handle);
  if (imported_images_.erase(driver_image_id) == nullptr) {
    FDF_LOG(ERROR, "Failed to release display Image (handle %" PRIu64 ")", driver_image_id.value());
  }
}

config_check_result_t FakeDisplay::DisplayEngineCheckConfiguration(
    const display_config_t* display_configs, size_t display_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual) {
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = 0;
  }

  if (display_count != 1) {
    ZX_DEBUG_ASSERT(display_count == 0);
    return CONFIG_CHECK_RESULT_OK;
  }
  ZX_DEBUG_ASSERT(display::ToDisplayId(display_configs[0].display_id) == kDisplayId);

  ZX_DEBUG_ASSERT(client_composition_opcodes_count >= display_configs[0].layer_count);
  cpp20::span<client_composition_opcode_t> client_composition_opcodes(
      out_client_composition_opcodes_list, display_configs[0].layer_count);
  std::fill(client_composition_opcodes.begin(), client_composition_opcodes.end(), 0);
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = client_composition_opcodes.size();
  }

  bool success;
  if (display_configs[0].layer_count != 1) {
    success = display_configs[0].layer_count == 0;
  } else {
    const primary_layer_t& layer = display_configs[0].layer_list[0].cfg.primary;
    const rect_u_t display_area = {.x = 0, .y = 0, .width = kWidth, .height = kHeight};
    success = display_configs[0].layer_list[0].type == LAYER_TYPE_PRIMARY &&
              layer.image_source_transformation == COORDINATE_TRANSFORMATION_IDENTITY &&
              layer.image_metadata.width == kWidth && layer.image_metadata.height == kHeight &&
              memcmp(&layer.display_destination, &display_area, sizeof(rect_u_t)) == 0 &&
              memcmp(&layer.image_source, &display_area, sizeof(rect_u_t)) == 0 &&
              layer.alpha_mode == ALPHA_DISABLE;
  }
  if (!success) {
    client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
    for (unsigned i = 1; i < display_configs[0].layer_count; i++) {
      client_composition_opcodes[i] = CLIENT_COMPOSITION_OPCODE_MERGE_SRC;
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

void FakeDisplay::DisplayEngineApplyConfiguration(const display_config_t* display_configs,
                                                  size_t display_count,
                                                  const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(display_configs);
  ZX_DEBUG_ASSERT(banjo_config_stamp != nullptr);
  {
    fbl::AutoLock lock(&image_mutex_);
    if (display_count == 1 && display_configs[0].layer_count) {
      // Only support one display.
      current_image_to_capture_id_ =
          display::ToDriverImageId(display_configs[0].layer_list[0].cfg.primary.image_handle);
    } else {
      current_image_to_capture_id_ = display::kInvalidDriverImageId;
    }
  }

  // The `current_config_stamp_` is stored by ApplyConfiguration() on the
  // display coordinator's controller loop thread, and loaded only by the
  // driver Vsync thread to notify coordinator for a new frame being displayed.
  // After that, captures will be triggered on the controller loop thread by
  // StartCapture(), which is synchronized with the capture thread (via mutex).
  //
  // Thus, for `current_config_stamp_`, there's no need for acquire-release
  // memory model (to synchronize `current_image_to_capture_` changes between
  // controller loop thread and Vsync thread), and relaxed memory order should
  // be sufficient to guarantee that both value changes in ApplyConfiguration()
  // will be visible to the capture thread for captures triggered after the
  // Vsync with this config stamp.
  //
  // As long as a client requests a capture after it sees the Vsync event of a
  // given config, the captured contents can only be contents applied no earlier
  // than that config (which can be that config itself, or a config applied
  // after that config).
  const display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);
  current_config_stamp_.store(config_stamp, std::memory_order_relaxed);
}

enum class FakeDisplay::BufferCollectionUsage {
  kPrimaryLayer = 1,
  kCapture = 2,
};

fuchsia_sysmem2::BufferCollectionConstraints FakeDisplay::CreateBufferCollectionConstraints(
    BufferCollectionUsage usage) {
  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  switch (usage) {
    case BufferCollectionUsage::kCapture:
      constraints.usage().emplace();
      constraints.usage()->cpu() =
          fuchsia_sysmem2::kCpuUsageReadOften | fuchsia_sysmem2::kCpuUsageWriteOften;
      break;
    case BufferCollectionUsage::kPrimaryLayer:
      constraints.usage().emplace();
      constraints.usage()->display() = fuchsia_sysmem2::kDisplayUsageLayer;
      break;
  }

  // TODO(https://fxbug.dev/42079320): In order to support capture, both capture sources
  // and capture targets must not be in the "inaccessible" coherency domain.
  constraints.buffer_memory_constraints().emplace();
  SetBufferMemoryConstraints(constraints.buffer_memory_constraints().value());

  // When we have C++20, we can use std::to_array to avoid specifying the array
  // size twice.
  static constexpr std::array<fuchsia_images2::PixelFormat, 2> kPixelFormats = {
      fuchsia_images2::PixelFormat::kR8G8B8A8, fuchsia_images2::PixelFormat::kB8G8R8A8};
  static constexpr std::array<fuchsia_images2::PixelFormatModifier, 2> kFormatModifiers = {
      fuchsia_images2::PixelFormatModifier::kLinear,
      fuchsia_images2::PixelFormatModifier::kGoogleGoldfishOptimal};

  constraints.image_format_constraints().emplace();
  for (auto pixel_format : kPixelFormats) {
    for (auto format_modifier : kFormatModifiers) {
      fuchsia_sysmem2::ImageFormatConstraints& image_constraints =
          constraints.image_format_constraints()->emplace_back();

      SetCommonImageFormatConstraints(pixel_format, format_modifier, image_constraints);
      switch (usage) {
        case BufferCollectionUsage::kCapture:
          SetCaptureImageFormatConstraints(image_constraints);
          break;
        case BufferCollectionUsage::kPrimaryLayer:
          SetLayerImageFormatConstraints(image_constraints);
          break;
      }
    }
  }
  return constraints;
}

void FakeDisplay::SetBufferMemoryConstraints(
    fuchsia_sysmem2::BufferMemoryConstraints& constraints) {
  constraints.min_size_bytes() = 0;
  constraints.max_size_bytes() = std::numeric_limits<uint32_t>::max();
  constraints.physically_contiguous_required() = false;
  constraints.secure_required() = false;
  constraints.ram_domain_supported() = true;
  constraints.cpu_domain_supported() = true;
  constraints.inaccessible_domain_supported() = true;
}

void FakeDisplay::SetCommonImageFormatConstraints(
    fuchsia_images2::PixelFormat pixel_format, fuchsia_images2::PixelFormatModifier format_modifier,
    fuchsia_sysmem2::ImageFormatConstraints& constraints) {
  constraints.pixel_format() = pixel_format;
  constraints.pixel_format_modifier() = format_modifier;

  constraints.color_spaces() = {fuchsia_images2::ColorSpace::kSrgb};

  constraints.size_alignment() = {1, 1};
  constraints.bytes_per_row_divisor() = 1;
  constraints.start_offset_divisor() = 1;
  constraints.display_rect_alignment() = {1, 1};
}

void FakeDisplay::SetCaptureImageFormatConstraints(
    fuchsia_sysmem2::ImageFormatConstraints& constraints) {
  constraints.min_size() = {kWidth, kHeight};
  constraints.max_size() = {kWidth, kHeight};
  constraints.min_bytes_per_row() = kWidth * 4;
  constraints.max_bytes_per_row() = kWidth * 4;
  constraints.max_width_times_height() = kWidth * kHeight;
}

void FakeDisplay::SetLayerImageFormatConstraints(
    fuchsia_sysmem2::ImageFormatConstraints& constraints) {
  constraints.min_size() = {0, 0};
  constraints.max_size() = {std::numeric_limits<uint32_t>::max(),
                            std::numeric_limits<uint32_t>::max()};
  constraints.min_bytes_per_row() = 0;
  constraints.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  constraints.max_width_times_height() = std::numeric_limits<uint32_t>::max();
}

zx_status_t FakeDisplay::DisplayEngineSetBufferCollectionConstraints(
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

  BufferCollectionUsage buffer_collection_usage = (usage->tiling_type == IMAGE_TILING_TYPE_CAPTURE)
                                                      ? BufferCollectionUsage::kCapture
                                                      : BufferCollectionUsage::kPrimaryLayer;

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest request;
  request.constraints() = CreateBufferCollectionConstraints(buffer_collection_usage);
  auto set_result = collection->SetConstraints(std::move(request));
  if (set_result.is_error()) {
    FDF_LOG(ERROR, "Failed to set constraints on a sysmem BufferCollection: %s",
            set_result.error_value().status_string());
    return set_result.error_value().status();
  }

  return ZX_OK;
}

zx_status_t FakeDisplay::DisplayEngineSetDisplayPower(uint64_t display_id, bool power_on) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t FakeDisplay::DisplayEngineImportImageForCapture(
    uint64_t banjo_driver_buffer_collection_id, uint32_t index, uint64_t* out_capture_handle) {
  if (!IsCaptureSupported()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::SyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  fbl::AutoLock lock(&capture_mutex_);

  auto check_result = collection->CheckAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (check_result.is_error()) {
    if (check_result.error_value().is_framework_error()) {
      return check_result.error_value().framework_error().status();
    }
    fuchsia_sysmem2::Error check_error = check_result.error_value().domain_error();
    if (check_error == fuchsia_sysmem2::Error::kPending) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return sysmem::V1CopyFromV2Error(check_error);
  }

  auto wait_result = collection->WaitForAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (wait_result.is_error()) {
    if (wait_result.error_value().is_framework_error()) {
      return wait_result.error_value().framework_error().status();
    }
    fuchsia_sysmem2::Error wait_error = wait_result.error_value().domain_error();
    return sysmem::V1CopyFromV2Error(wait_error);
  }
  auto& wait_response = wait_result.value();
  fuchsia_sysmem2::BufferCollectionInfo& collection_info =
      wait_response.buffer_collection_info().value();

  if (!collection_info.settings()->image_format_constraints().has_value()) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (index >= collection_info.buffers()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // TODO(https://fxbug.dev/42079320): Capture target images should not be of
  // "inaccessible" coherency domain. We should add a check here.
  display::DriverCaptureImageId driver_capture_image_id = next_imported_driver_capture_image_id_++;
  ImageMetadata capture_image_metadata = {
      .pixel_format =
          collection_info.settings()->image_format_constraints()->pixel_format().value(),
      .coherency_domain = collection_info.settings()->buffer_settings()->coherency_domain().value(),
  };
  zx::vmo vmo = std::move(collection_info.buffers()->at(index).vmo().value());

  fbl::AllocChecker alloc_checker;
  auto capture_image_info = fbl::make_unique_checked<CaptureImageInfo>(
      &alloc_checker, driver_capture_image_id, std::move(capture_image_metadata), std::move(vmo));
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out_capture_handle = display::ToBanjoDriverCaptureImageId(driver_capture_image_id);
  imported_captures_.insert(std::move(capture_image_info));
  return ZX_OK;
}

bool FakeDisplay::DisplayEngineIsCaptureSupported() { return IsCaptureSupported(); }

zx_status_t FakeDisplay::DisplayEngineStartCapture(uint64_t capture_handle) {
  if (!IsCaptureSupported()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  fbl::AutoLock lock(&capture_mutex_);
  if (current_capture_target_image_id_ != display::kInvalidDriverCaptureImageId) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Confirm the handle was previously imported (hence valid)
  display::DriverCaptureImageId driver_capture_image_id =
      display::ToDriverCaptureImageId(capture_handle);
  auto it = imported_captures_.find(driver_capture_image_id);
  if (it == imported_captures_.end()) {
    // invalid handle
    return ZX_ERR_INVALID_ARGS;
  }
  current_capture_target_image_id_ = driver_capture_image_id;

  return ZX_OK;
}

zx_status_t FakeDisplay::DisplayEngineReleaseCapture(uint64_t capture_handle) {
  if (!IsCaptureSupported()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  fbl::AutoLock lock(&capture_mutex_);
  display::DriverCaptureImageId driver_capture_image_id =
      display::ToDriverCaptureImageId(capture_handle);
  if (current_capture_target_image_id_ == driver_capture_image_id) {
    return ZX_ERR_SHOULD_WAIT;
  }

  if (imported_captures_.erase(driver_capture_image_id) == nullptr) {
    // invalid handle
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t FakeDisplay::InitializeCapture() {
  {
    fbl::AutoLock image_lock(&image_mutex_);
    current_image_to_capture_id_ = display::kInvalidDriverImageId;
  }

  if (IsCaptureSupported()) {
    fbl::AutoLock capture_lock(&capture_mutex_);
    current_capture_target_image_id_ = display::kInvalidDriverCaptureImageId;
  }
  return ZX_OK;
}

bool FakeDisplay::IsCaptureSupported() const { return !device_config_.no_buffer_access; }

int FakeDisplay::CaptureThread() {
  ZX_DEBUG_ASSERT(IsCaptureSupported());
  while (true) {
    zx::nanosleep(zx::deadline_after(zx::sec(1) / kRefreshRateFps));
    if (capture_shutdown_flag_.load()) {
      break;
    }
    {
      fbl::AutoLock engine_listene(&engine_listener_mutex_);
      // `current_capture_target_image_` is a pointer to DisplayImageInfo stored in
      // `imported_captures_` (guarded by `capture_mutex_`). So `capture_mutex_`
      // must be locked while the capture image is being used.
      fbl::AutoLock capture_lock(&capture_mutex_);
      if (engine_listener_client_.is_valid() &&
          (current_capture_target_image_id_ != display::kInvalidDriverCaptureImageId) &&
          ++capture_complete_signal_count_ >= kNumOfVsyncsForCapture) {
        {
          auto dst = imported_captures_.find(current_capture_target_image_id_);

          // `dst` should be always valid.
          //
          // The current capture target image is guaranteed to be valid in
          // `imported_captures_` in StartCapture(), and can only be released
          // (i.e. removed from `imported_captures_`) after the active capture
          // is done.
          ZX_DEBUG_ASSERT(dst.IsValid());

          // `current_image_to_capture_id_` is a key to DisplayImageInfo stored
          // in `imported_images_` (guarded by `image_mutex_`). So `image_mutex_`
          // must be locked while the source image is being used.
          fbl::AutoLock image_lock(&image_mutex_);
          if (current_image_to_capture_id_ != display::kInvalidDriverImageId) {
            // We have a valid image being displayed. Let's capture it.
            auto src = imported_images_.find(current_image_to_capture_id_);

            // `src` should be always valid.
            //
            // The "current image to capture" is guaranteed to be valid in
            // `imported_images_` in ApplyConfig(), and can only be released
            // (i.e. removed from `imported_images_`) after there's an vsync
            // not containing the image anymore.
            ZX_ASSERT(src.IsValid());

            if (src->metadata().pixel_format != dst->metadata().pixel_format) {
              FDF_LOG(ERROR, "Trying to capture format=%u as format=%u\n",
                      static_cast<uint32_t>(src->metadata().pixel_format),
                      static_cast<uint32_t>(dst->metadata().pixel_format));
              continue;
            }
            size_t src_vmo_size;
            auto status = src->vmo().get_size(&src_vmo_size);
            if (status != ZX_OK) {
              FDF_LOG(ERROR, "Failed to get the size of the displayed image VMO: %s",
                      zx_status_get_string(status));
              continue;
            }
            size_t dst_vmo_size;
            status = dst->vmo().get_size(&dst_vmo_size);
            if (status != ZX_OK) {
              FDF_LOG(ERROR, "Failed to get the size of the VMO for the captured image: %s",
                      zx_status_get_string(status));
              continue;
            }
            if (dst_vmo_size != src_vmo_size) {
              FDF_LOG(ERROR,
                      "Capture will fail; the displayed image VMO size %zu does not match the "
                      "captured image VMO size %zu",
                      src_vmo_size, dst_vmo_size);
              continue;
            }
            fzl::VmoMapper mapped_src;
            status = mapped_src.Map(src->vmo(), 0, src_vmo_size, ZX_VM_PERM_READ);
            if (status != ZX_OK) {
              FDF_LOG(ERROR, "Capture thread will exit; failed to map displayed image VMO: %s",
                      zx_status_get_string(status));
              return status;
            }

            fzl::VmoMapper mapped_dst;
            status =
                mapped_dst.Map(dst->vmo(), 0, dst_vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE);
            if (status != ZX_OK) {
              FDF_LOG(ERROR, "Capture thread will exit; failed to map capture image VMO: %s",
                      zx_status_get_string(status));
              return status;
            }
            if (src->metadata().coherency_domain == fuchsia_sysmem2::CoherencyDomain::kRam) {
              zx_cache_flush(mapped_src.start(), src_vmo_size,
                             ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
            }
            std::memcpy(mapped_dst.start(), mapped_src.start(), dst_vmo_size);
            if (dst->metadata().coherency_domain == fuchsia_sysmem2::CoherencyDomain::kRam) {
              zx_cache_flush(mapped_dst.start(), dst_vmo_size,
                             ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
            }
          }
        }
        engine_listener_client_.OnCaptureComplete();
        current_capture_target_image_id_ = display::kInvalidDriverCaptureImageId;
        capture_complete_signal_count_ = 0;
      }
    }
  }
  return ZX_OK;
}

int FakeDisplay::VSyncThread() {
  while (true) {
    zx::nanosleep(zx::deadline_after(zx::sec(1) / kRefreshRateFps));
    if (vsync_shutdown_flag_.load()) {
      break;
    }
    SendVsync();
  }
  return ZX_OK;
}

void FakeDisplay::SendVsync() {
  fbl::AutoLock engine_listener_lock(&engine_listener_mutex_);
  if (engine_listener_client_.is_valid()) {
    // See the discussion in `DisplayEngineApplyConfiguration()` about
    // the reason we use relaxed memory order here.
    const display::ConfigStamp current_config_stamp =
        current_config_stamp_.load(std::memory_order_relaxed);
    const config_stamp_t banjo_current_config_stamp =
        display::ToBanjoConfigStamp(current_config_stamp);
    engine_listener_client_.OnDisplayVsync(ToBanjoDisplayId(kDisplayId), zx_clock_get_monotonic(),
                                           &banjo_current_config_stamp);
  }
}

void FakeDisplay::RecordDisplayConfigToInspectRootNode() {
  inspect::Node& root_node = inspector_.GetRoot();
  ZX_ASSERT(root_node);
  root_node.RecordChild("device_config", [&](inspect::Node& config_node) {
    config_node.RecordInt("width_px", kWidth);
    config_node.RecordInt("height_px", kHeight);
    config_node.RecordDouble("refresh_rate_hz", kRefreshRateFps);
    config_node.RecordBool("manual_vsync_trigger", device_config_.manual_vsync_trigger);
    config_node.RecordBool("no_buffer_access", device_config_.no_buffer_access);
  });
}

zx_status_t FakeDisplay::Initialize() {
  ZX_DEBUG_ASSERT(!initialized_);

  zx_status_t status = InitSysmemAllocatorClient();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize sysmem Allocator client: %s",
            zx_status_get_string(status));
    return status;
  }

  status = InitializeCapture();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize display capture: %s", zx_status_get_string(status));
    return status;
  }

  if (!device_config_.manual_vsync_trigger) {
    status = thrd_status_to_zx_status(thrd_create(
        &vsync_thread_,
        [](void* context) { return static_cast<FakeDisplay*>(context)->VSyncThread(); }, this));
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to create VSync thread: %s", zx_status_get_string(status));
      return status;
    }
    vsync_thread_running_ = true;
  }

  if (IsCaptureSupported()) {
    status = thrd_status_to_zx_status(thrd_create(
        &capture_thread_,
        [](void* context) { return static_cast<FakeDisplay*>(context)->CaptureThread(); }, this));
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to not create image capture thread: %s", zx_status_get_string(status));
      return status;
    }
  }

  RecordDisplayConfigToInspectRootNode();

  initialized_ = true;

  return ZX_OK;
}

void FakeDisplay::Deinitialize() {
  if (!initialized_) {
    return;
  }

  vsync_shutdown_flag_.store(true);
  if (vsync_thread_running_) {
    // Ignore return value here in case the vsync_thread_ isn't running.
    thrd_join(vsync_thread_, nullptr);
  }
  if (IsCaptureSupported()) {
    capture_shutdown_flag_.store(true);
    thrd_join(capture_thread_, nullptr);
  }

  initialized_ = false;
}

}  // namespace fake_display
