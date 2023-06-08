// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/virtio-guest/gpu.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/fit/defer.h>
#include <lib/image-format/image_format.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zircon-internal/align.h>
#include <string.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/time.h>

#include <cinttypes>
#include <memory>
#include <optional>
#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/virtio-guest/virtio-abi.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace virtio {

namespace {

constexpr uint32_t kRefreshRateHz = 30;
constexpr uint64_t kDisplayId = 1;

zx_status_t ResponseTypeToZxStatus(virtio_abi::ControlType type) {
  if (type != virtio_abi::ControlType::kEmptyResponse) {
    zxlogf(ERROR, "Unexpected response type: %s (0x%04x)", ControlTypeToString(type), type);
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

zx_status_t GpuDevice::DdkGetProtocol(uint32_t proto_id, void* out) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out);
  proto->ctx = this;
  if (proto_id == ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL) {
    proto->ops = &display_controller_impl_protocol_ops_;
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

void GpuDevice::DisplayControllerImplSetDisplayControllerInterface(
    const display_controller_interface_protocol_t* intf) {
  {
    fbl::AutoLock al(&flush_lock_);
    dc_intf_ = *intf;
  }

  added_display_args_t args = {
      .display_id = kDisplayId,
      .edid_present = false,
      .panel =
          {
              .params =
                  {
                      .width = pmode_.geometry.width,
                      .height = pmode_.geometry.height,
                      .refresh_rate_e2 = kRefreshRateHz * 100,
                  },
          },
      .pixel_format_list = kSupportedFormats.data(),
      .pixel_format_count = kSupportedFormats.size(),
  };
  display_controller_interface_on_displays_changed(intf, &args, 1, nullptr, 0, nullptr, 0, nullptr);
}

zx::result<GpuDevice::BufferInfo> GpuDevice::GetAllocatedBufferInfoForImage(
    uint64_t collection_id, uint32_t index, const image_t* image) const {
  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& client =
      buffer_collections_.at(collection_id);
  fidl::WireResult check_result = client->CheckBuffersAllocated();
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
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
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
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

zx_status_t GpuDevice::DisplayControllerImplImportBufferCollection(uint64_t collection_id,
                                                                   zx::channel collection_token) {
  if (buffer_collections_.find(collection_id) != buffer_collections_.end()) {
    zxlogf(ERROR, "Buffer Collection (id=%lu) already exists", collection_id);
    return ZX_ERR_ALREADY_EXISTS;
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_allocator_client_.is_valid(), "sysmem allocator is not initialized");

  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollection>();
  if (!endpoints.is_ok()) {
    zxlogf(ERROR, "Cannot create sysmem BufferCollection endpoints: %s", endpoints.status_string());
    return ZX_ERR_INTERNAL;
  }
  auto& [collection_client_endpoint, collection_server_endpoint] = endpoints.value();

  auto bind_result = sysmem_allocator_client_->BindSharedCollection(
      fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>(std::move(collection_token)),
      std::move(collection_server_endpoint));
  if (!bind_result.ok()) {
    zxlogf(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
           bind_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[collection_id] = fidl::WireSyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t GpuDevice::DisplayControllerImplReleaseBufferCollection(uint64_t collection_id) {
  if (buffer_collections_.find(collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Cannot release buffer collection %lu: buffer collection doesn't exist",
           collection_id);
    return ZX_ERR_NOT_FOUND;
  }
  buffer_collections_.erase(collection_id);
  return ZX_OK;
}

zx_status_t GpuDevice::DisplayControllerImplImportImage(image_t* image, uint64_t collection_id,
                                                        uint32_t index) {
  const auto it = buffer_collections_.find(collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)", collection_id);
    return ZX_ERR_NOT_FOUND;
  }

  zx::result<BufferInfo> buffer_info_result =
      GetAllocatedBufferInfoForImage(collection_id, index, image);
  if (!buffer_info_result.is_ok()) {
    return buffer_info_result.error_value();
  }
  BufferInfo& buffer_info = buffer_info_result.value();
  return Import(std::move(buffer_info.vmo), image, buffer_info.offset, buffer_info.bytes_per_pixel,
                buffer_info.bytes_per_row, buffer_info.pixel_format);
}

zx_status_t GpuDevice::Import(zx::vmo vmo, image_t* image, size_t offset, uint32_t pixel_size,
                              uint32_t row_bytes, fuchsia_images2::wire::PixelFormat pixel_format) {
  if (image->type != IMAGE_TYPE_SIMPLE) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  auto import_data = fbl::make_unique_checked<imported_image_t>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  unsigned size = ZX_ROUNDUP(row_bytes * image->height, zx_system_get_page_size());
  zx_paddr_t paddr;
  zx_status_t status = bti_.pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, vmo, offset, size, &paddr, 1,
                                &import_data->pmt);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to pin VMO: %s", zx_status_get_string(status));
    return status;
  }

  status = allocate_2d_resource(&import_data->resource_id, row_bytes / pixel_size, image->height,
                                pixel_format);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate 2D resource: %s", zx_status_get_string(status));
    return status;
  }

  status = attach_backing(import_data->resource_id, paddr, size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to attach resource backing store: %s", zx_status_get_string(status));
    return status;
  }

  image->handle = reinterpret_cast<uint64_t>(import_data.release());

  return ZX_OK;
}

void GpuDevice::DisplayControllerImplReleaseImage(image_t* image) {
  delete reinterpret_cast<imported_image_t*>(image->handle);
}

config_check_result_t GpuDevice::DisplayControllerImplCheckConfiguration(
    const display_config_t** display_configs, size_t display_count, uint32_t** layer_cfg_results,
    size_t* layer_cfg_result_count) {
  if (display_count != 1) {
    ZX_DEBUG_ASSERT(display_count == 0);
    return CONFIG_CHECK_RESULT_OK;
  }
  ZX_DEBUG_ASSERT(display_configs[0]->display_id == kDisplayId);
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
    layer_cfg_results[0][0] = CLIENT_MERGE_BASE;
    for (unsigned i = 1; i < display_configs[0]->layer_count; i++) {
      layer_cfg_results[0][i] = CLIENT_MERGE_SRC;
    }
    layer_cfg_result_count[0] = display_configs[0]->layer_count;
  }
  return CONFIG_CHECK_RESULT_OK;
}

void GpuDevice::DisplayControllerImplApplyConfiguration(const display_config_t** display_configs,
                                                        size_t display_count,
                                                        const config_stamp_t* config_stamp) {
  ZX_DEBUG_ASSERT(config_stamp);
  uint64_t handle = display_count == 0 || display_configs[0]->layer_count == 0
                        ? 0
                        : display_configs[0]->layer_list[0]->cfg.primary.image.handle;

  {
    fbl::AutoLock al(&flush_lock_);
    latest_fb_ = reinterpret_cast<imported_image_t*>(handle);
    latest_config_stamp_ = *config_stamp;
  }
}

zx_status_t GpuDevice::DisplayControllerImplGetSysmemConnection(zx::channel sysmem_handle) {
  auto result =
      sysmem_->ConnectServer(fidl::ServerEnd<fuchsia_sysmem::Allocator>{std::move(sysmem_handle)});
  if (!result.ok()) {
    return result.status();
  }

  return ZX_OK;
}

zx_status_t GpuDevice::DisplayControllerImplSetBufferCollectionConstraints(const image_t* config,
                                                                           uint64_t collection_id) {
  const auto it = buffer_collections_.find(collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "SetBufferCollectionConstraints: Cannot find imported buffer collection (id=%lu)",
           collection_id);
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
    zxlogf(ERROR, "virtio::GpuDevice: Failed to set constraints");
    return status;
  }

  return ZX_OK;
}

zx_status_t GpuDevice::DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on) {
  return ZX_ERR_NOT_SUPPORTED;
}

