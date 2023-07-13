// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/amlogic-display.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/clamprgb/cpp/banjo.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <fuchsia/hardware/dsiimpl/cpp/banjo.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/image-format/image_format.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zircon-internal/align.h>
#include <lib/zx/channel.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <cstddef>
#include <iterator>

#include <ddk/metadata/display.h>
#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace amlogic_display {

// Currently amlogic-display implementation uses pointers to ImageInfo as
// handles to images, while handles to images are defined as a fixed-size
// uint64_t in the banjo protocol. This works on platforms where uint64_t and
// uintptr_t are equivalent but this may cause portability issues in the future.
// TODO(fxbug.dev/128653): Do not use pointers as handles.
static_assert(std::is_same_v<uint64_t, uintptr_t>);

namespace {

// List of supported pixel formats.
// TODO(fxbug.dev/69236): Add more supported formats.
constexpr std::array<fuchsia_images2::wire::PixelFormat, 2> kSupportedPixelFormats = {
    fuchsia_images2::wire::PixelFormat::kBgra32,
    fuchsia_images2::wire::PixelFormat::kR8G8B8A8,
};

constexpr std::array<fuchsia_images2_pixel_format_enum_value_t, 2> kSupportedBanjoPixelFormats = {
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kBgra32),
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
};

constexpr uint32_t kCanvasLittleEndian64Bit = 7;
constexpr uint32_t kBufferAlignment = 64;

bool IsFormatSupported(fuchsia_images2::wire::PixelFormat format) {
  return std::find(kSupportedPixelFormats.begin(), kSupportedPixelFormats.end(), format) !=
         kSupportedPixelFormats.end();
}

void SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType format,
                                      uint64_t modifier,
                                      fuchsia_sysmem::wire::ImageFormatConstraints& constraints) {
  constraints.color_spaces_count = 1;
  constraints.color_space[0].type = fuchsia_sysmem::wire::ColorSpaceType::kSrgb;
  constraints.pixel_format = {
      .type = format,
      .has_format_modifier = true,
      .format_modifier =
          {
              .value = modifier,
          },
  };
  constraints.bytes_per_row_divisor = kBufferAlignment;
  constraints.start_offset_divisor = kBufferAlignment;
}

}  // namespace

zx_status_t AmlogicDisplay::DisplayClampRgbImplSetMinimumRgb(uint8_t minimum_rgb) {
  if (fully_initialized()) {
    osd_->SetMinimumRgb(minimum_rgb);
    return ZX_OK;
  }
  return ZX_ERR_INTERNAL;
}

zx_status_t AmlogicDisplay::RestartDisplay() {
  if (!fully_initialized()) {
    return ZX_ERR_INTERNAL;
  }
  vpu_->PowerOff();
  vpu_->PowerOn();
  vpu_->VppInit();
  // Need to call this function since VPU/VPP registers were reset
  vpu_->SetFirstTimeDriverLoad();

  return vout_->RestartDisplay();
}

zx_status_t AmlogicDisplay::DisplayInit() {
  ZX_ASSERT(!fully_initialized());
  zx_status_t status;
  fbl::AllocChecker ac;

  // Setup VPU and VPP units first
  vpu_ = fbl::make_unique_checked<Vpu>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  status = vpu_->Init(pdev_);
  if (status != ZX_OK) {
    DISP_ERROR("Could not initialize VPU object\n");
    return status;
  }

  // Determine whether it's first time boot or not
  const bool skip_disp_init = vpu_->SetFirstTimeDriverLoad();
  if (skip_disp_init) {
    DISP_INFO("First time driver load. Skip display initialization\n");
    // Make sure AFBC engine is on. Since bootloader does not use AFBC, it might not have powered
    // on AFBC engine.
    vpu_->AfbcPower(true);
  } else {
    DISP_INFO("Display driver reloaded. Initialize display system\n");
    RestartDisplay();
  }

  // The "osd" node must be created because these metric paths are load-bearing
  // for some triage workflows.
  osd_node_ = root_node_.CreateChild("osd");
  auto osd_or_status = Osd::Create(&pdev_, vout_->fb_width(), vout_->fb_height(),
                                   vout_->display_width(), vout_->display_height(), &osd_node_);
  if (osd_or_status.is_error()) {
    return osd_or_status.status_value();
  }
  osd_ = std::move(osd_or_status).value();

  osd_->HwInit();
  return ZX_OK;
}

// part of ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL ops
void AmlogicDisplay::DisplayControllerImplSetDisplayControllerInterface(
    const display_controller_interface_protocol_t* intf) {
  fbl::AutoLock lock(&display_lock_);
  dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient(intf);

  if (display_attached_) {
    added_display_info_t info{.is_standard_srgb_out = false};  // Random default
    added_display_args_t args;
    vout_->PopulateAddedDisplayArgs(&args, display_id_, kSupportedBanjoPixelFormats);
    dc_intf_.OnDisplaysChanged(&args, 1, nullptr, 0, &info, 1, nullptr);
    vout_->OnDisplaysChanged(info);
  }
}

