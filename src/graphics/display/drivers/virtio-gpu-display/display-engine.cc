// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-gpu-display/display-engine.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/stdcompat/span.h>
#include <lib/virtio/driver_utils.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <array>
#include <cinttypes>
#include <cstdint>
#include <cstring>
#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/virtio-gpu-display/imported-image.h"
#include "src/graphics/display/drivers/virtio-gpu-display/virtio-gpu-device.h"
#include "src/graphics/display/drivers/virtio-gpu-display/virtio-pci-device.h"
#include "src/graphics/display/lib/api-types/cpp/alpha-mode.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/coordinate-transformation.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"
#include "src/graphics/display/lib/api-types/cpp/rectangle.h"
#include "src/graphics/lib/virtio/virtio-abi.h"

namespace virtio_display {

namespace {

// TODO(https://fxbug.dev/42073721): Support more formats.
constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 1> kSupportedFormats = {
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kB8G8R8A8),
};

constexpr uint32_t kRefreshRateHz = 30;
constexpr display::DisplayId kDisplayId{1};

}  // namespace

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

  engine_events_.OnDisplayAdded(banjo_display_info);
}

zx::result<> DisplayEngine::ImportBufferCollection(
    display::DriverBufferCollectionId buffer_collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) {
  return imported_images_.ImportBufferCollection(buffer_collection_id,
                                                 std::move(buffer_collection_token));
}

zx::result<> DisplayEngine::ReleaseBufferCollection(
    display::DriverBufferCollectionId buffer_collection_id) {
  return imported_images_.ReleaseBufferCollection(buffer_collection_id);
}

zx::result<display::DriverImageId> DisplayEngine::ImportImage(
    const display::ImageMetadata& image_metadata,
    display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) {
  if (image_metadata.tiling_type() != display::ImageTilingType::kLinear) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  zx::result<display::DriverImageId> image_id_result =
      imported_images_.ImportImage(buffer_collection_id, buffer_index);
  if (image_id_result.is_error()) {
    // ImportImage() already logged the error.
    return image_id_result.take_error();
  }

  const display::DriverImageId image_id = image_id_result.value();
  SysmemBufferInfo* sysmem_buffer_info = imported_images_.FindSysmemInfoById(image_id);
  ZX_DEBUG_ASSERT(sysmem_buffer_info != nullptr);

  ZX_DEBUG_ASSERT(sysmem_buffer_info->pixel_format ==
                  fuchsia_images2::wire::PixelFormat::kB8G8R8A8);
  static constexpr int kBytesPerPixel = 4;

  ZX_DEBUG_ASSERT(sysmem_buffer_info->pixel_format_modifier ==
                  fuchsia_images2::wire::PixelFormatModifier::kLinear);

  size_t image_size = static_cast<size_t>(image_metadata.width()) *
                      static_cast<size_t>(image_metadata.height()) * kBytesPerPixel;

  zx::result<ImportedImage> imported_image_result =
      ImportedImage::Create(gpu_device_->bti(), sysmem_buffer_info->image_vmo,
                            sysmem_buffer_info->image_vmo_offset, image_size);
  if (imported_image_result.is_error()) {
    // Create() already logged the error.
    return imported_image_result.take_error();
  }

  ImportedImage* imported_image = imported_images_.FindImageById(image_id);
  ZX_DEBUG_ASSERT(imported_image != nullptr);
  *imported_image = std::move(imported_image_result).value();

  zx::result<uint32_t> create_resource_result = gpu_device_->Create2DResource(
      image_metadata.width(), image_metadata.height(), sysmem_buffer_info->pixel_format);
  if (create_resource_result.is_error()) {
    FDF_LOG(ERROR, "Failed to allocate 2D resource: %s", create_resource_result.status_string());
    return create_resource_result.take_error();
  }
  imported_image->set_virtio_resource_id(create_resource_result.value());

  zx::result<> attach_result = gpu_device_->AttachResourceBacking(
      imported_image->virtio_resource_id(), imported_image->physical_address(), image_size);
  if (attach_result.is_error()) {
    FDF_LOG(ERROR, "Failed to attach resource backing store: %s", attach_result.status_string());
    return attach_result.take_error();
  }

  return zx::ok(image_id);
}

