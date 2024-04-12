// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/display-engine.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
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
#include <cstring>
#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/virtio-guest/v1/gpu-device-driver.h"
#include "src/graphics/display/drivers/virtio-guest/v1/virtio-gpu-device.h"
#include "src/graphics/display/drivers/virtio-guest/v1/virtio-pci-device.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/lib/virtio/virtio-abi.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace virtio_display {

namespace {

constexpr uint32_t kRefreshRateHz = 30;
constexpr display::DisplayId kDisplayId{1};

}  // namespace

// DDK level ops

using imported_image_t = struct imported_image {
  uint32_t resource_id;
  zx::pmt pmt;
};

zx_status_t DisplayEngine::DdkGetProtocol(uint32_t proto_id, void* out) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out);
  proto->ctx = this;
  if (proto_id == ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL) {
    proto->ops = &display_controller_impl_protocol_ops_;
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

void DisplayEngine::DisplayControllerImplSetDisplayControllerInterface(
    const display_controller_interface_protocol_t* intf) {
  {
    fbl::AutoLock al(&flush_lock_);
    dc_intf_ = *intf;
  }

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

  added_display_args_t args = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .panel_capabilities_source = PANEL_CAPABILITIES_SOURCE_DISPLAY_MODE,
      .panel =
          {
              .mode = display::ToBanjoDisplayMode(timing),
          },
      .pixel_format_list = kSupportedFormats.data(),
      .pixel_format_count = kSupportedFormats.size(),
  };
  display_controller_interface_on_displays_changed(intf, &args, 1, nullptr, 0);
}

void DisplayEngine::DisplayControllerImplResetDisplayControllerInterface() {
  fbl::AutoLock al(&flush_lock_);
  dc_intf_ = display_controller_interface_protocol_t{
      .ops = nullptr,
      .ctx = nullptr,
  };
}