// part of ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL ops
zx_status_t AmlogicDisplay::DisplayControllerImplImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    zxlogf(ERROR, "Buffer Collection (id=%lu) already exists", driver_buffer_collection_id.value());
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

  buffer_collections_[driver_buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

// part of ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL ops
zx_status_t AmlogicDisplay::DisplayControllerImplReleaseBufferCollection(
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

// part of ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL ops
zx_status_t AmlogicDisplay::DisplayControllerImplImportImage(
    image_t* image, uint64_t banjo_driver_buffer_collection_id, uint32_t index) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Cannot import Image on collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  zx_status_t status = ZX_OK;
  auto import_info = std::make_unique<ImageInfo>();
  if (import_info == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }

  if (image->type != IMAGE_TYPE_SIMPLE) {
    status = ZX_ERR_INVALID_ARGS;
    return status;
  }

  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& collection =
      buffer_collections_.at(driver_buffer_collection_id);
  fidl::WireResult check_result = collection->CheckBuffersAllocated();
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    return check_result.status();
  }
  const auto& check_response = check_result.value();
  if (check_response.status == ZX_ERR_UNAVAILABLE) {
    return ZX_ERR_SHOULD_WAIT;
  }
  if (check_response.status != ZX_OK) {
    return check_response.status;
  }

  fidl::WireResult wait_result = collection->WaitForBuffersAllocated();
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    return wait_result.status();
  }
  auto& wait_response = wait_result.value();
  if (wait_response.status != ZX_OK) {
    return wait_response.status;
  }
  fuchsia_sysmem::wire::BufferCollectionInfo2& collection_info =
      wait_response.buffer_collection_info;

  if (!collection_info.settings.has_image_format_constraints ||
      index >= collection_info.buffer_count) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (const auto v2_pixel_format = sysmem::V2CopyFromV1PixelFormatType(
          collection_info.settings.image_format_constraints.pixel_format.type);
      !format_support_check_(v2_pixel_format)) {
    zxlogf(ERROR, "Cannot import image: pixel format %u not supported",
           static_cast<uint32_t>(v2_pixel_format));
    return ZX_ERR_NOT_SUPPORTED;
  }

  import_info->pixel_format = sysmem::V2CopyFromV1PixelFormat(
      collection_info.settings.image_format_constraints.pixel_format);

  ZX_DEBUG_ASSERT(
      collection_info.settings.image_format_constraints.pixel_format.has_format_modifier);

  const auto format_modifier =
      collection_info.settings.image_format_constraints.pixel_format.format_modifier.value;

  switch (format_modifier) {
    case fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuv:
    case fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTe: {
      // AFBC does not use canvas.
      uint64_t offset = collection_info.buffers[index].vmo_usable_start;
      size_t size =
          ZX_ROUNDUP(ImageFormatImageSize(
                         ImageConstraintsToFormat(collection_info.settings.image_format_constraints,
                                                  image->width, image->height)
                             .value()),
                     PAGE_SIZE);
      zx_paddr_t paddr;
      zx_status_t status =
          bti_.pin(ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, collection_info.buffers[index].vmo,
                   offset & ~(PAGE_SIZE - 1), size, &paddr, 1, &import_info->pmt);
      if (status != ZX_OK) {
        DISP_ERROR("Could not pin BTI (%d)\n", status);
        return status;
      }
      import_info->paddr = paddr;
      import_info->image_height = image->height;
      import_info->image_width = image->width;
      import_info->is_afbc = true;
    } break;
    case fuchsia_sysmem::wire::kFormatModifierLinear:
    case fuchsia_sysmem::wire::kFormatModifierArmLinearTe: {
      uint32_t minimum_row_bytes;
      if (!ImageFormatMinimumRowBytes(collection_info.settings.image_format_constraints,
                                      image->width, &minimum_row_bytes)) {
        DISP_ERROR("Invalid image width %d for collection\n", image->width);
        return ZX_ERR_INVALID_ARGS;
      }

      fuchsia_hardware_amlogiccanvas::wire::CanvasInfo canvas_info;
      canvas_info.height = image->height;
      canvas_info.stride_bytes = minimum_row_bytes;
      canvas_info.blkmode = fuchsia_hardware_amlogiccanvas::CanvasBlockMode::kLinear;
      canvas_info.endianness = fuchsia_hardware_amlogiccanvas::CanvasEndianness();
      canvas_info.flags = fuchsia_hardware_amlogiccanvas::CanvasFlags::kRead;

      fidl::WireResult result =
          canvas_->Config(std::move(collection_info.buffers[index].vmo),
                          collection_info.buffers[index].vmo_usable_start, canvas_info);
      if (!result.ok()) {
        DISP_ERROR("Could not configure canvas: %s\n", result.error().FormatDescription().c_str());
        return ZX_ERR_NO_RESOURCES;
      }
      if (result->is_error()) {
        DISP_ERROR("Could not configure canvas: %s\n", zx_status_get_string(result->error_value()));
        return ZX_ERR_NO_RESOURCES;
      }

      import_info->canvas = canvas_.client_end();
      import_info->canvas_idx = result->value()->canvas_idx;
      import_info->image_height = image->height;
      import_info->image_width = image->width;
      import_info->is_afbc = false;
    } break;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid pixel format modifier: %lu\n", format_modifier);
      return ZX_ERR_INVALID_ARGS;
  }
  // TODO(fxbug.dev/128653): Using pointers as handles impedes portability of
  // the driver. Do not use pointers as handles.
  image->handle = reinterpret_cast<uint64_t>(import_info.get());
  fbl::AutoLock lock(&image_lock_);
  imported_images_.push_back(std::move(import_info));
  return status;
}