zx::result<display::DriverCaptureImageId> DisplayEngine::ImportImageForCapture(
    display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

void DisplayEngine::ReleaseImage(display::DriverImageId image_id) {
  zx::result result = imported_images_.ReleaseImage(image_id);
  if (result.is_error()) {
    // ReleaseImage() already logged the error.
    // The display coordinator API does not have error reporting for this call.
    return;
  }
}

config_check_result_t DisplayEngine::CheckConfiguration(
    display::DisplayId display_id, cpp20::span<const display::DriverLayer> layers,
    cpp20::span<client_composition_opcode_t> out_client_composition_opcodes,
    size_t* out_client_composition_opcodes_actual) {
  ZX_DEBUG_ASSERT(display_id == kDisplayId);

  ZX_DEBUG_ASSERT(out_client_composition_opcodes.size() >= layers.size());
  ZX_DEBUG_ASSERT(!out_client_composition_opcodes_actual ||
                  *out_client_composition_opcodes_actual == layers.size());

  ZX_DEBUG_ASSERT(layers.size() == 1);

  const display::DriverLayer& layer = layers[0];
  const display::Rectangle display_area({
      .x = 0,
      .y = 0,
      .width = static_cast<int32_t>(current_display_.scanout_info.geometry.width),
      .height = static_cast<int32_t>(current_display_.scanout_info.geometry.height),
  });

  if (layer.display_destination() != display_area) {
    // TODO(costan): Doesn't seem right?
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (layer.image_source() != layer.display_destination()) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_FRAME_SCALE;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (layer.image_metadata().dimensions() != layer.image_source().dimensions()) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_SRC_FRAME;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (layer.alpha_mode() != display::AlphaMode::kDisable) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_TRANSFORM;
    return CONFIG_CHECK_RESULT_OK;
  }
  if (layer.image_source_transformation() != display::CoordinateTransformation::kIdentity) {
    out_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_TRANSFORM;
    return CONFIG_CHECK_RESULT_OK;
  }

  return CONFIG_CHECK_RESULT_OK;
}

void DisplayEngine::ApplyConfiguration(display::DisplayId display_id,
                                       cpp20::span<const display::DriverLayer> layers,
                                       display::ConfigStamp config_stamp) {
  ZX_DEBUG_ASSERT(layers.size() == 1);
  const display::DriverImageId image_id = layers[0].image_id();
  const ImportedImage* imported_image = imported_images_.FindImageById(image_id);
  if (imported_image == nullptr) {
    FDF_LOG(ERROR, "ApplyConfiguration() used invalid image ID");
    return;
  }

  {
    fbl::AutoLock al(&flush_lock_);
    latest_framebuffer_resource_id_ = imported_image->virtio_resource_id();
    latest_config_stamp_ = config_stamp;
  }
}

zx::result<> DisplayEngine::SetBufferCollectionConstraints(
    const display::ImageBufferUsage& image_buffer_usage,
    display::DriverBufferCollectionId buffer_collection_id) {
  ImportedBufferCollection* imported_buffer_collection =
      imported_images_.FindBufferCollectionById(buffer_collection_id);
  if (imported_buffer_collection == nullptr) {
    FDF_LOG(WARNING,
            "Rejected request to set constraints on BufferCollection with unknown ID: %" PRIu64,
            buffer_collection_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  // TODO(costan): fidl::Arena may allocate memory and crash. Find a way to get
  // control over memory allocation.
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
                      .color_spaces(std::array{fuchsia_images2::wire::ColorSpace::kSrgb})
                      .bytes_per_row_divisor(4)
                      .Build()});

  fidl::OneWayStatus set_constraints_status =
      imported_buffer_collection->sysmem_client()->SetConstraints(
          fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena)
              .constraints(constraints.Build())
              .Build());
  if (!set_constraints_status.ok()) {
    FDF_LOG(ERROR, "SetConstraints() FIDL call failed: %s", set_constraints_status.status_string());
    return zx::error(set_constraints_status.status());
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

DisplayEngine::DisplayEngine(DisplayEngineEventsInterface* engine_events,
                             fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client,
                             std::unique_ptr<VirtioGpuDevice> gpu_device)
    : imported_images_(std::move(sysmem_client)),
      engine_events_(*engine_events),
      gpu_device_(std::move(gpu_device)) {
  ZX_DEBUG_ASSERT(engine_events != nullptr);
  ZX_DEBUG_ASSERT(gpu_device_);
}

DisplayEngine::~DisplayEngine() = default;

// static
zx::result<std::unique_ptr<DisplayEngine>> DisplayEngine::Create(
    fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_client, zx::bti bti,
    std::unique_ptr<virtio::Backend> backend, DisplayEngineEventsInterface* engine_events) {
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
      &alloc_checker, engine_events, std::move(sysmem_client), std::move(gpu_device));
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
      fb_change = displayed_framebuffer_resource_id_ != latest_framebuffer_resource_id_;
      displayed_framebuffer_resource_id_ = latest_framebuffer_resource_id_;
      displayed_config_stamp_ = latest_config_stamp_;
    }

    FDF_LOG(TRACE, "flushing");

    if (fb_change) {
      uint32_t resource_id = displayed_framebuffer_resource_id_ ? displayed_framebuffer_resource_id_
                                                                : virtio_abi::kInvalidResourceId;
      zx::result<> set_scanout_result = gpu_device_->SetScanoutProperties(
          current_display_.scanout_id, resource_id, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (set_scanout_result.is_error()) {
        FDF_LOG(ERROR, "Failed to set scanout: %s", set_scanout_result.status_string());
        continue;
      }
    }

    if (displayed_framebuffer_resource_id_) {
      zx::result<> transfer_result = gpu_device_->TransferToHost2D(
          displayed_framebuffer_resource_id_, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (transfer_result.is_error()) {
        FDF_LOG(ERROR, "Failed to transfer resource: %s", transfer_result.status_string());
        continue;
      }

      zx::result<> flush_result = gpu_device_->FlushResource(
          displayed_framebuffer_resource_id_, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (flush_result.is_error()) {
        FDF_LOG(ERROR, "Failed to flush resource: %s", flush_result.status_string());
        continue;
      }
    }

    {
      fbl::AutoLock al(&flush_lock_);
      engine_events_.OnDisplayVsync(kDisplayId, zx::time(next_deadline), displayed_config_stamp_);
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

zx_status_t DisplayEngine::Init() {
  FDF_LOG(TRACE, "Init()");

  zx::result<> imported_images_init_result = imported_images_.Initialize();
  if (imported_images_init_result.is_error()) {
    // Initialize() already logged the error.
    return imported_images_init_result.error_value();
  }

  return ZX_OK;
}

}  // namespace virtio_display