GpuDevice::GpuDevice(zx_device_t* bus_device, zx::bti bti, std::unique_ptr<Backend> backend)
    : virtio::Device(bus_device, std::move(bti), std::move(backend)), DeviceType(bus_device) {
  sem_init(&request_sem_, 0, 1);
  sem_init(&response_sem_, 0, 0);
}

GpuDevice::~GpuDevice() {
  io_buffer_release(&gpu_req_);

  // TODO: clean up allocated physical memory
  sem_destroy(&request_sem_);
  sem_destroy(&response_sem_);
}

template <typename RequestType, typename ResponseType>
void GpuDevice::send_command_response(const RequestType* cmd, ResponseType** res) {
  size_t cmd_len = sizeof(RequestType);
  size_t res_len = sizeof(ResponseType);
  zxlogf(TRACE,
         "Sending command (buffer at %p, length %zu), expecting reply (pointer at %p, length %zu)",
         cmd, cmd_len, res, res_len);

  // Keep this single message at a time
  sem_wait(&request_sem_);
  auto cleanup = fit::defer([this]() { sem_post(&request_sem_); });

  uint16_t i;
  struct vring_desc* desc = vring_.AllocDescChain(2, &i);
  ZX_ASSERT(desc);

  void* gpu_req_base = io_buffer_virt(&gpu_req_);
  zx_paddr_t gpu_req_pa = io_buffer_phys(&gpu_req_);

  memcpy(gpu_req_base, cmd, cmd_len);

  desc->addr = gpu_req_pa;
  desc->len = static_cast<uint32_t>(cmd_len);
  desc->flags = VRING_DESC_F_NEXT;

  // Set the second descriptor to the response with the write bit set
  desc = vring_.DescFromIndex(desc->next);
  ZX_ASSERT(desc);

  *res = reinterpret_cast<ResponseType*>(static_cast<uint8_t*>(gpu_req_base) + cmd_len);
  zx_paddr_t res_phys = gpu_req_pa + cmd_len;
  memset(*res, 0, res_len);

  desc->addr = res_phys;
  desc->len = static_cast<uint32_t>(res_len);
  desc->flags = VRING_DESC_F_WRITE;

  // Submit the transfer & wait for the response
  vring_.SubmitChain(i);
  vring_.Kick();
  sem_wait(&response_sem_);
}