zx::result<DisplayEngine::BufferInfo> DisplayEngine::GetAllocatedBufferInfoForImage(
    display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index,
    const image_metadata_t& image_metadata) const {
  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& client =
      buffer_collections_.at(driver_buffer_collection_id);
  fidl::WireResult check_result = client->CheckBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    zxlogf(ERROR, "CheckBuffersAllocated IPC failed: %s", check_result.status_string());
    return zx::error(check_result.status());
  }
  const auto& check_response = check_result.value();
  if (check_response.status == ZX_ERR_UNAVAILABLE) {
    return zx::error(ZX_ERR_SHOULD_WAIT);
  }
  if (check_response.status != ZX_OK) {
    zxlogf(ERROR, "CheckBuffersAllocated returned error: %s",
           zx_status_get_string(check_response.status));
    return zx::error(check_response.status);
  }

  auto wait_result = client->WaitForBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    zxlogf(ERROR, "WaitForBuffersAllocated IPC failed: %s", wait_result.status_string());
    return zx::error(wait_result.status());
  }
  auto& wait_response = wait_result.value();
  if (wait_response.status != ZX_OK) {
    zxlogf(ERROR, "WaitForBuffersAllocated returned error: %s",
           zx_status_get_string(wait_response.status));
    return zx::error(wait_response.status);
  }
  fuchsia_sysmem::wire::BufferCollectionInfo2& collection_info =
      wait_response.buffer_collection_info;

  if (!collection_info.settings.has_image_format_constraints) {
    zxlogf(ERROR, "Bad image format constraints");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (index >= collection_info.buffer_count) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  ZX_DEBUG_ASSERT(collection_info.settings.image_format_constraints.pixel_format.type ==
                  fuchsia_sysmem::wire::PixelFormatType::kBgra32);
  ZX_DEBUG_ASSERT(
      collection_info.settings.image_format_constraints.pixel_format.has_format_modifier);
  ZX_DEBUG_ASSERT(
      collection_info.settings.image_format_constraints.pixel_format.format_modifier.value ==
      fuchsia_sysmem::wire::kFormatModifierLinear);

  const auto& format_constraints = collection_info.settings.image_format_constraints;
  uint32_t minimum_row_bytes;
  if (!ImageFormatMinimumRowBytes(format_constraints, image_metadata.width, &minimum_row_bytes)) {
    zxlogf(ERROR, "Invalid image width %" PRIu32 " for collection", image_metadata.width);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(BufferInfo{
      .vmo = std::move(collection_info.buffers[index].vmo),
      .offset = collection_info.buffers[index].vmo_usable_start,
      .bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(format_constraints.pixel_format),
      .bytes_per_row = minimum_row_bytes,
      .pixel_format = sysmem::V2CopyFromV1PixelFormatType(format_constraints.pixel_format.type),
  });
}

zx_status_t DisplayEngine::DisplayControllerImplImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    zxlogf(ERROR, "Buffer Collection (id=%lu) already exists", driver_buffer_collection_id.value());
    return ZX_ERR_ALREADY_EXISTS;
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_.is_valid(), "sysmem allocator is not initialized");

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<fuchsia_sysmem::BufferCollection>::Create();

  auto bind_result = sysmem_->BindSharedCollection(
      fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>(std::move(collection_token)),
      std::move(collection_server_endpoint));
  if (!bind_result.ok()) {
    zxlogf(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
           bind_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplReleaseBufferCollection(
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

zx_status_t DisplayEngine::DisplayControllerImplImportImage(
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

  zx::result<BufferInfo> buffer_info_result =
      GetAllocatedBufferInfoForImage(driver_buffer_collection_id, index, *image_metadata);
  if (!buffer_info_result.is_ok()) {
    return buffer_info_result.error_value();
  }
  BufferInfo& buffer_info = buffer_info_result.value();
  zx::result<display::DriverImageId> import_result =
      Import(std::move(buffer_info.vmo), *image_metadata, buffer_info.offset,
             buffer_info.bytes_per_pixel, buffer_info.bytes_per_row, buffer_info.pixel_format);
  if (import_result.is_ok()) {
    *out_image_handle = display::ToBanjoDriverImageId(import_result.value());
    return ZX_OK;
  }
  return import_result.error_value();
}

zx::result<display::DriverImageId> DisplayEngine::Import(
    zx::vmo vmo, const image_metadata_t& image_metadata, size_t offset, uint32_t pixel_size,
    uint32_t row_bytes, fuchsia_images2::wire::PixelFormat pixel_format) {
  if (image_metadata.tiling_type != IMAGE_TILING_TYPE_LINEAR) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fbl::AllocChecker ac;
  auto import_data = fbl::make_unique_checked<imported_image_t>(&ac);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  unsigned size = ZX_ROUNDUP(row_bytes * image_metadata.height, zx_system_get_page_size());
  zx_paddr_t paddr;
  zx_status_t status = gpu_device_->bti().pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, vmo, offset,
                                              size, &paddr, 1, &import_data->pmt);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to pin VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx::result<uint32_t> create_resource_result =
      gpu_device_->Create2DResource(row_bytes / pixel_size, image_metadata.height, pixel_format);
  if (create_resource_result.is_error()) {
    zxlogf(ERROR, "Failed to allocate 2D resource: %s", create_resource_result.status_string());
    return create_resource_result.take_error();
  }
  import_data->resource_id = create_resource_result.value();

  zx::result<> attach_result =
      gpu_device_->AttachResourceBacking(import_data->resource_id, paddr, size);
  if (attach_result.is_error()) {
    zxlogf(ERROR, "Failed to attach resource backing store: %s", attach_result.status_string());
    return attach_result.take_error();
  }

  display::DriverImageId image_id(reinterpret_cast<uint64_t>(import_data.release()));
  return zx::ok(image_id);
}

void DisplayEngine::DisplayControllerImplReleaseImage(uint64_t image_handle) {
  delete reinterpret_cast<imported_image_t*>(image_handle);
}

config_check_result_t DisplayEngine::DisplayControllerImplCheckConfiguration(
    const display_config_t** display_configs, size_t display_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual) {
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = 0;
  }

  if (display_count != 1) {
    ZX_DEBUG_ASSERT(display_count == 0);
    return CONFIG_CHECK_RESULT_OK;
  }
  ZX_DEBUG_ASSERT(display::ToDisplayId(display_configs[0]->display_id) == kDisplayId);

  ZX_DEBUG_ASSERT(client_composition_opcodes_count >= display_configs[0]->layer_count);
  cpp20::span<client_composition_opcode_t> client_composition_opcodes(
      out_client_composition_opcodes_list, display_configs[0]->layer_count);
  std::fill(client_composition_opcodes.begin(), client_composition_opcodes.end(), 0);
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = client_composition_opcodes.size();
  }

  bool success;
  if (display_configs[0]->layer_count != 1) {
    success = display_configs[0]->layer_count == 0;
  } else {
    const primary_layer_t* layer = &display_configs[0]->layer_list[0]->cfg.primary;
    frame_t frame = {
        .x_pos = 0,
        .y_pos = 0,
        .width = current_display_.scanout_info.geometry.width,
        .height = current_display_.scanout_info.geometry.height,
    };
    success = display_configs[0]->layer_list[0]->type == LAYER_TYPE_PRIMARY &&
              layer->transform_mode == FRAME_TRANSFORM_IDENTITY &&
              layer->image_metadata.width == current_display_.scanout_info.geometry.width &&
              layer->image_metadata.height == current_display_.scanout_info.geometry.height &&
              memcmp(&layer->dest_frame, &frame, sizeof(frame_t)) == 0 &&
              memcmp(&layer->src_frame, &frame, sizeof(frame_t)) == 0 &&
              display_configs[0]->cc_flags == 0 && layer->alpha_mode == ALPHA_DISABLE;
  }
  if (!success) {
    client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
    for (unsigned i = 1; i < display_configs[0]->layer_count; i++) {
      client_composition_opcodes[i] = CLIENT_COMPOSITION_OPCODE_MERGE_SRC;
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

void DisplayEngine::DisplayControllerImplApplyConfiguration(
    const display_config_t** display_configs, size_t display_count,
    const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(banjo_config_stamp);
  display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);
  uint64_t handle = display_count == 0 || display_configs[0]->layer_count == 0
                        ? 0
                        : display_configs[0]->layer_list[0]->cfg.primary.image_handle;

  {
    fbl::AutoLock al(&flush_lock_);
    latest_fb_ = reinterpret_cast<imported_image_t*>(handle);
    latest_config_stamp_ = config_stamp;
  }
}

zx_status_t DisplayEngine::DisplayControllerImplSetBufferCollectionConstraints(
    const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "SetBufferCollectionConstraints: Cannot find imported buffer collection (id=%lu)",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  fuchsia_sysmem::wire::BufferCollectionConstraints constraints;
  constraints.usage.display = fuchsia_sysmem::wire::kDisplayUsageLayer;
  constraints.has_buffer_memory_constraints = true;
  fuchsia_sysmem::wire::BufferMemoryConstraints& buffer_constraints =
      constraints.buffer_memory_constraints;
  buffer_constraints.min_size_bytes = 0;
  buffer_constraints.max_size_bytes = 0xffffffff;
  buffer_constraints.physically_contiguous_required = true;
  buffer_constraints.secure_required = false;
  buffer_constraints.ram_domain_supported = true;
  buffer_constraints.cpu_domain_supported = true;
  constraints.image_format_constraints_count = 1;
  fuchsia_sysmem::wire::ImageFormatConstraints& image_constraints =
      constraints.image_format_constraints[0];
  image_constraints.pixel_format.type = fuchsia_sysmem::wire::PixelFormatType::kBgra32;
  image_constraints.pixel_format.has_format_modifier = true;
  image_constraints.pixel_format.format_modifier.value =
      fuchsia_sysmem::wire::kFormatModifierLinear;
  image_constraints.color_spaces_count = 1;
  image_constraints.color_space[0].type = fuchsia_sysmem::wire::ColorSpaceType::kSrgb;
  image_constraints.min_coded_width = 0;
  image_constraints.max_coded_width = 0xffffffff;
  image_constraints.min_coded_height = 0;
  image_constraints.max_coded_height = 0xffffffff;
  image_constraints.min_bytes_per_row = 0;
  image_constraints.max_bytes_per_row = 0xffffffff;
  image_constraints.max_coded_width_times_coded_height = 0xffffffff;
  image_constraints.layers = 1;
  image_constraints.coded_width_divisor = 1;
  image_constraints.coded_height_divisor = 1;
  // Bytes per row needs to be a multiple of the pixel size.
  image_constraints.bytes_per_row_divisor = 4;
  image_constraints.start_offset_divisor = 1;
  image_constraints.display_width_divisor = 1;
  image_constraints.display_height_divisor = 1;

  zx_status_t status = it->second->SetConstraints(true, constraints).status();

  if (status != ZX_OK) {
    zxlogf(ERROR, "virtio::DisplayEngine: Failed to set constraints");
    return status;
  }

  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplSetDisplayPower(uint64_t display_id,
                                                                bool power_on) {
  return ZX_ERR_NOT_SUPPORTED;
}

DisplayEngine::DisplayEngine(zx_device_t* bus_device,
                             fidl::ClientEnd<fuchsia_sysmem::Allocator> sysmem_client,
                             std::unique_ptr<VirtioGpuDevice> gpu_device)
    : sysmem_(std::move(sysmem_client)),
      bus_device_(bus_device),
      gpu_device_(std::move(gpu_device)) {
  ZX_DEBUG_ASSERT(gpu_device_);
}

DisplayEngine::~DisplayEngine() { io_buffer_release(&gpu_req_); }

// static
zx::result<std::unique_ptr<DisplayEngine>> DisplayEngine::Create(zx_device_t* bus_device) {
  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> sysmem_client_result =
      DdkDeviceType::DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::AllocatorV1>(
          bus_device, "sysmem");
  if (sysmem_client_result.is_error()) {
    zxlogf(ERROR, "Failed to get sysmem client: %s", sysmem_client_result.status_string());
    return sysmem_client_result.take_error();
  }

  zx::result<std::pair<zx::bti, std::unique_ptr<virtio::Backend>>> bti_and_backend_result =
      virtio::GetBtiAndBackend(bus_device);
  if (!bti_and_backend_result.is_ok()) {
    zxlogf(ERROR, "GetBtiAndBackend failed: %s", bti_and_backend_result.status_string());
    return bti_and_backend_result.take_error();
  }
  auto& [bti, backend] = bti_and_backend_result.value();

  zx::result<std::unique_ptr<VirtioPciDevice>> virtio_device_result =
      VirtioPciDevice::Create(std::move(bti), std::move(backend));
  if (!bti_and_backend_result.is_ok()) {
    // VirtioPciDevice::Create() logs on error.
    return bti_and_backend_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto gpu_device = fbl::make_unique_checked<VirtioGpuDevice>(
      &alloc_checker, std::move(virtio_device_result).value());
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for VirtioGpuDevice");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  auto display_engine = fbl::make_unique_checked<DisplayEngine>(
      &alloc_checker, bus_device, std::move(sysmem_client_result).value(), std::move(gpu_device));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayEngine");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t status = display_engine->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize device");
    return zx::error(status);
  }

  return zx::ok(std::move(display_engine));
}

void DisplayEngine::virtio_gpu_flusher() {
  zxlogf(TRACE, "Entering VirtioGpuFlusher()");

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

    zxlogf(TRACE, "flushing");

    if (fb_change) {
      uint32_t resource_id =
          displayed_fb_ ? displayed_fb_->resource_id : virtio_abi::kInvalidResourceId;
      zx::result<> set_scanout_result = gpu_device_->SetScanoutProperties(
          current_display_.scanout_id, resource_id, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (set_scanout_result.is_error()) {
        zxlogf(ERROR, "Failed to set scanout: %s", set_scanout_result.status_string());
        continue;
      }
    }

    if (displayed_fb_) {
      zx::result<> transfer_result = gpu_device_->TransferToHost2D(
          displayed_fb_->resource_id, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (transfer_result.is_error()) {
        zxlogf(ERROR, "Failed to transfer resource: %s", transfer_result.status_string());
        continue;
      }

      zx::result<> flush_result = gpu_device_->FlushResource(
          displayed_fb_->resource_id, current_display_.scanout_info.geometry.width,
          current_display_.scanout_info.geometry.height);
      if (flush_result.is_error()) {
        zxlogf(ERROR, "Failed to flush resource: %s", flush_result.status_string());
        continue;
      }
    }

    {
      fbl::AutoLock al(&flush_lock_);
      if (dc_intf_.ops) {
        const uint64_t banjo_display_id = display::ToBanjoDisplayId(kDisplayId);
        const config_stamp_t banjo_config_stamp =
            display::ToBanjoConfigStamp(displayed_config_stamp_);
        display_controller_interface_on_display_vsync(&dc_intf_, banjo_display_id, next_deadline,
                                                      &banjo_config_stamp);
      }
    }
    next_deadline = zx_time_add_duration(next_deadline, period);
  }
}

zx_status_t DisplayEngine::Start() {
  zxlogf(TRACE, "Start()");

  // Get the display info and see if we find a valid pmode
  zx::result<fbl::Vector<DisplayInfo>> display_infos_result = gpu_device_->GetDisplayInfo();
  if (display_infos_result.is_error()) {
    zxlogf(ERROR, "Failed to get display info: %s", display_infos_result.status_string());
    return display_infos_result.error_value();
  }

  const DisplayInfo* current_display = FirstValidDisplay(display_infos_result.value());
  if (current_display == nullptr) {
    zxlogf(ERROR, "Failed to find a usable display");
    return ZX_ERR_NOT_FOUND;
  }
  current_display_ = *current_display;

  zxlogf(INFO,
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
    zxlogf(WARNING, "Failed to move cursor: %s", move_cursor_result.status_string());
  }

  // Run a worker thread to shove in flush events
  auto virtio_gpu_flusher_entry = [](void* arg) {
    static_cast<DisplayEngine*>(arg)->virtio_gpu_flusher();
    return 0;
  };
  thrd_create_with_name(&flush_thread_, virtio_gpu_flusher_entry, this, "virtio-gpu-flusher");
  thrd_detach(flush_thread_);

  zxlogf(TRACE, "Start() completed");
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
  zxlogf(TRACE, "Init()");

  auto pid = GetKoid(zx_process_self());
  std::string debug_name = fxl::StringPrintf("virtio-gpu-display[%lu]", pid);
  auto set_debug_status =
      sysmem_->SetDebugClientInfo(fidl::StringView::FromExternal(debug_name), pid);
  if (!set_debug_status.ok()) {
    zxlogf(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
    return set_debug_status.error().status();
  }

  // Allocate a GPU request
  zx_status_t status = io_buffer_init(&gpu_req_, gpu_device_->bti().get(),
                                      zx_system_get_page_size(), IO_BUFFER_RW | IO_BUFFER_CONTIG);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate command buffers: %s", zx_status_get_string(status));
    return status;
  }

  zxlogf(TRACE, "Allocated command buffer at virtual address %p, physical address 0x%016" PRIx64,
         io_buffer_virt(&gpu_req_), io_buffer_phys(&gpu_req_));

  return ZX_OK;
}

}  // namespace virtio_display