// part of ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL ops
void AmlogicDisplay::DisplayControllerImplReleaseImage(image_t* image) {
  fbl::AutoLock lock(&image_lock_);
  auto info = reinterpret_cast<ImageInfo*>(image->handle);
  imported_images_.erase(*info);
}

// part of ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL ops
config_check_result_t AmlogicDisplay::DisplayControllerImplCheckConfiguration(
    const display_config_t** display_configs, size_t display_count,
    client_composition_opcode_t** layer_cfg_results, size_t* layer_cfg_result_count) {
  if (display_count != 1) {
    ZX_DEBUG_ASSERT(display_count == 0);
    return CONFIG_CHECK_RESULT_OK;
  }

  fbl::AutoLock lock(&display_lock_);

  // no-op, just wait for the client to try a new config
  if (!display_attached_ || display::ToDisplayId(display_configs[0]->display_id) != display_id_) {
    return CONFIG_CHECK_RESULT_OK;
  }

  if (vout_->CheckMode(&display_configs[0]->mode)) {
    return CONFIG_CHECK_RESULT_UNSUPPORTED_MODES;
  }

  bool success = true;

  if (display_configs[0]->layer_count > 1) {
    // We only support 1 layer
    success = false;
  }

  if (success && display_configs[0]->cc_flags) {
    // Make sure cc values are correct
    if (display_configs[0]->cc_flags & COLOR_CONVERSION_PREOFFSET) {
      for (float cc_preoffset : display_configs[0]->cc_preoffsets) {
        success = success && cc_preoffset > -1;
        success = success && cc_preoffset < 1;
      }
    }
    if (success && display_configs[0]->cc_flags & COLOR_CONVERSION_POSTOFFSET) {
      for (float cc_postoffset : display_configs[0]->cc_postoffsets) {
        success = success && cc_postoffset > -1;
        success = success && cc_postoffset < 1;
      }
    }
  }

  if (success) {
    const uint32_t width = display_configs[0]->mode.h_addressable;
    const uint32_t height = display_configs[0]->mode.v_addressable;
    // Make sure ther layer configuration is supported
    const primary_layer_t& layer = display_configs[0]->layer_list[0]->cfg.primary;
    frame_t frame = {
        .x_pos = 0,
        .y_pos = 0,
        .width = width,
        .height = height,
    };

    if (layer.alpha_mode == ALPHA_PREMULTIPLIED) {
      // we don't support pre-multiplied alpha mode
      layer_cfg_results[0][0] |= CLIENT_COMPOSITION_OPCODE_ALPHA;
    }
    success = display_configs[0]->layer_list[0]->type == LAYER_TYPE_PRIMARY &&
              layer.transform_mode == FRAME_TRANSFORM_IDENTITY && layer.image.width == width &&
              layer.image.height == height &&
              memcmp(&layer.dest_frame, &frame, sizeof(frame_t)) == 0 &&
              memcmp(&layer.src_frame, &frame, sizeof(frame_t)) == 0;
  }
  if (!success) {
    layer_cfg_results[0][0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
    for (unsigned i = 1; i < display_configs[0]->layer_count; i++) {
      layer_cfg_results[0][i] = CLIENT_COMPOSITION_OPCODE_MERGE_SRC;
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

// part of ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL ops
void AmlogicDisplay::DisplayControllerImplApplyConfiguration(
    const display_config_t** display_configs, size_t display_count,
    const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(display_configs);
  ZX_DEBUG_ASSERT(banjo_config_stamp);
  const display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);

  fbl::AutoLock lock(&display_lock_);

  if (display_count == 1 && display_configs[0]->layer_count) {
    // Setting up OSD may require Vout framebuffer information, which may be
    // changed on each ApplyConfiguration(), so we need to apply the
    // configuration to Vout first before initializing the display and OSD.
    if (zx_status_t status = vout_->ApplyConfiguration(&display_configs[0]->mode);
        status != ZX_OK) {
      DISP_ERROR("Could not apply config to Vout! %d\n", status);
      return;
    }

    if (!fully_initialized()) {
      if (zx_status_t status = DisplayInit(); status != ZX_OK) {
        DISP_ERROR("Display Hardware Initialization failed! %d\n", status);
        ZX_ASSERT(0);
      }
      set_fully_initialized();
    }

    // The only way a checked configuration could now be invalid is if display was
    // unplugged. If that's the case, then the upper layers will give a new configuration
    // once they finish handling the unplug event. So just return.
    if (!display_attached_ || display::ToDisplayId(display_configs[0]->display_id) != display_id_) {
      return;
    }

    // Since Amlogic does not support plug'n play (fixed display), there is no way
    // a checked configuration could be invalid at this point.
    auto info =
        reinterpret_cast<ImageInfo*>(display_configs[0]->layer_list[0]->cfg.primary.image.handle);
    osd_->FlipOnVsync(info->canvas_idx, display_configs[0], config_stamp);
  } else {
    if (fully_initialized()) {
      {
        fbl::AutoLock lock2(&capture_lock_);
        if (current_capture_target_image_ != nullptr) {
          // there's an active capture. stop it before disabling osd
          vpu_->CaptureDone();
          current_capture_target_image_ = nullptr;
        }
      }
      osd_->Disable(config_stamp);
    }
  }

  // If bootloader does not enable any of the display hardware, no vsync will be generated.
  // This fakes a vsync to let clients know we are ready until we actually initialize hardware
  if (!fully_initialized()) {
    if (dc_intf_.is_valid()) {
      if (display_count == 0 || display_configs[0]->layer_count == 0) {
        const config_stamp_t banjo_config_stamp_out = display::ToBanjoConfigStamp(config_stamp);
        dc_intf_.OnDisplayVsync(display::ToBanjoDisplayId(display_id_), zx_clock_get_monotonic(),
                                &banjo_config_stamp_out);
      }
    }
  }
}

void AmlogicDisplay::DdkSuspend(ddk::SuspendTxn txn) {
  if (txn.suspend_reason() != DEVICE_SUSPEND_REASON_MEXEC) {
    txn.Reply(ZX_ERR_NOT_SUPPORTED, txn.requested_state());
    return;
  }
  if (fully_initialized()) {
    osd_->Disable();
  }

  fbl::AutoLock l(&image_lock_);
  for (auto& i : imported_images_) {
    if (i.pmt) {
      i.pmt.unpin();
    }
    if (i.canvas.has_value() && i.canvas_idx > 0) {
      fidl::WireResult result = fidl::WireCall(i.canvas.value())->Free(i.canvas_idx);
    }
  }
  txn.Reply(ZX_OK, txn.requested_state());
}

void AmlogicDisplay::DdkResume(ddk::ResumeTxn txn) {
  if (fully_initialized()) {
    osd_->Enable();
  }
  txn.Reply(ZX_OK, DEV_POWER_STATE_D0, txn.requested_state());
}

void AmlogicDisplay::DdkRelease() {
  vsync_irq_.destroy();
  thrd_join(vsync_thread_, nullptr);
  if (fully_initialized()) {
    osd_->Release();
    vpu_->PowerOff();
  }

  vd1_wr_irq_.destroy();
  thrd_join(capture_thread_, nullptr);
  hpd_irq_.destroy();
  thrd_join(hpd_thread_, nullptr);
  delete this;
}

zx_status_t AmlogicDisplay::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out_protocol);
  proto->ctx = this;
  switch (proto_id) {
    case ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL:
      proto->ops = &display_controller_impl_protocol_ops_;
      return ZX_OK;
    case ZX_PROTOCOL_DISPLAY_CLAMP_RGB_IMPL:
      proto->ops = &display_clamp_rgb_impl_protocol_ops_;
      return ZX_OK;
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t AmlogicDisplay::DisplayControllerImplGetSysmemConnection(zx::channel connection) {
  auto status =
      sysmem_->ConnectServer(fidl::ServerEnd<fuchsia_sysmem::Allocator>(std::move(connection)));
  if (!status.ok()) {
    DISP_ERROR("Failed to connect to sysmem: %s\n", status.status_string());
    return status.status();
  }

  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplSetBufferCollectionConstraints(
    const image_t* config, uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR,
           "Cannot set buffer collection constraints for %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& collection =
      buffer_collections_.at(driver_buffer_collection_id);
  fuchsia_sysmem::wire::BufferCollectionConstraints constraints = {};
  const char* buffer_name;
  if (config->type == IMAGE_TYPE_CAPTURE) {
    constraints.usage.cpu =
        fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  } else {
    constraints.usage.display = fuchsia_sysmem::wire::kDisplayUsageLayer;
  }
  constraints.has_buffer_memory_constraints = true;
  fuchsia_sysmem::wire::BufferMemoryConstraints& buffer_constraints =
      constraints.buffer_memory_constraints;
  buffer_constraints.physically_contiguous_required = true;
  buffer_constraints.secure_required = false;
  buffer_constraints.ram_domain_supported = true;
  buffer_constraints.cpu_domain_supported = false;
  buffer_constraints.inaccessible_domain_supported = true;
  buffer_constraints.heap_permitted_count = 2;
  buffer_constraints.heap_permitted[0] = fuchsia_sysmem::wire::HeapType::kSystemRam;
  buffer_constraints.heap_permitted[1] = fuchsia_sysmem::wire::HeapType::kAmlogicSecure;

  if (config->type == IMAGE_TYPE_CAPTURE) {
    constraints.image_format_constraints_count = 1;
    fuchsia_sysmem::wire::ImageFormatConstraints& image_constraints =
        constraints.image_format_constraints[0];

    SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType::kBgr24,
                                     fuchsia_sysmem::wire::kFormatModifierLinear,
                                     image_constraints);
    image_constraints.min_coded_width = vout_->display_width();
    image_constraints.max_coded_width = vout_->display_width();
    image_constraints.min_coded_height = vout_->display_height();
    image_constraints.max_coded_height = vout_->display_height();
    // Amlogic display capture engine (VDIN) outputs in formats with 3 bytes per
    // pixel.
    constexpr uint32_t kCaptureImageBytesPerPixel = 3;
    image_constraints.min_bytes_per_row =
        fbl::round_up(vout_->display_width() * kCaptureImageBytesPerPixel, kBufferAlignment);
    image_constraints.max_coded_width_times_coded_height =
        vout_->display_width() * vout_->display_height();
    buffer_name = "Display capture";
  } else {
    // TODO(fxbug.dev/94535): Currently the buffer collection constraints are
    // applied to all displays. If the |vout_| device type changes, then the
    // existing image formats might not work for the new device type. To resolve
    // this, the driver should set per-display buffer collection constraints
    // instead.
    constraints.image_format_constraints_count = 0;
    ZX_DEBUG_ASSERT(format_support_check_ != nullptr);
    if (format_support_check_(fuchsia_images2::wire::PixelFormat::kBgra32)) {
      for (const auto format_modifier : {fuchsia_sysmem::wire::kFormatModifierLinear,
                                         fuchsia_sysmem::wire::kFormatModifierArmLinearTe}) {
        const size_t index = constraints.image_format_constraints_count++;
        auto& image_constraints = constraints.image_format_constraints[index];
        SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType::kBgra32,
                                         format_modifier, image_constraints);
      }
    }
    if (format_support_check_(fuchsia_images2::wire::PixelFormat::kR8G8B8A8)) {
      for (const auto format_modifier :
           {fuchsia_sysmem::wire::kFormatModifierLinear,
            fuchsia_sysmem::wire::kFormatModifierArmLinearTe,
            fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuv,
            fuchsia_sysmem::wire::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTe}) {
        const size_t index = constraints.image_format_constraints_count++;
        auto& image_constraints = constraints.image_format_constraints[index];
        SetDefaultImageFormatConstraints(fuchsia_sysmem::wire::PixelFormatType::kR8G8B8A8,
                                         format_modifier, image_constraints);
      }
    }
    buffer_name = "Display";
  }

  // Set priority to 10 to override the Vulkan driver name priority of 5, but be less than most
  // application priorities.
  constexpr uint32_t kNamePriority = 10;
  fidl::OneWayStatus name_res =
      collection->SetName(kNamePriority, fidl::StringView::FromExternal(buffer_name));
  if (!name_res.ok()) {
    DISP_ERROR("Failed to set name: %d", name_res.status());
    return name_res.status();
  }
  fidl::OneWayStatus res = collection->SetConstraints(true, constraints);
  if (!res.ok()) {
    DISP_ERROR("Failed to set constraints: %d", res.status());
    return res.status();
  }

  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplSetDisplayPower(uint64_t display_id,
                                                                 bool power_on) {
  fbl::AutoLock lock(&display_lock_);
  if (display::ToDisplayId(display_id) != display_id_ || !display_attached_) {
    return ZX_ERR_NOT_FOUND;
  }
  if (power_on) {
    return vout_->PowerOn().status_value();
  }
  return vout_->PowerOff().status_value();
}

zx_status_t AmlogicDisplay::DisplayControllerImplSetDisplayCaptureInterface(
    const display_capture_interface_protocol_t* intf) {
  fbl::AutoLock lock(&capture_lock_);
  capture_intf_ = ddk::DisplayCaptureInterfaceProtocolClient(intf);
  current_capture_target_image_ = INVALID_ID;
  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplImportImageForCapture(
    uint64_t banjo_driver_buffer_collection_id, uint32_t index, uint64_t* out_capture_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Cannot import capture image on collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& collection =
      buffer_collections_.at(driver_buffer_collection_id);
  auto import_capture = std::make_unique<ImageInfo>();
  if (import_capture == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }
  fbl::AutoLock lock(&capture_lock_);
  fidl::WireResult check_result = collection->CheckBuffersAllocated();
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    return check_result.status();
  }
  const auto& check_response = check_result.value();
  if (check_response.status == ZX_ERR_UNAVAILABLE) {
    return ZX_ERR_SHOULD_WAIT;
  }
  if (check_response.status != ZX_OK) {
    return check_response.status;
  }

  fidl::WireResult wait_result = collection->WaitForBuffersAllocated();
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    return wait_result.status();
  }
  auto& wait_response = wait_result.value();
  if (wait_response.status != ZX_OK) {
    return wait_response.status;
  }
  fuchsia_sysmem::wire::BufferCollectionInfo2& collection_info =
      wait_response.buffer_collection_info;

  if (!collection_info.settings.has_image_format_constraints ||
      index >= collection_info.buffer_count) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // Ensure the proper format
  ZX_DEBUG_ASSERT(collection_info.settings.image_format_constraints.pixel_format.type ==
                  fuchsia_sysmem::wire::PixelFormatType::kBgr24);

  // Allocate a canvas for the capture image
  fuchsia_hardware_amlogiccanvas::wire::CanvasInfo canvas_info;
  canvas_info.height = collection_info.settings.image_format_constraints.min_coded_height;
  canvas_info.stride_bytes = collection_info.settings.image_format_constraints.min_bytes_per_row;
  canvas_info.blkmode = fuchsia_hardware_amlogiccanvas::CanvasBlockMode::kLinear;
  canvas_info.endianness =
      fuchsia_hardware_amlogiccanvas::CanvasEndianness(kCanvasLittleEndian64Bit);
  canvas_info.flags = fuchsia_hardware_amlogiccanvas::CanvasFlags::kRead |
                      fuchsia_hardware_amlogiccanvas::CanvasFlags::kWrite;

  fidl::WireResult result =
      canvas_->Config(std::move(collection_info.buffers[index].vmo),
                      collection_info.buffers[index].vmo_usable_start, canvas_info);
  if (!result.ok()) {
    DISP_ERROR("Could not configure canvas: %s\n", result.error().FormatDescription().c_str());
    return ZX_ERR_NO_RESOURCES;
  }
  if (result->is_error()) {
    DISP_ERROR("Could not configure canvas: %s\n", zx_status_get_string(result->error_value()));
    return ZX_ERR_NO_RESOURCES;
  }

  // At this point, we have setup a canvas with the BufferCollection-based VMO. Store the
  // capture information
  import_capture->canvas_idx = result->value()->canvas_idx;
  import_capture->canvas = canvas_.client_end();
  import_capture->image_height = collection_info.settings.image_format_constraints.min_coded_height;
  import_capture->image_width = collection_info.settings.image_format_constraints.min_coded_width;
  // TODO(fxbug.dev/128653): Using pointers as handles impedes portability of
  // the driver. Do not use pointers as handles.
  *out_capture_handle = reinterpret_cast<uint64_t>(import_capture.get());
  imported_captures_.push_back(std::move(import_capture));
  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplStartCapture(uint64_t capture_handle) {
  if (!fully_initialized()) {
    DISP_ERROR("Cannot start capture before initializing the display\n");
    return ZX_ERR_SHOULD_WAIT;
  }

  fbl::AutoLock lock(&capture_lock_);
  if (current_capture_target_image_ != INVALID_ID) {
    DISP_ERROR("Cannot start capture while another capture is in progress\n");
    return ZX_ERR_SHOULD_WAIT;
  }

  // Confirm that the handle was previously imported (hence valid)
  auto info = reinterpret_cast<ImageInfo*>(capture_handle);
  if (imported_captures_.find_if([info](auto& i) { return i.canvas_idx == info->canvas_idx; }) ==
      imported_captures_.end()) {
    // invalid handle
    DISP_ERROR("Invalid capture_handle\n");
    return ZX_ERR_NOT_FOUND;
  }

  ZX_DEBUG_ASSERT(info->canvas_idx > 0);
  ZX_DEBUG_ASSERT(info->image_height > 0);
  ZX_DEBUG_ASSERT(info->image_width > 0);

  auto status = vpu_->CaptureInit(info->canvas_idx, info->image_height, info->image_width);
  if (status != ZX_OK) {
    DISP_ERROR("Failed to init capture %d\n", status);
    return status;
  }

  status = vpu_->CaptureStart();
  if (status != ZX_OK) {
    DISP_ERROR("Failed to start capture %d\n", status);
    return status;
  }
  current_capture_target_image_ = info;
  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
  fbl::AutoLock lock(&capture_lock_);
  if (capture_handle == reinterpret_cast<uint64_t>(current_capture_target_image_)) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Find and erase previously imported capture
  auto idx = reinterpret_cast<ImageInfo*>(capture_handle)->canvas_idx;
  if (imported_captures_.erase_if([idx](auto& i) { return i.canvas_idx == idx; }) == nullptr) {
    DISP_ERROR("Tried to release non-existent capture image %d\n", idx);
    return ZX_ERR_NOT_FOUND;
  }

  return ZX_OK;
}

bool AmlogicDisplay::DisplayControllerImplIsCaptureCompleted() {
  fbl::AutoLock lock(&capture_lock_);
  return (current_capture_target_image_ == nullptr);
}

int AmlogicDisplay::CaptureThread() {
  zx_status_t status;
  while (true) {
    zx::time timestamp;
    status = vd1_wr_irq_.wait(&timestamp);
    if (status != ZX_OK) {
      DISP_ERROR("Vd1 Wr interrupt wait failed %d\n", status);
      break;
    }
    if (!fully_initialized()) {
      DISP_ERROR("Capture interrupt fired before the display was initialized\n");
      continue;
    }
    vpu_->CaptureDone();
    fbl::AutoLock lock(&capture_lock_);
    if (capture_intf_.is_valid()) {
      capture_intf_.OnCaptureComplete();
    }
    current_capture_target_image_ = nullptr;
  }
  return status;
}

int AmlogicDisplay::VSyncThread() {
  zx_status_t status;
  while (true) {
    zx::time timestamp;
    status = vsync_irq_.wait(&timestamp);
    if (status != ZX_OK) {
      DISP_ERROR("VSync Interrupt Wait failed\n");
      break;
    }
    display::ConfigStamp current_config_stamp = display::kInvalidConfigStamp;
    if (fully_initialized()) {
      current_config_stamp = osd_->GetLastConfigStampApplied();
    }
    fbl::AutoLock lock(&display_lock_);
    if (dc_intf_.is_valid() && display_attached_) {
      const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(current_config_stamp);
      dc_intf_.OnDisplayVsync(display::ToBanjoDisplayId(display_id_), timestamp.get(),
                              &banjo_config_stamp);
    }
  }

  return status;
}

int AmlogicDisplay::HpdThread() {
  zx_status_t status;
  while (true) {
    status = hpd_irq_.wait(nullptr);
    if (status != ZX_OK) {
      DISP_ERROR("Waiting in Interrupt failed %d\n", status);
      break;
    }
    usleep(500000);
    uint8_t hpd;
    status = hpd_gpio_.Read(&hpd);
    if (status != ZX_OK) {
      DISP_ERROR("gpio_read failed HDMI HPD\n");
      continue;
    }

    fbl::AutoLock lock(&display_lock_);

    bool display_added = false;
    added_display_args_t added_display_args;
    added_display_info_t added_display_info;

    bool display_removed = false;
    display::DisplayId removed_display_id;

    if (hpd && !display_attached_) {
      DISP_INFO("Display is connected\n");

      display_attached_ = true;
      vout_->DisplayConnected();
      vout_->PopulateAddedDisplayArgs(&added_display_args, display_id_,
                                      kSupportedBanjoPixelFormats);
      display_added = true;
      hpd_gpio_.SetPolarity(GPIO_POLARITY_LOW);
    } else if (!hpd && display_attached_) {
      DISP_INFO("Display Disconnected!\n");
      vout_->DisplayDisconnected();

      display_removed = true;
      removed_display_id = display_id_;
      display_id_++;
      display_attached_ = false;

      hpd_gpio_.SetPolarity(GPIO_POLARITY_HIGH);
    }

    if (dc_intf_.is_valid() && (display_removed || display_added)) {
      const size_t added_display_count = display_added ? 1 : 0;
      const uint64_t banjo_removed_display_id = display::ToBanjoDisplayId(removed_display_id);
      const size_t removed_display_count = display_removed ? 1 : 0;

      dc_intf_.OnDisplaysChanged(
          /*added_display_list=*/&added_display_args, added_display_count,
          /*removed_display_list=*/&banjo_removed_display_id, removed_display_count,
          /*out_display_info_list=*/&added_display_info,
          /*display_info_count=*/added_display_count, /*out_display_info_actual=*/nullptr);
      if (display_added) {
        // See if we need to change output color to RGB
        status = vout_->OnDisplaysChanged(added_display_info);
      }
    }
  }
  return status;
}

zx_status_t AmlogicDisplay::SetupHotplugDisplayDetection() {
  if (zx_status_t status = ddk::GpioProtocolClient::CreateFromDevice(
          parent_, "gpio-hdmi-hotplug-detect", &hpd_gpio_);
      status != ZX_OK) {
    DISP_ERROR("Could not obtain GPIO protocol for HDMI hotplug detection: %s\n",
               zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = hpd_gpio_.ConfigIn(GPIO_PULL_DOWN); status != ZX_OK) {
    DISP_ERROR("Hotplug detection GPIO input config failed: %s\n", zx_status_get_string(status));
    return status;
  }

  if (zx_status_t status = hpd_gpio_.GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_HIGH, &hpd_irq_);
      status != ZX_OK) {
    DISP_ERROR("Hotplug detection GPIO: cannot get interrupt: %s\n", zx_status_get_string(status));
    return status;
  }

  auto hotplug_detection_thread = [](void* arg) {
    return static_cast<AmlogicDisplay*>(arg)->HpdThread();
  };
  if (int status =
          thrd_create_with_name(&hpd_thread_, hotplug_detection_thread, this, "hpd_thread");
      status != thrd_success) {
    zx_status_t zx_status = thrd_status_to_zx_status(status);
    DISP_ERROR("Could not create hotplug detection thread: %s\n", zx_status_get_string(zx_status));
    return zx_status;
  }

  return ZX_OK;
}

// TODO(payamm): make sure unbind/release are called if we return error
zx_status_t AmlogicDisplay::Bind() {
  root_node_ = inspector_.GetRoot().CreateChild("amlogic-display");
  fbl::AllocChecker ac;
  vout_ = fbl::make_unique_checked<Vout>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  SetFormatSupportCheck(
      [](fuchsia_images2::wire::PixelFormat format) { return IsFormatSupported(format); });

  {
    display_panel_t display_info;
    size_t actual;
    switch (zx_status_t status = device_get_metadata(parent_, DEVICE_METADATA_DISPLAY_CONFIG,
                                                     &display_info, sizeof(display_info), &actual);
            status) {
      case ZX_ERR_NOT_FOUND: {
        auto hdmi_client =
            DdkConnectFragmentFidlProtocol<fuchsia_hardware_hdmi::Service::Device>("hdmi");
        if (hdmi_client.is_error()) {
          zxlogf(ERROR, "Failed to connect to hdmi FIDL protocol: %s", hdmi_client.status_string());
          return hdmi_client.status_value();
        }

        // If configuration is missing, init HDMI.
        if (zx_status_t status = vout_->InitHdmi(parent_, std::move(*hdmi_client));
            status != ZX_OK) {
          DISP_ERROR("Could not initialize HDMI Vout device! %s\n", zx_status_get_string(status));
          return status;
        }
        break;
      }
      case ZX_OK:
        if (actual != sizeof(display_info)) {
          DISP_ERROR("Could not get display panel metadata %zu/%zu\n", actual,
                     sizeof(display_info));
          return ZX_ERR_INTERNAL;
        }
        DISP_INFO("Provided Display Info: %d x %d with panel type %d\n", display_info.width,
                  display_info.height, display_info.panel_type);
        {
          fbl::AutoLock lock(&display_lock_);
          if (zx_status_t status = vout_->InitDsi(parent_, display_info.panel_type,
                                                  display_info.width, display_info.height);
              status != ZX_OK) {
            DISP_ERROR("Could not initialize DSI Vout device! %d\n", status);
            return status;
          }
          display_attached_ = true;
        }

        root_node_.CreateUint("input_panel_type", display_info.panel_type, &inspector_);
        break;
      default:
        DISP_ERROR("Could not get display panel metadata %d\n", status);
        return status;
    }
  }
  root_node_.CreateUint("panel_type", vout_->panel_type(), &inspector_);
  root_node_.CreateUint("vout_type", vout_->type(), &inspector_);

  if (zx_status_t status = ddk::PDevFidl::FromFragment(parent_, &pdev_); status != ZX_OK) {
    DISP_ERROR("Could not get PDEV protocol %d\n", status);
    return status;
  }

  // Get board info
  if (zx_status_t status = pdev_.GetBoardInfo(&board_info_); status != ZX_OK) {
    DISP_ERROR("Could not obtain board info\n");
    return status;
  }

  // Get device info
  if (zx_status_t status = pdev_.GetDeviceInfo(&device_info_); status != ZX_OK) {
    DISP_ERROR("Could not obtain device_info_\n");
    return status;
  }

  zx::result sysmem_client =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::Sysmem>(
          parent_, "sysmem-fidl");
  if (sysmem_client.is_error()) {
    zxlogf(ERROR, "Failed to get sysmem protocol: %s", sysmem_client.status_string());
    return sysmem_client.status_value();
  }
  sysmem_.Bind(std::move(*sysmem_client));

  zx::result result =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_amlogiccanvas::Service::Device>(parent_,
                                                                                      "canvas");
  if (result.is_error()) {
    DISP_ERROR("Could not obtain CANVAS protocol %s\n", result.status_string());
    return result.status_value();
  }

  canvas_.Bind(std::move(result.value()));

  if (zx_status_t status = pdev_.GetBti(0, &bti_); status != ZX_OK) {
    DISP_ERROR("Could not get BTI handle %d\n", status);
    return status;
  }

  // Map VSync Interrupt
  if (zx_status_t status = pdev_.GetInterrupt(IRQ_VSYNC, 0, &vsync_irq_); status != ZX_OK) {
    DISP_ERROR("Could not map vsync interrupt %d\n", status);
    return status;
  }

  auto start_thread = [](void* arg) { return static_cast<AmlogicDisplay*>(arg)->VSyncThread(); };
  if (int status = thrd_create_with_name(&vsync_thread_, start_thread, this, "vsync_thread");
      status != 0) {
    DISP_ERROR("Could not create vsync_thread %d\n", status);
    return status;
  }

  // Map VD1_WR Interrupt (used for capture)
  if (zx_status_t status = pdev_.GetInterrupt(IRQ_VD1_WR, 0, &vd1_wr_irq_); status != ZX_OK) {
    DISP_ERROR("Could not map vd1 wr interrupt %d\n", status);
    return status;
  }

  auto vd_thread = [](void* arg) { return static_cast<AmlogicDisplay*>(arg)->CaptureThread(); };
  if (int status = thrd_create_with_name(&capture_thread_, vd_thread, this, "capture_thread");
      status != 0) {
    DISP_ERROR("Could not create capture_thread %d\n", status);
    return status;
  }

  if (vout_->supports_hpd()) {
    if (zx_status_t status = SetupHotplugDisplayDetection(); status != ZX_OK) {
      DISP_ERROR("Cannot set up hotplug display: %s\n", zx_status_get_string(status));
      return status;
    }
  }

  // Set scheduler role for vsync thread.
  {
    const char* role_name = "fuchsia.graphics.display.drivers.amlogic-display.vsync";
    zx_status_t status = device_set_profile_by_role(parent(), thrd_get_zx_handle(vsync_thread_),
                                                    role_name, strlen(role_name));
    if (status != ZX_OK) {
      DISP_ERROR("Failed to apply role: %d\n", status);
    }
  }

  // Set sysmem allocator client.
  {
    auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
    if (!endpoints.is_ok()) {
      zxlogf(ERROR, "Cannot create sysmem allocator endpoints: %s", endpoints.status_string());
      return endpoints.status_value();
    }
    auto& [client, server] = endpoints.value();
    auto status = sysmem_->ConnectServer(std::move(endpoints->server));
    if (!status.ok()) {
      zxlogf(ERROR, "Cannot connect to sysmem Allocator protocol: %s", status.status_string());
      return status.status();
    }
    sysmem_allocator_client_ = fidl::WireSyncClient(std::move(client));

    std::string debug_name =
        fxl::StringPrintf("amlogic-display[%lu]", fsl::GetCurrentProcessKoid());
    auto set_debug_status = sysmem_allocator_client_->SetDebugClientInfo(
        fidl::StringView::FromExternal(debug_name), fsl::GetCurrentProcessKoid());
    if (!set_debug_status.ok()) {
      zxlogf(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
      return set_debug_status.status();
    }
  }

  auto cleanup = fit::defer([&]() { DdkRelease(); });

  if (zx_status_t status = DdkAdd(ddk::DeviceAddArgs("amlogic-display")
                                      .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                                      .set_inspect_vmo(inspector_.DuplicateVmo()));
      status != ZX_OK) {
    DISP_ERROR("Could not add device %d\n", status);
    return status;
  }

  cleanup.cancel();

  return ZX_OK;
}

AmlogicDisplay::AmlogicDisplay(zx_device_t* parent) : DeviceType(parent) {}
AmlogicDisplay::~AmlogicDisplay() = default;

// static
zx_status_t AmlogicDisplay::Create(zx_device_t* parent) {
  fbl::AllocChecker alloc_checker;
  auto dev = fbl::make_unique_checked<AmlogicDisplay>(&alloc_checker, parent);
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  const zx_status_t status = dev->Bind();
  if (status == ZX_OK) {
    // devmgr now owns the memory for `dev`
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return AmlogicDisplay::Create(parent); },
};

}  // namespace

}  // namespace amlogic_display

// clang-format off
ZIRCON_DRIVER(amlogic_display, amlogic_display::kDriverOps, "zircon", "0.1");