zx_status_t GpuDevice::get_display_info() {
  const virtio_abi::GetDisplayInfoCommand command = {
      .header = {.type = virtio_abi::ControlType::kGetDisplayInfoCommand},
  };

  virtio_abi::DisplayInfoResponse* response;
  send_command_response(&command, &response);
  if (response->header.type != virtio_abi::ControlType::kDisplayInfoResponse) {
    zxlogf(ERROR, "Expected DisplayInfo response, got %s (0x%04x)",
           ControlTypeToString(response->header.type), response->header.type);
    return ZX_ERR_NOT_FOUND;
  }

  for (int i = 0; i < virtio_abi::kMaxScanouts; i++) {
    const virtio_abi::ScanoutInfo& scanout = response->scanouts[i];
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
    pmode_ = response->scanouts[i];
    pmode_id_ = i;
  }
  return ZX_OK;
}

namespace {

// Returns nullopt for an unsupported format.
std::optional<virtio_abi::ResourceFormat> To2DResourceFormat(
    fuchsia_images2::wire::PixelFormat pixel_format) {
  // TODO(fxbug.dev/122802): Support more formats.
  switch (pixel_format) {
    case fuchsia_images2::PixelFormat::kBgra32:
      return virtio_abi::ResourceFormat::kBgra32;
    default:
      return std::nullopt;
  }
}

}  // namespace

