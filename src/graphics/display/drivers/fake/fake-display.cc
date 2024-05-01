// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-display.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/cpp/inspector.h>
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

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/preferred-scanout-image-type.h"
#include "src/graphics/display/drivers/fake/image-info.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
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
constexpr uint32_t kWidth = 1280;
constexpr uint32_t kHeight = 800;

constexpr display::DisplayId kDisplayId(1);

constexpr uint32_t kRefreshRateFps = 60;
// Arbitrary slowdown for testing purposes
// TODO(payamm): Randomizing the delay value is more value
constexpr uint64_t kNumOfVsyncsForCapture = 5;  // 5 * 16ms = 80ms
}  // namespace

FakeDisplay::FakeDisplay(zx_device_t* parent, FakeDisplayDeviceConfig device_config,
                         inspect::Inspector inspector)
    : DeviceType(parent),
      display_controller_impl_banjo_protocol_({&display_controller_impl_protocol_ops_, this}),
      device_config_(device_config),
      inspector_(std::move(inspector)) {
  ZX_DEBUG_ASSERT(parent);
}

FakeDisplay::~FakeDisplay() = default;

zx_status_t FakeDisplay::DisplayControllerImplSetMinimumRgb(uint8_t minimum_rgb) {
  fbl::AutoLock lock(&capture_mutex_);

  clamp_rgb_value_ = minimum_rgb;
  return ZX_OK;
}

