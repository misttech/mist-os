// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/v1/gpu.h"

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
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <algorithm>
#include <cinttypes>
#include <cstring>
#include <memory>
#include <optional>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/virtio-guest/v1/gpu-device-driver.h"
#include "src/graphics/display/drivers/virtio-guest/v1/virtio-abi.h"
#include "src/graphics/display/drivers/virtio-guest/v1/virtio-pci-device.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace virtio_display {

namespace {

constexpr uint32_t kRefreshRateHz = 30;
constexpr display::DisplayId kDisplayId{1};

zx_status_t ResponseTypeToZxStatus(virtio_abi::ControlType type) {
  if (type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04x)", ControlTypeToString(type),
           static_cast<unsigned int>(type));
    return ZX_ERR_NO_MEMORY;
  }
  return ZX_OK;
}

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

  const uint32_t width = pmode_.geometry.width;
  const uint32_t height = pmode_.geometry.height;

  const int64_t pixel_clock_hz = int64_t{width} * height * kRefreshRateHz;
  const int64_t pixel_clock_khz = (pixel_clock_hz + 500) / 1000;
  ZX_DEBUG_ASSERT(pixel_clock_khz >= 0);
  ZX_DEBUG_ASSERT(pixel_clock_khz <= std::numeric_limits<uint32_t>::max());

  const display::DisplayTiming timing = {
      .horizontal_active_px = static_cast<int32_t>(width),
      .horizontal_front_porch_px = 0,
      .horizontal_sync_width_px = 0,
      .horizontal_back_porch_px = 0,
      .vertical_active_lines = static_cast<int32_t>(height),
      .vertical_front_porch_lines = 0,
      .vertical_sync_width_lines = 0,
      .vertical_back_porch_lines = 0,
      .pixel_clock_frequency_khz = static_cast<int32_t>(pixel_clock_khz),
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

zx::result<DisplayEngine::BufferInfo> DisplayEngine::GetAllocatedBufferInfoForImage(
    display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index,
    const image_t* image) const {
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
  if (!ImageFormatMinimumRowBytes(format_constraints, image->width, &minimum_row_bytes)) {
    zxlogf(ERROR, "Invalid image width %" PRIu32 " for collection", image->width);
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

  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollection>();
  if (!endpoints.is_ok()) {
    zxlogf(ERROR, "Cannot create sysmem BufferCollection endpoints: %s", endpoints.status_string());
    return ZX_ERR_INTERNAL;
  }
  auto& [collection_client_endpoint, collection_server_endpoint] = endpoints.value();

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
    const image_t* image, uint64_t banjo_driver_buffer_collection_id, uint32_t index,
    uint64_t* out_image_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  zx::result<BufferInfo> buffer_info_result =
      GetAllocatedBufferInfoForImage(driver_buffer_collection_id, index, image);
  if (!buffer_info_result.is_ok()) {
    return buffer_info_result.error_value();
  }
  BufferInfo& buffer_info = buffer_info_result.value();
  zx::result<display::DriverImageId> import_result =
      Import(std::move(buffer_info.vmo), image, buffer_info.offset, buffer_info.bytes_per_pixel,
             buffer_info.bytes_per_row, buffer_info.pixel_format);
  if (import_result.is_ok()) {
    *out_image_handle = display::ToBanjoDriverImageId(import_result.value());
    return ZX_OK;
  }
  return import_result.error_value();
}

zx::result<display::DriverImageId> DisplayEngine::Import(
    zx::vmo vmo, const image_t* image, size_t offset, uint32_t pixel_size, uint32_t row_bytes,
    fuchsia_images2::wire::PixelFormat pixel_format) {
  if (image->type != IMAGE_TYPE_SIMPLE) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fbl::AllocChecker ac;
  auto import_data = fbl::make_unique_checked<imported_image_t>(&ac);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  unsigned size = ZX_ROUNDUP(row_bytes * image->height, zx_system_get_page_size());
  zx_paddr_t paddr;
  zx_status_t status = virtio_device_->bti().pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, vmo, offset,
                                                 size, &paddr, 1, &import_data->pmt);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to pin VMO: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = Create2DResource(&import_data->resource_id, row_bytes / pixel_size, image->height,
                            pixel_format);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate 2D resource: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = AttachResourceBacking(import_data->resource_id, paddr, size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to attach resource backing store: %s", zx_status_get_string(status));
    return zx::error(status);
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
    primary_layer_t* layer = &display_configs[0]->layer_list[0]->cfg.primary;
    frame_t frame = {
        .x_pos = 0,
        .y_pos = 0,
        .width = pmode_.geometry.width,
        .height = pmode_.geometry.height,
    };
    success = display_configs[0]->layer_list[0]->type == LAYER_TYPE_PRIMARY &&
              layer->transform_mode == FRAME_TRANSFORM_IDENTITY &&
              layer->image.width == pmode_.geometry.width &&
              layer->image.height == pmode_.geometry.height &&
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
                        : display_configs[0]->layer_list[0]->cfg.primary.image.handle;

  {
    fbl::AutoLock al(&flush_lock_);
    latest_fb_ = reinterpret_cast<imported_image_t*>(handle);
    latest_config_stamp_ = config_stamp;
  }
}

zx_status_t DisplayEngine::DisplayControllerImplGetSysmemConnection(zx::channel sysmem_handle) {
  // We can't use DdkConnectFragmentFidlProtocol here because it wants to create the endpoints but
  // we only have the server_end here.
  using ServiceMember = fuchsia_hardware_sysmem::Service::AllocatorV1;
  zx_status_t status =
      device_connect_fragment_fidl_protocol(bus_device_, "sysmem", ServiceMember::ServiceName,
                                            ServiceMember::Name, sysmem_handle.release());
  if (status != ZX_OK) {
    return status;
  }
  return ZX_OK;
}

zx_status_t DisplayEngine::DisplayControllerImplSetBufferCollectionConstraints(
    const image_t* config, uint64_t banjo_driver_buffer_collection_id) {
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
                             std::unique_ptr<VirtioPciDevice> virtio_device)
    : sysmem_(std::move(sysmem_client)),
      bus_device_(bus_device),
      virtio_device_(std::move(virtio_device)) {}

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
  auto display_engine = fbl::make_unique_checked<DisplayEngine>(
      &alloc_checker, bus_device, std::move(sysmem_client_result).value(),
      std::move(virtio_device_result).value());
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

zx_status_t DisplayEngine::GetDisplayInfo() {
  const virtio_abi::GetDisplayInfoCommand command = {
      .header = {.type = virtio_abi::ControlType::kGetDisplayInfoCommand},
  };

  const auto& response =
      virtio_device_->ExchangeRequestResponse<virtio_abi::DisplayInfoResponse>(command);
  if (response.header.type != virtio_abi::ControlType::kDisplayInfoResponse) {
    zxlogf(ERROR, "Expected DisplayInfo response, got %s (0x%04x)",
           ControlTypeToString(response.header.type),
           static_cast<unsigned int>(response.header.type));
    return ZX_ERR_NOT_FOUND;
  }

  for (int i = 0; i < virtio_abi::kMaxScanouts; i++) {
    const virtio_abi::ScanoutInfo& scanout = response.scanouts[i];
    if (!scanout.enabled) {
      continue;
    }

    zxlogf(TRACE,
           "Scanout %d: placement (%" PRIu32 ", %" PRIu32 "), resolution %" PRIu32 "x%" PRIu32
           " flags 0x%08" PRIx32,
           i, scanout.geometry.placement_x, scanout.geometry.placement_y, scanout.geometry.width,
           scanout.geometry.height, scanout.flags);
    if (pmode_id_ >= 0) {
      continue;
    }

    // Save the first valid pmode we see
    pmode_ = response.scanouts[i];
    pmode_id_ = i;
  }
  return ZX_OK;
}

namespace {

// Returns nullopt for an unsupported format.
std::optional<virtio_abi::ResourceFormat> To2DResourceFormat(
    fuchsia_images2::wire::PixelFormat pixel_format) {
  // TODO(https://fxbug.dev/42073721): Support more formats.
  switch (pixel_format) {
    case fuchsia_images2::PixelFormat::kB8G8R8A8:
      return virtio_abi::ResourceFormat::kBgra32;
    default:
      return std::nullopt;
  }
}

}  // namespace

zx_status_t DisplayEngine::Create2DResource(uint32_t* resource_id, uint32_t width, uint32_t height,
                                            fuchsia_images2::wire::PixelFormat pixel_format) {
  ZX_ASSERT(resource_id);

  zxlogf(TRACE, "Allocate2DResource");

  std::optional<virtio_abi::ResourceFormat> resource_format = To2DResourceFormat(pixel_format);
  if (!resource_format.has_value()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const virtio_abi::Create2DResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kCreate2DResourceCommand},
      .resource_id = next_resource_id_++,
      .format = virtio_abi::ResourceFormat::kBgra32,
      .width = width,
      .height = height,
  };
  *resource_id = command.resource_id;

  const auto& response =
      virtio_device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t DisplayEngine::AttachResourceBacking(uint32_t resource_id, zx_paddr_t ptr,
                                                 size_t buf_len) {
  ZX_ASSERT(ptr);

  zxlogf(TRACE,
         "AttachResourceBacking - resource ID %" PRIu32 ", address 0x%" PRIx64 ", length %zu",
         resource_id, ptr, buf_len);

  const virtio_abi::AttachResourceBackingCommand<1> command = {
      .header = {.type = virtio_abi::ControlType::kAttachResourceBackingCommand},
      .resource_id = resource_id,
      .entries =
          {
              {.address = ptr, .length = static_cast<uint32_t>(buf_len)},
          },
  };

  const auto& response =
      virtio_device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t DisplayEngine::SetScanoutProperties(uint32_t scanout_id, uint32_t resource_id,
                                                uint32_t width, uint32_t height) {
  zxlogf(TRACE,
         "SetScanoutProperties - scanout ID %" PRIu32 ", resource ID %" PRIu32 ", size %" PRIu32
         "x%" PRIu32,
         scanout_id, resource_id, width, height);

  const virtio_abi::SetScanoutCommand command = {
      .header = {.type = virtio_abi::ControlType::kSetScanoutCommand},
      .geometry =
          {
              .placement_x = 0,
              .placement_y = 0,
              .width = width,
              .height = height,
          },
      .scanout_id = scanout_id,
      .resource_id = resource_id,
  };

  const auto& response =
      virtio_device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t DisplayEngine::FlushResource(uint32_t resource_id, uint32_t width, uint32_t height) {
  zxlogf(TRACE, "FlushResource - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32, resource_id,
         width, height);

  virtio_abi::FlushResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kFlushResourceCommand},
      .geometry = {.placement_x = 0, .placement_y = 0, .width = width, .height = height},
      .resource_id = resource_id,
  };

  const auto& response =
      virtio_device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
}

zx_status_t DisplayEngine::TransferToHost2D(uint32_t resource_id, uint32_t width, uint32_t height) {
  zxlogf(TRACE, "Transfer2DResourceToHost - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32,
         resource_id, width, height);

  virtio_abi::Transfer2DResourceToHostCommand command = {
      .header = {.type = virtio_abi::ControlType::kTransfer2DResourceToHostCommand},
      .geometry =
          {
              .placement_x = 0,
              .placement_y = 0,
              .width = width,
              .height = height,
          },
      .destination_offset = 0,
      .resource_id = resource_id,
  };

  const auto& response =
      virtio_device_->ExchangeRequestResponse<virtio_abi::EmptyResponse>(command);
  return ResponseTypeToZxStatus(response.header.type);
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
      uint32_t res_id = displayed_fb_ ? displayed_fb_->resource_id : 0;
      zx_status_t status =
          SetScanoutProperties(pmode_id_, res_id, pmode_.geometry.width, pmode_.geometry.height);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to set scanout: %s", zx_status_get_string(status));
        continue;
      }
    }

    if (displayed_fb_) {
      zx_status_t status = TransferToHost2D(displayed_fb_->resource_id, pmode_.geometry.width,
                                            pmode_.geometry.height);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to transfer resource: %s", zx_status_get_string(status));
        continue;
      }

      status =
          FlushResource(displayed_fb_->resource_id, pmode_.geometry.width, pmode_.geometry.height);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to flush resource: %s", zx_status_get_string(status));
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
  zx_status_t status = GetDisplayInfo();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get display info: %s", zx_status_get_string(status));
    return status;
  }

  if (pmode_id_ < 0) {
    zxlogf(ERROR, "Failed to find a pmode");
    return ZX_ERR_NOT_FOUND;
  }

  zxlogf(INFO,
         "Found display at (%" PRIu32 ", %" PRIu32 ") size %" PRIu32 "x%" PRIu32
         ", flags 0x%08" PRIx32,
         pmode_.geometry.placement_x, pmode_.geometry.placement_y, pmode_.geometry.width,
         pmode_.geometry.height, pmode_.flags);

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
  zx_status_t status = io_buffer_init(&gpu_req_, virtio_device_->bti().get(),
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