zx_status_t GpuDevice::allocate_2d_resource(uint32_t* resource_id, uint32_t width, uint32_t height,
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

  virtio_abi::EmptyResponse* response;
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::attach_backing(uint32_t resource_id, zx_paddr_t ptr, size_t buf_len) {
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

  virtio_abi::EmptyResponse* response;
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::set_scanout(uint32_t scanout_id, uint32_t resource_id, uint32_t width,
                                   uint32_t height) {
  zxlogf(TRACE,
         "SetScanout - scanout ID %" PRIu32 ", resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32,
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

  virtio_abi::EmptyResponse* response;
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::flush_resource(uint32_t resource_id, uint32_t width, uint32_t height) {
  zxlogf(TRACE, "FlushResource - resource ID %" PRIu32 ", size %" PRIu32 "x%" PRIu32, resource_id,
         width, height);

  virtio_abi::FlushResourceCommand command = {
      .header = {.type = virtio_abi::ControlType::kFlushResourceCommand},
      .geometry = {.placement_x = 0, .placement_y = 0, .width = width, .height = height},
      .resource_id = resource_id,
  };

  virtio_abi::EmptyResponse* response;
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

zx_status_t GpuDevice::transfer_to_host_2d(uint32_t resource_id, uint32_t width, uint32_t height) {
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

  virtio_abi::EmptyResponse* response;
  send_command_response(&command, &response);
  return ResponseTypeToZxStatus(response->header.type);
}

void GpuDevice::virtio_gpu_flusher() {
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

    if (displayed_fb_) {
      zx_status_t status = transfer_to_host_2d(displayed_fb_->resource_id, pmode_.geometry.width,
                                               pmode_.geometry.height);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to transfer resource: %s", zx_status_get_string(status));
        continue;
      }

      status =
          flush_resource(displayed_fb_->resource_id, pmode_.geometry.width, pmode_.geometry.height);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to flush resource: %s", zx_status_get_string(status));
        continue;
      }
    }

    if (fb_change) {
      uint32_t res_id = displayed_fb_ ? displayed_fb_->resource_id : 0;
      zx_status_t status =
          set_scanout(pmode_id_, res_id, pmode_.geometry.width, pmode_.geometry.height);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to set scanout: %s", zx_status_get_string(status));
        continue;
      }
    }

    {
      fbl::AutoLock al(&flush_lock_);
      if (dc_intf_.ops) {
        display_controller_interface_on_display_vsync(&dc_intf_, kDisplayId, next_deadline,
                                                      &displayed_config_stamp_);
      }
    }
    next_deadline = zx_time_add_duration(next_deadline, period);
  }
}

zx_status_t GpuDevice::virtio_gpu_start() {
  zxlogf(TRACE, "VirtioGpuStart()");

  // Get the display info and see if we find a valid pmode
  zx_status_t status = get_display_info();
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
    static_cast<GpuDevice*>(arg)->virtio_gpu_flusher();
    return 0;
  };
  thrd_create_with_name(&flush_thread_, virtio_gpu_flusher_entry, this, "virtio-gpu-flusher");
  thrd_detach(flush_thread_);

  zxlogf(TRACE, "VirtioGpuStart() publishing device");

  status = DdkAdd("virtio-gpu-display");
  device_ = zxdev();

  if (status != ZX_OK) {
    device_ = nullptr;
    return status;
  }

  zxlogf(TRACE, "VirtioGpuStart() completed");
  return ZX_OK;
}