void FakeDisplay::PopulateAddedDisplayArgs(added_display_args_t* args) {
  args->display_id = display::ToBanjoDisplayId(kDisplayId);
  args->panel_capabilities_source = PANEL_CAPABILITIES_SOURCE_DISPLAY_MODE;

  const int32_t pixel_clock_hz = kWidth * kHeight * kRefreshRateFps;
  ZX_DEBUG_ASSERT(pixel_clock_hz >= 0);
  ZX_DEBUG_ASSERT(pixel_clock_hz <= display::kMaxPixelClockHz);

  const display::DisplayTiming timing = {
      .horizontal_active_px = static_cast<int32_t>(kWidth),
      .horizontal_front_porch_px = 0,
      .horizontal_sync_width_px = 0,
      .horizontal_back_porch_px = 0,
      .vertical_active_lines = static_cast<int32_t>(kHeight),
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
  args->panel.mode = display::ToBanjoDisplayMode(timing);
  args->pixel_format_list = kSupportedPixelFormats;
  args->pixel_format_count = std::size(kSupportedPixelFormats);
}

zx_status_t FakeDisplay::InitSysmemAllocatorClient() {
  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> sysmem_client_result =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::AllocatorV1>(parent_,
                                                                                    "sysmem");
  if (sysmem_client_result.is_error()) {
    zxlogf(ERROR, "Cannot connect to the fuchsia.hardware.sysmem.Sysmem protocol: %s",
           sysmem_client_result.status_string());
    return sysmem_client_result.error_value();
  }
  sysmem_ = fidl::SyncClient(std::move(sysmem_client_result.value()));

  std::string debug_name = fxl::StringPrintf("fake-display[%lu]", fsl::GetCurrentProcessKoid());
  fuchsia_sysmem::AllocatorSetDebugClientInfoRequest request;
  request.name() = std::move(debug_name);
  request.id() = fsl::GetCurrentProcessKoid();
  auto set_debug_status = sysmem_->SetDebugClientInfo(std::move(request));
  if (!set_debug_status.is_ok()) {
    zxlogf(ERROR, "Cannot set sysmem allocator debug info: %s",
           set_debug_status.error_value().status_string());
    return set_debug_status.error_value().status();
  }

  return ZX_OK;
}

void FakeDisplay::DisplayControllerImplSetDisplayControllerInterface(
    const display_controller_interface_protocol_t* intf) {
  fbl::AutoLock interface_lock(&interface_mutex_);
  controller_interface_client_ = ddk::DisplayControllerInterfaceProtocolClient(intf);
  added_display_args_t args;
  PopulateAddedDisplayArgs(&args);
  controller_interface_client_.OnDisplaysChanged(&args, 1, nullptr, 0);
}

void FakeDisplay::DisplayControllerImplResetDisplayControllerInterface() {
  fbl::AutoLock interface_lock(&interface_mutex_);
  controller_interface_client_ = ddk::DisplayControllerInterfaceProtocolClient();
}

zx::result<display::DriverImageId> FakeDisplay::ImportVmoImageForTesting(zx::vmo vmo,
                                                                         size_t offset) {
  fbl::AllocChecker alloc_checker;
  fbl::AutoLock lock(&image_mutex_);

  display::DriverImageId driver_image_id = next_imported_display_driver_image_id_++;
  // Image metadata for testing only and may not reflect the actual image
  // buffer format.
  ImageMetadata display_image_metadata = {
      .pixel_format = fuchsia_sysmem::wire::PixelFormatType::kBgra32,
      .coherency_domain = fuchsia_sysmem::wire::CoherencyDomain::kCpu,
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

zx_status_t FakeDisplay::DisplayControllerImplImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    zxlogf(ERROR, "Buffer Collection (id=%lu) already exists", driver_buffer_collection_id.value());
    return ZX_ERR_ALREADY_EXISTS;
  }

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<fuchsia_sysmem::BufferCollection>::Create();

  fuchsia_sysmem::AllocatorBindSharedCollectionRequest bind_request;
  bind_request.token() =
      fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>(std::move(collection_token));
  bind_request.buffer_collection_request() = std::move(collection_server_endpoint);
  auto bind_result = sysmem_->BindSharedCollection(std::move(bind_request));
  if (bind_result.is_error()) {
    zxlogf(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
           bind_result.error_value().status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::SyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t FakeDisplay::DisplayControllerImplReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Cannot release buffer collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  buffer_collections_.erase(driver_buffer_collection_id);
  return ZX_OK;
}

zx_status_t FakeDisplay::DisplayControllerImplImportImage(
    const image_metadata_t* image_metadata, uint64_t banjo_driver_buffer_collection_id,
    uint32_t index, uint64_t* out_image_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::SyncClient<fuchsia_sysmem::BufferCollection>& collection = it->second;

  fbl::AutoLock lock(&image_mutex_);
  if (!IsAcceptableImageTilingType(image_metadata->tiling_type)) {
    zxlogf(INFO, "ImportImage() will fail due to invalid Image tiling type %" PRIu32,
           image_metadata->tiling_type);
    return ZX_ERR_INVALID_ARGS;
  }

  fidl::Result check_result = collection->CheckBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (check_result.is_error()) {
    return check_result.error_value().status();
  }
  const auto& check_response = check_result.value();
  if (check_response.status() == ZX_ERR_UNAVAILABLE) {
    return ZX_ERR_SHOULD_WAIT;
  }
  if (check_response.status() != ZX_OK) {
    return check_response.status();
  }

  fidl::Result wait_result = collection->WaitForBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (wait_result.is_error()) {
    return wait_result.error_value().status();
  }
  auto& wait_response = wait_result.value();
  if (wait_response.status() != ZX_OK) {
    return wait_response.status();
  }
  auto& collection_info = wait_response.buffer_collection_info();

  fbl::Vector<zx::vmo> vmos;
  for (uint32_t i = 0; i < collection_info.buffer_count(); ++i) {
    vmos.push_back(std::move(collection_info.buffers()[i].vmo()));
  }

  if (!collection_info.settings().has_image_format_constraints() || index >= vmos.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // TODO(https://fxbug.dev/42079320): When capture is enabled
  // (IsCaptureSupported() is true), we should perform a check to ensure that
  // the display images should not be of "inaccessible" coherency domain.

  display::DriverImageId driver_image_id = next_imported_display_driver_image_id_++;
  ImageMetadata display_image_metadata = {
      .pixel_format = collection_info.settings().image_format_constraints().pixel_format().type(),
      .coherency_domain = collection_info.settings().buffer_settings().coherency_domain(),
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

void FakeDisplay::DisplayControllerImplReleaseImage(uint64_t image_handle) {
  fbl::AutoLock lock(&image_mutex_);
  display::DriverImageId driver_image_id = display::ToDriverImageId(image_handle);
  if (imported_images_.erase(driver_image_id) == nullptr) {
    zxlogf(ERROR, "Failed to release display Image (handle %" PRIu64 ")", driver_image_id.value());
  }
}

config_check_result_t FakeDisplay::DisplayControllerImplCheckConfiguration(
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
    frame_t frame = {
        .x_pos = 0,
        .y_pos = 0,
        .width = kWidth,
        .height = kHeight,
    };
    success = display_configs[0].layer_list[0].type == LAYER_TYPE_PRIMARY &&
              layer.transform_mode == FRAME_TRANSFORM_IDENTITY &&
              layer.image_metadata.width == kWidth && layer.image_metadata.height == kHeight &&
              memcmp(&layer.dest_frame, &frame, sizeof(frame_t)) == 0 &&
              memcmp(&layer.src_frame, &frame, sizeof(frame_t)) == 0 &&
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

void FakeDisplay::DisplayControllerImplApplyConfiguration(
    const display_config_t* display_configs, size_t display_count,
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

void FakeDisplay::DisplayControllerImplSetEld(uint64_t display_id, const uint8_t* raw_eld_list,
                                              size_t raw_eld_count) {}

enum class FakeDisplay::BufferCollectionUsage {
  kPrimaryLayer = 1,
  kCapture = 2,
};

fuchsia_sysmem::BufferCollectionConstraints FakeDisplay::CreateBufferCollectionConstraints(
    BufferCollectionUsage usage) {
  fuchsia_sysmem::BufferCollectionConstraints constraints;
  switch (usage) {
    case BufferCollectionUsage::kCapture:
      constraints.usage().cpu() =
          fuchsia_sysmem::kCpuUsageReadOften | fuchsia_sysmem::kCpuUsageWriteOften;
      break;
    case BufferCollectionUsage::kPrimaryLayer:
      constraints.usage().display() = fuchsia_sysmem::kDisplayUsageLayer;
      break;
  }

  // TODO(https://fxbug.dev/42079320): In order to support capture, both capture sources
  // and capture targets must not be in the "inaccessible" coherency domain.
  constraints.has_buffer_memory_constraints() = true;
  SetBufferMemoryConstraints(constraints.buffer_memory_constraints());

  // When we have C++20, we can use std::to_array to avoid specifying the array
  // size twice.
  static constexpr std::array<fuchsia_sysmem::PixelFormatType, 2> kPixelFormats = {
      fuchsia_sysmem::PixelFormatType::kR8G8B8A8, fuchsia_sysmem::PixelFormatType::kBgra32};
  static constexpr std::array<uint64_t, 2> kFormatModifiers = {
      fuchsia_sysmem::kFormatModifierLinear, fuchsia_sysmem::kFormatModifierGoogleGoldfishOptimal};

  size_t format_constraints_count = 0;
  for (fuchsia_sysmem::PixelFormatType pixel_format : kPixelFormats) {
    for (uint64_t format_modifier : kFormatModifiers) {
      fuchsia_sysmem::ImageFormatConstraints& image_constraints =
          constraints.image_format_constraints()[format_constraints_count];
      ++format_constraints_count;

      SetCommonImageFormatConstraints(pixel_format, fuchsia_sysmem::FormatModifier(format_modifier),
                                      image_constraints);
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
  ZX_DEBUG_ASSERT(format_constraints_count <= std::numeric_limits<uint32_t>::max());
  constraints.image_format_constraints_count() = static_cast<uint32_t>(format_constraints_count);
  return constraints;
}

void FakeDisplay::SetBufferMemoryConstraints(fuchsia_sysmem::BufferMemoryConstraints& constraints) {
  constraints.min_size_bytes() = 0;
  constraints.max_size_bytes() = std::numeric_limits<uint32_t>::max();
  constraints.physically_contiguous_required() = false;
  constraints.secure_required() = false;
  constraints.ram_domain_supported() = true;
  constraints.cpu_domain_supported() = true;
  constraints.inaccessible_domain_supported() = true;
}

void FakeDisplay::SetCommonImageFormatConstraints(
    fuchsia_sysmem::PixelFormatType pixel_format_type,
    fuchsia_sysmem::FormatModifier format_modifier,
    fuchsia_sysmem::ImageFormatConstraints& constraints) {
  constraints.pixel_format().type() = pixel_format_type;
  constraints.pixel_format().has_format_modifier() = true;
  constraints.pixel_format().format_modifier() = format_modifier;

  constraints.color_spaces_count() = 1;
  constraints.color_space()[0].type() = fuchsia_sysmem::ColorSpaceType::kSrgb;

  constraints.layers() = 1;
  constraints.coded_width_divisor() = 1;
  constraints.coded_height_divisor() = 1;
  constraints.bytes_per_row_divisor() = 1;
  constraints.start_offset_divisor() = 1;
  constraints.display_width_divisor() = 1;
  constraints.display_height_divisor() = 1;
}

void FakeDisplay::SetCaptureImageFormatConstraints(
    fuchsia_sysmem::ImageFormatConstraints& constraints) {
  constraints.min_coded_width() = kWidth;
  constraints.max_coded_width() = kWidth;
  constraints.min_coded_height() = kHeight;
  constraints.max_coded_height() = kHeight;
  constraints.min_bytes_per_row() = kWidth * 4;
  constraints.max_bytes_per_row() = kWidth * 4;
  constraints.max_coded_width_times_coded_height() = kWidth * kHeight;
}

void FakeDisplay::SetLayerImageFormatConstraints(
    fuchsia_sysmem::ImageFormatConstraints& constraints) {
  constraints.min_coded_width() = 0;
  constraints.max_coded_width() = std::numeric_limits<uint32_t>::max();
  constraints.min_coded_height() = 0;
  constraints.max_coded_height() = std::numeric_limits<uint32_t>::max();
  constraints.min_bytes_per_row() = 0;
  constraints.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  constraints.max_coded_width_times_coded_height() = std::numeric_limits<uint32_t>::max();
}

zx_status_t FakeDisplay::DisplayControllerImplSetBufferCollectionConstraints(
    const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::SyncClient<fuchsia_sysmem::BufferCollection>& collection = it->second;

  BufferCollectionUsage buffer_collection_usage = (usage->tiling_type == IMAGE_TILING_TYPE_CAPTURE)
                                                      ? BufferCollectionUsage::kCapture
                                                      : BufferCollectionUsage::kPrimaryLayer;
  auto set_result = collection->SetConstraints(
      {true, CreateBufferCollectionConstraints(buffer_collection_usage)});
  if (set_result.is_error()) {
    zxlogf(ERROR, "Failed to set constraints on a sysmem BufferCollection: %s",
           set_result.error_value().status_string());
    return set_result.error_value().status();
  }

  return ZX_OK;
}

zx_status_t FakeDisplay::DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t FakeDisplay::DisplayControllerImplImportImageForCapture(
    uint64_t banjo_driver_buffer_collection_id, uint32_t index, uint64_t* out_capture_handle) {
  if (!IsCaptureSupported()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::SyncClient<fuchsia_sysmem::BufferCollection>& collection = it->second;

  fbl::AutoLock lock(&capture_mutex_);

  fidl::Result check_result = collection->CheckBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (check_result.is_error()) {
    return check_result.error_value().status();
  }
  const auto& check_response = check_result.value();
  if (check_response.status() == ZX_ERR_UNAVAILABLE) {
    return ZX_ERR_SHOULD_WAIT;
  }
  if (check_response.status() != ZX_OK) {
    return check_response.status();
  }

  fidl::Result wait_result = collection->WaitForBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (wait_result.is_error()) {
    return wait_result.error_value().status();
  }
  auto& wait_response = wait_result.value();
  if (wait_response.status() != ZX_OK) {
    return wait_response.status();
  }
  fuchsia_sysmem::BufferCollectionInfo2& collection_info = wait_response.buffer_collection_info();

  if (!collection_info.settings().has_image_format_constraints()) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (index >= collection_info.buffer_count()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // TODO(https://fxbug.dev/42079320): Capture target images should not be of
  // "inaccessible" coherency domain. We should add a check here.
  display::DriverCaptureImageId driver_capture_image_id = next_imported_driver_capture_image_id_++;
  ImageMetadata capture_image_metadata = {
      .pixel_format = collection_info.settings().image_format_constraints().pixel_format().type(),
      .coherency_domain = collection_info.settings().buffer_settings().coherency_domain(),
  };
  zx::vmo vmo = std::move(collection_info.buffers()[index].vmo());

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

bool FakeDisplay::DisplayControllerImplIsCaptureSupported() { return IsCaptureSupported(); }

zx_status_t FakeDisplay::DisplayControllerImplStartCapture(uint64_t capture_handle) {
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

zx_status_t FakeDisplay::DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
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

bool FakeDisplay::DisplayControllerImplIsCaptureCompleted() {
  fbl::AutoLock lock(&capture_mutex_);
  return current_capture_target_image_id_ == display::kInvalidDriverCaptureImageId;
}

void FakeDisplay::DdkRelease() {
  vsync_shutdown_flag_.store(true);
  if (vsync_thread_running_) {
    // Ignore return value here in case the vsync_thread_ isn't running.
    thrd_join(vsync_thread_, nullptr);
  }
  if (IsCaptureSupported()) {
    capture_shutdown_flag_.store(true);
    thrd_join(capture_thread_, nullptr);
  }
  delete this;
}

zx_status_t FakeDisplay::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out_protocol);
  proto->ctx = this;
  switch (proto_id) {
    case ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL:
      proto->ops = &display_controller_impl_protocol_ops_;
      return ZX_OK;
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t FakeDisplay::SetupDisplayInterface() {
  {
    fbl::AutoLock image_lock(&image_mutex_);
    current_image_to_capture_id_ = display::kInvalidDriverImageId;
  }

  {
    fbl::AutoLock interface_lock(&interface_mutex_);
    if (controller_interface_client_.is_valid()) {
      added_display_args_t args;
      PopulateAddedDisplayArgs(&args);

      controller_interface_client_.OnDisplaysChanged(&args, /*added_display_count=*/1,
                                                     /*removed_display_list=*/nullptr,
                                                     /*removed_display_count=*/0);
    }
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
      fbl::AutoLock interface_lock_(&interface_mutex_);
      // `current_capture_target_image_` is a pointer to DisplayImageInfo stored in
      // `imported_captures_` (guarded by `capture_mutex_`). So `capture_mutex_`
      // must be locked while the capture image is being used.
      fbl::AutoLock capture_lock(&capture_mutex_);
      if (controller_interface_client_.is_valid() &&
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
              zxlogf(ERROR, "Trying to capture format=%u as format=%u\n",
                     static_cast<uint32_t>(src->metadata().pixel_format),
                     static_cast<uint32_t>(dst->metadata().pixel_format));
              continue;
            }
            size_t src_vmo_size;
            auto status = src->vmo().get_size(&src_vmo_size);
            if (status != ZX_OK) {
              zxlogf(ERROR, "Failed to get the size of the displayed image VMO: %s",
                     zx_status_get_string(status));
              continue;
            }
            size_t dst_vmo_size;
            status = dst->vmo().get_size(&dst_vmo_size);
            if (status != ZX_OK) {
              zxlogf(ERROR, "Failed to get the size of the VMO for the captured image: %s",
                     zx_status_get_string(status));
              continue;
            }
            if (dst_vmo_size != src_vmo_size) {
              zxlogf(ERROR,
                     "Capture will fail; the displayed image VMO size %zu does not match the "
                     "captured image VMO size %zu",
                     src_vmo_size, dst_vmo_size);
              continue;
            }
            fzl::VmoMapper mapped_src;
            status = mapped_src.Map(src->vmo(), 0, src_vmo_size, ZX_VM_PERM_READ);
            if (status != ZX_OK) {
              zxlogf(ERROR, "Capture thread will exit; failed to map displayed image VMO: %s",
                     zx_status_get_string(status));
              return status;
            }

            fzl::VmoMapper mapped_dst;
            status =
                mapped_dst.Map(dst->vmo(), 0, dst_vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE);
            if (status != ZX_OK) {
              zxlogf(ERROR, "Capture thread will exit; failed to map capture image VMO: %s",
                     zx_status_get_string(status));
              return status;
            }
            if (src->metadata().coherency_domain == fuchsia_sysmem::wire::CoherencyDomain::kRam) {
              zx_cache_flush(mapped_src.start(), src_vmo_size,
                             ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
            }
            std::memcpy(mapped_dst.start(), mapped_src.start(), dst_vmo_size);
            if (dst->metadata().coherency_domain == fuchsia_sysmem::wire::CoherencyDomain::kRam) {
              zx_cache_flush(mapped_dst.start(), dst_vmo_size,
                             ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
            }
          }
        }
        controller_interface_client_.OnCaptureComplete();
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
  fbl::AutoLock interface_lock(&interface_mutex_);
  if (controller_interface_client_.is_valid()) {
    // See the discussion in `DisplayControllerImplApplyConfiguration()` about
    // the reason we use relaxed memory order here.
    const display::ConfigStamp current_config_stamp =
        current_config_stamp_.load(std::memory_order_relaxed);
    const config_stamp_t banjo_current_config_stamp =
        display::ToBanjoConfigStamp(current_config_stamp);
    controller_interface_client_.OnDisplayVsync(
        ToBanjoDisplayId(kDisplayId), zx_clock_get_monotonic(), &banjo_current_config_stamp);
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

zx_status_t FakeDisplay::Bind() {
  zx_status_t status = InitSysmemAllocatorClient();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize sysmem Allocator client: %s", zx_status_get_string(status));
    return status;
  }

  // Setup Display Interface
  status = SetupDisplayInterface();
  if (status != ZX_OK) {
    zxlogf(ERROR, "SetupDisplayInterface() failed: %s", zx_status_get_string(status));
    return status;
  }

  if (!device_config_.manual_vsync_trigger) {
    status = thrd_status_to_zx_status(thrd_create_with_name(
        &vsync_thread_,
        [](void* context) { return static_cast<FakeDisplay*>(context)->VSyncThread(); }, this,
        "vsync_thread"));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to create VSync thread: %s", zx_status_get_string(status));
      return status;
    }
    vsync_thread_running_ = true;
  }

  if (IsCaptureSupported()) {
    status = thrd_status_to_zx_status(thrd_create_with_name(
        &capture_thread_,
        [](void* context) { return static_cast<FakeDisplay*>(context)->CaptureThread(); }, this,
        "capture_thread"));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to not create image capture thread: %s", zx_status_get_string(status));
      return status;
    }
  }

  status = DdkAdd(ddk::DeviceAddArgs("fake-display").set_inspect_vmo(inspector_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
    return status;
  }

  RecordDisplayConfigToInspectRootNode();

  return ZX_OK;
}

}  // namespace fake_display