zx_status_t GpuDevice::InitSysmemAllocatorClient() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  if (!endpoints.is_ok()) {
    zxlogf(ERROR, "Cannot create sysmem allocator endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }
  auto& [client, server] = endpoints.value();
  auto connect_result = sysmem_->ConnectServer(std::move(server));
  if (!connect_result.ok()) {
    zxlogf(ERROR, "Cannot connect to sysmem Allocator protocol: %s",
           connect_result.status_string());
    return connect_result.status();
  }
  sysmem_allocator_client_ = fidl::WireSyncClient(std::move(client));

  std::string debug_name =
      fxl::StringPrintf("virtio-gpu-display[%lu]", fsl::GetCurrentProcessKoid());
  auto set_debug_status = sysmem_allocator_client_->SetDebugClientInfo(
      fidl::StringView::FromExternal(debug_name), fsl::GetCurrentProcessKoid());
  if (!set_debug_status.ok()) {
    zxlogf(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
  }

  return ZX_OK;
}

zx_status_t GpuDevice::Init() {
  zxlogf(TRACE, "Init()");

  zx::result client =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::Sysmem>("sysmem-fidl");
  if (client.is_error()) {
    zxlogf(ERROR, "Could not get Display SYSMEM protocol: %s", client.status_string());
    return client.status_value();
  }

  sysmem_ = fidl::WireSyncClient(std::move(*client));

  zx_status_t status = InitSysmemAllocatorClient();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to create sysmem Allocator client: %s", zx_status_get_string(status));
    return status;
  }

  DeviceReset();

  virtio_abi::GpuDeviceConfig config;
  CopyDeviceConfig(&config, sizeof(config));
  zxlogf(TRACE, "GpuDeviceConfig - pending events: 0x%08" PRIx32, config.pending_events);
  zxlogf(TRACE, "GpuDeviceConfig - scanout limit: %d", config.scanout_limit);
  zxlogf(TRACE, "GpuDeviceConfig - capability set limit: %d", config.capability_set_limit);

  // Ack and set the driver status bit
  DriverStatusAck();

  if (!DeviceFeaturesSupported(VIRTIO_F_VERSION_1)) {
    // Declaring non-support until there is a need in the future.
    zxlogf(ERROR, "Legacy virtio interface is not supported by this driver");
    return ZX_ERR_NOT_SUPPORTED;
  }
  DriverFeaturesAck(VIRTIO_F_VERSION_1);
  if (zx_status_t status = DeviceStatusFeaturesOk(); status != ZX_OK) {
    zxlogf(ERROR, "Feature negotiation failed: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate the main vring
  status = vring_.Init(0, 16);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate vring: %s", zx_status_get_string(status));
    return status;
  }

  // Allocate a GPU request
  status = io_buffer_init(&gpu_req_, bti_.get(), zx_system_get_page_size(),
                          IO_BUFFER_RW | IO_BUFFER_CONTIG);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate command buffers: %s", zx_status_get_string(status));
    return status;
  }

  zxlogf(TRACE, "Allocated command buffer at virtual address %p, physical address 0x%016" PRIx64,
         io_buffer_virt(&gpu_req_), io_buffer_phys(&gpu_req_));

  StartIrqThread();
  DriverStatusOk();

  // Start a worker thread that runs through a sequence to finish initializing the GPU
  auto virtio_gpu_start_entry = [](void* arg) {
    return static_cast<GpuDevice*>(arg)->virtio_gpu_start();
  };
  thrd_create_with_name(&start_thread_, virtio_gpu_start_entry, this, "virtio-gpu-starter");
  thrd_detach(start_thread_);

  return ZX_OK;
}

void GpuDevice::IrqRingUpdate() {
  zxlogf(TRACE, "IrqRingUpdate()");

  // Parse our descriptor chain, add back to the free queue
  auto free_chain = [this](vring_used_elem* used_elem) {
    uint16_t i = static_cast<uint16_t>(used_elem->id);
    struct vring_desc* desc = vring_.DescFromIndex(i);
    for (;;) {
      int next;

      if (desc->flags & VRING_DESC_F_NEXT) {
        next = desc->next;
      } else {
        // End of chain
        next = -1;
      }

      vring_.FreeDesc(i);

      if (next < 0) {
        break;
      }
      i = static_cast<uint16_t>(next);
      desc = vring_.DescFromIndex(i);
    }
    // Notify the request thread
    sem_post(&response_sem_);
  };

  // Tell the ring to find free chains and hand it back to our lambda
  vring_.IrqRingUpdate(free_chain);
}

void GpuDevice::IrqConfigChange() { zxlogf(TRACE, "IrqConfigChange()"); }

}  // namespace virtio
