// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/amlogic-display.h"

#include <fidl/fuchsia.hardware.amlogiccanvas/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
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
#include <zircon/status.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <cstddef>

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

ColorSpaceConversionMode GetColorSpaceConversionMode(VoutType vout_type) {
  switch (vout_type) {
    case VoutType::kDsi:
      return ColorSpaceConversionMode::kRgbInternalRgbOut;
    case VoutType::kHdmi:
      return ColorSpaceConversionMode::kRgbInternalYuvOut;
    default:
      ZX_ASSERT_MSG(false, "Invalid VoutType: %d", static_cast<int>(vout_type));
  }
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
  const ColorSpaceConversionMode color_conversion_mode = GetColorSpaceConversionMode(vout_->type());
  vpu_->SetupPostProcessorColorConversion(color_conversion_mode);
  // Need to call this function since VPU/VPP registers were reset
  vpu_->CheckAndClaimHardwareOwnership();

  return vout_->RestartDisplay();
}

zx_status_t AmlogicDisplay::DisplayInit() {
  ZX_ASSERT(!fully_initialized());

  // Determine whether it's first time boot or not
  const bool skip_disp_init = vpu_->CheckAndClaimHardwareOwnership();
  if (skip_disp_init) {
    zxlogf(INFO, "First time driver load. Skip display initialization");
    // Make sure AFBC engine is on. Since bootloader does not use AFBC, it might not have powered
    // on AFBC engine.
    vpu_->AfbcPower(true);
  } else {
    zxlogf(INFO, "Display driver reloaded. Initialize display system");
    RestartDisplay();
  }

  // The "osd" node must be created because these metric paths are load-bearing
  // for some triage workflows.
  osd_node_ = root_node_.CreateChild("osd");
  zx::result<std::unique_ptr<Osd>> osd_create_result =
      Osd::Create(&pdev_, vout_->fb_width(), vout_->fb_height(), vout_->display_width(),
                  vout_->display_height(), &osd_node_);
  if (osd_create_result.is_error()) {
    zxlogf(ERROR, "Failed to create OSD instance: %s", osd_create_result.status_string());
    return osd_create_result.status_value();
  }
  osd_ = std::move(osd_create_result).value();

  osd_->HwInit();
  return ZX_OK;
}

void AmlogicDisplay::DisplayControllerImplSetDisplayControllerInterface(
    const display_controller_interface_protocol_t* intf) {
  fbl::AutoLock lock(&display_mutex_);
  dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient(intf);

  if (display_attached_) {
    added_display_info_t info{.is_standard_srgb_out = false};  // Random default
    added_display_args_t args;
    vout_->PopulateAddedDisplayArgs(&args, display_id_, kSupportedBanjoPixelFormats);
    dc_intf_.OnDisplaysChanged(&args, 1, nullptr, 0, &info, 1, nullptr);
    vout_->OnDisplaysChanged(info);
  }
}

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
    zxlogf(ERROR, "Failed to create sysmem BufferCollection endpoints: %s",
           endpoints.status_string());
    return ZX_ERR_INTERNAL;
  }
  auto& [collection_client_endpoint, collection_server_endpoint] = endpoints.value();

  auto bind_result = sysmem_allocator_client_->BindSharedCollection(
      fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>(std::move(collection_token)),
      std::move(collection_server_endpoint));
  if (!bind_result.ok()) {
    zxlogf(ERROR, "Failed to complete FIDL call BindSharedCollection: %s",
           bind_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Failed to release buffer collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  buffer_collections_.erase(driver_buffer_collection_id);
  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplImportImage(
    image_t* image, uint64_t banjo_driver_buffer_collection_id, uint32_t index) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR, "Failed to import Image on collection %lu: buffer collection doesn't exist",
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
    zxlogf(ERROR, "Failed to import image: pixel format %u not supported",
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
        zxlogf(ERROR, "Failed to pin BTI: %s", zx_status_get_string(status));
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
        zxlogf(ERROR, "Invalid image width %d for collection", image->width);
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
        zxlogf(ERROR, "Failed to configure canvas: %s", result.error().FormatDescription().c_str());
        return ZX_ERR_NO_RESOURCES;
      }
      fidl::WireResultUnwrapType<fuchsia_hardware_amlogiccanvas::Device::Config>& response =
          result.value();
      if (response.is_error()) {
        zxlogf(ERROR, "Failed to configure canvas: %s",
               zx_status_get_string(response.error_value()));
        return ZX_ERR_NO_RESOURCES;
      }

      import_info->canvas = canvas_.client_end();
      import_info->canvas_idx = result->value()->canvas_idx;
      import_info->image_height = image->height;
      import_info->image_width = image->width;
      import_info->is_afbc = false;
    } break;
    default:
      ZX_DEBUG_ASSERT_MSG(false, "Invalid pixel format modifier: %lu", format_modifier);
      return ZX_ERR_INVALID_ARGS;
  }
  // TODO(fxbug.dev/128653): Using pointers as handles impedes portability of
  // the driver. Do not use pointers as handles.
  image->handle = reinterpret_cast<uint64_t>(import_info.get());
  fbl::AutoLock lock(&image_mutex_);
  imported_images_.push_back(std::move(import_info));
  return status;
}

void AmlogicDisplay::DisplayControllerImplReleaseImage(image_t* image) {
  fbl::AutoLock lock(&image_mutex_);
  auto info = reinterpret_cast<ImageInfo*>(image->handle);
  imported_images_.erase(*info);
}

config_check_result_t AmlogicDisplay::DisplayControllerImplCheckConfiguration(
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

  fbl::AutoLock lock(&display_mutex_);

  // no-op, just wait for the client to try a new config
  if (!display_attached_ || display::ToDisplayId(display_configs[0]->display_id) != display_id_) {
    return CONFIG_CHECK_RESULT_OK;
  }

  if (vout_->CheckMode(&display_configs[0]->mode)) {
    return CONFIG_CHECK_RESULT_UNSUPPORTED_MODES;
  }

  bool success = true;

  ZX_DEBUG_ASSERT(client_composition_opcodes_count >= display_configs[0]->layer_count);
  cpp20::span<client_composition_opcode_t> client_composition_opcodes(
      out_client_composition_opcodes_list, display_configs[0]->layer_count);
  std::fill(client_composition_opcodes.begin(), client_composition_opcodes.end(), 0);
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = client_composition_opcodes.size();
  }

  if (display_configs[0]->layer_count > 1) {
    // We only support 1 layer
    success = false;
  }

  // TODO(fxbug.dev/130593): Move color conversion validation code to a common
  // library.
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
    // TODO(fxbug.dev/130594) Instead of using memcmp() to compare the frame
    // with expected frames, we should use the common type in "api-types-cpp"
    // which supports comparison opeartors.
    frame_t frame = {
        .x_pos = 0,
        .y_pos = 0,
        .width = width,
        .height = height,
    };

    if (layer.alpha_mode == ALPHA_PREMULTIPLIED) {
      // we don't support pre-multiplied alpha mode
      client_composition_opcodes[0] |= CLIENT_COMPOSITION_OPCODE_ALPHA;
    }
    success = display_configs[0]->layer_list[0]->type == LAYER_TYPE_PRIMARY &&
              layer.transform_mode == FRAME_TRANSFORM_IDENTITY && layer.image.width == width &&
              layer.image.height == height &&
              memcmp(&layer.dest_frame, &frame, sizeof(frame_t)) == 0 &&
              memcmp(&layer.src_frame, &frame, sizeof(frame_t)) == 0;
  }
  if (!success) {
    client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
    for (unsigned i = 1; i < display_configs[0]->layer_count; i++) {
      client_composition_opcodes[i] = CLIENT_COMPOSITION_OPCODE_MERGE_SRC;
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

void AmlogicDisplay::DisplayControllerImplApplyConfiguration(
    const display_config_t** display_configs, size_t display_count,
    const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(display_configs);
  ZX_DEBUG_ASSERT(banjo_config_stamp);
  const display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);

  fbl::AutoLock lock(&display_mutex_);

  if (display_count == 1 && display_configs[0]->layer_count) {
    // Setting up OSD may require Vout framebuffer information, which may be
    // changed on each ApplyConfiguration(), so we need to apply the
    // configuration to Vout first before initializing the display and OSD.
    if (zx_status_t status = vout_->ApplyConfiguration(&display_configs[0]->mode);
        status != ZX_OK) {
      zxlogf(ERROR, "Failed to apply config to Vout: %s", zx_status_get_string(status));
      return;
    }

    if (!fully_initialized()) {
      if (zx_status_t status = DisplayInit(); status != ZX_OK) {
        zxlogf(ERROR, "Display Hardware Initialization failed: %s", zx_status_get_string(status));
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
        fbl::AutoLock lock2(&capture_mutex_);
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

  fbl::AutoLock l(&image_mutex_);
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
  if (vsync_irq_.is_valid()) {
    zx_status_t status = vsync_irq_.destroy();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Vsync IRQ destroy failed: %s", zx_status_get_string(status));
    }
  }
  if (vsync_thread_) {
    int status = thrd_join(*vsync_thread_, nullptr);
    if (status != thrd_success) {
      zxlogf(ERROR, "Vsync thread join failed: %s",
             zx_status_get_string(thrd_status_to_zx_status(status)));
    }
  }

  // TODO(fxbug.dev/132066): Power off should occur after all threads are
  // destroyed. Otherwise other threads may still write to the VPU MMIO which
  // can cause the system to hang.
  if (fully_initialized()) {
    osd_->Release();
    vpu_->PowerOff();
  }

  if (capture_finished_irq_.is_valid()) {
    zx_status_t status = capture_finished_irq_.destroy();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Capture finished IRQ destroy failed: %s", zx_status_get_string(status));
    }
  }
  if (capture_thread_.has_value()) {
    int status = thrd_join(*capture_thread_, nullptr);
    if (status != thrd_success) {
      zxlogf(ERROR, "Capture thread join failed: %s",
             zx_status_get_string(thrd_status_to_zx_status(status)));
    }
  }

  if (hpd_irq_.is_valid()) {
    zx_status_t status = hpd_irq_.destroy();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Hot-plug IRQ destroy failed: %s", zx_status_get_string(status));
    }
  }
  if (hpd_thread_.has_value()) {
    int status = thrd_join(*hpd_thread_, nullptr);
    if (status != thrd_success) {
      zxlogf(ERROR, "Hot-plug thread join failed: %s",
             zx_status_get_string(thrd_status_to_zx_status(status)));
    }
  }
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
  fidl::OneWayStatus status =
      sysmem_->ConnectServer(fidl::ServerEnd<fuchsia_sysmem::Allocator>(std::move(connection)));
  if (!status.ok()) {
    zxlogf(ERROR, "Failed to connect to sysmem: %s", status.status_string());
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
           "Failed to set buffer collection constraints for %lu: buffer collection doesn't exist",
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
  fidl::OneWayStatus set_name_result =
      collection->SetName(kNamePriority, fidl::StringView::FromExternal(buffer_name));
  if (!set_name_result.ok()) {
    zxlogf(ERROR, "Failed to set name: %d", set_name_result.status());
    return set_name_result.status();
  }
  fidl::OneWayStatus set_constraints_result = collection->SetConstraints(true, constraints);
  if (!set_constraints_result.ok()) {
    zxlogf(ERROR, "Failed to set constraints: %d", set_constraints_result.status());
    return set_constraints_result.status();
  }

  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplSetDisplayPower(uint64_t display_id,
                                                                 bool power_on) {
  fbl::AutoLock lock(&display_mutex_);
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
  fbl::AutoLock lock(&capture_mutex_);
  capture_intf_ = ddk::DisplayCaptureInterfaceProtocolClient(intf);
  current_capture_target_image_ = INVALID_ID;
  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplImportImageForCapture(
    uint64_t banjo_driver_buffer_collection_id, uint32_t index, uint64_t* out_capture_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    zxlogf(ERROR,
           "Failed to import capture image on collection %lu: buffer collection doesn't exist",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  const fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>& collection =
      buffer_collections_.at(driver_buffer_collection_id);
  auto import_capture = std::make_unique<ImageInfo>();
  if (import_capture == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }
  fbl::AutoLock lock(&capture_mutex_);
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

  // Canvas images are by default little-endian for each 128-bit (16-byte)
  // chunk. By default, for 8-bit YUV444 images, the pixels are interpreted as
  //   Y0  U0  V0  Y1  U1  V1  Y2  U2    V2  Y3  U3  V3  Y4  U4  V4  Y5...
  //
  // However, capture memory interface uses big-endian for each 128-bit
  // (16-byte) chunk (defined in Vpu::CaptureInit), and the high- and low-64
  // bits (8 bytes) are already swapped. This is effectively big-endian for
  // each 64-bit chunk. So, the 8-bit YUV444 pixels are stored by the capture
  // memory interface as
  //   U2  Y2  V1  U1  Y1  V0  U0  Y0    Y5  V4  U4  Y4  V3  U3  Y3  V2...
  //
  // In order to read / write the captured canvas image correctly, the canvas
  // endianness must match that of capture memory interface.
  //
  // To convert 128-bit little-endian to 64-bit big-endian, we need to swap
  // every 8-bit pairs, 16-bit pairs and 32-bit pairs within every 64-bit chunk:
  //
  //   The original bytes written by the capture memory interface:
  //     U2  Y2  V1  U1  Y1  V0  U0  Y0    Y5  V4  U4  Y4  V3  U3  Y3  V2...
  //   Swapping every 8-bit pairs we get:
  //     Y2  U2  U1  V1  V0  Y1  Y0  U0    V4  Y5  Y4  U4  U3  V3  V2  Y3...
  //   Then we swap every 16-bit pairs:
  //     U1  V1  Y2  U2  Y0  U0  V0  Y1    Y4  U4  V4  Y5  V2  Y3  U3  V3...
  //   Then we swap every 32-bit pairs:
  //     Y0  U0  V0  Y1  U1  V1  Y2  U2    V2  Y3  U3  V3  Y4  U4  V4  Y5...
  //   Then we got the correct pixel interpretation.
  constexpr fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness kCanvasBigEndian64Bit =
      fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness::kSwap8BitPairs |
      fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness::kSwap16BitPairs |
      fuchsia_hardware_amlogiccanvas::wire::CanvasEndianness::kSwap32BitPairs;

  canvas_info.endianness = fuchsia_hardware_amlogiccanvas::CanvasEndianness(kCanvasBigEndian64Bit);
  canvas_info.flags = fuchsia_hardware_amlogiccanvas::CanvasFlags::kRead |
                      fuchsia_hardware_amlogiccanvas::CanvasFlags::kWrite;

  fidl::WireResult result =
      canvas_->Config(std::move(collection_info.buffers[index].vmo),
                      collection_info.buffers[index].vmo_usable_start, canvas_info);
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to configure canvas: %s", result.error().FormatDescription().c_str());
    return ZX_ERR_NO_RESOURCES;
  }
  fidl::WireResultUnwrapType<fuchsia_hardware_amlogiccanvas::Device::Config>& response =
      result.value();
  if (response.is_error()) {
    zxlogf(ERROR, "Failed to configure canvas: %s", zx_status_get_string(response.error_value()));
    return ZX_ERR_NO_RESOURCES;
  }

  // At this point, we have setup a canvas with the BufferCollection-based VMO. Store the
  // capture information
  //
  // TODO(fxbug.dev/132064): Currently there's no guarantee in the canvas API
  // for the uniqueness of `canvas_idx`, and this driver doesn't check if there
  // is any image with the same canvas index either. We should either make this
  // a formal guarantee in Canvas.Config() API, or perform a check against all
  // imported images to make sure the canvas is unique so that the driver won't
  // overwrite other images.
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
    zxlogf(ERROR, "Failed to start capture before initializing the display");
    return ZX_ERR_SHOULD_WAIT;
  }

  fbl::AutoLock lock(&capture_mutex_);
  if (current_capture_target_image_ != INVALID_ID) {
    zxlogf(ERROR, "Failed to start capture while another capture is in progress");
    return ZX_ERR_SHOULD_WAIT;
  }

  // Confirm that the handle was previously imported (hence valid)
  // TODO(fxbug.dev/128653): This requires an enumeration over all the imported
  // capture images for each StartCapture(). We should use hash maps to map
  // handles (which shouldn't be pointers) to ImageInfo instead.
  ImageInfo* info = reinterpret_cast<ImageInfo*>(capture_handle);
  uint8_t canvas_index = info->canvas_idx;
  if (imported_captures_.find_if([canvas_index](const ImageInfo& info) {
        return info.canvas_idx == canvas_index;
      }) == imported_captures_.end()) {
    // invalid handle
    zxlogf(ERROR, "Invalid capture_handle: %" PRIu64, capture_handle);
    return ZX_ERR_NOT_FOUND;
  }

  // TODO(fxbug.dev/132064): A valid canvas index can be zero.
  ZX_DEBUG_ASSERT(info->canvas_idx > 0);
  ZX_DEBUG_ASSERT(info->image_height > 0);
  ZX_DEBUG_ASSERT(info->image_width > 0);

  auto status = vpu_->CaptureInit(info->canvas_idx, info->image_height, info->image_width);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to init capture: %s", zx_status_get_string(status));
    return status;
  }

  status = vpu_->CaptureStart();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start capture: %s", zx_status_get_string(status));
    return status;
  }
  current_capture_target_image_ = info;
  return ZX_OK;
}

zx_status_t AmlogicDisplay::DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
  fbl::AutoLock lock(&capture_mutex_);
  if (capture_handle == reinterpret_cast<uint64_t>(current_capture_target_image_)) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Find and erase previously imported capture
  // TODO(fxbug.dev/128653): This requires an enumeration over all the imported
  // capture images for each StartCapture(). We should use hash maps to map
  // handles (which shouldn't be pointers) to ImageInfo instead.
  uint8_t canvas_index = reinterpret_cast<ImageInfo*>(capture_handle)->canvas_idx;
  if (imported_captures_.erase_if(
          [canvas_index](const ImageInfo& i) { return i.canvas_idx == canvas_index; }) == nullptr) {
    zxlogf(ERROR, "Tried to release non-existent capture image %d", canvas_index);
    return ZX_ERR_NOT_FOUND;
  }

  return ZX_OK;
}

bool AmlogicDisplay::DisplayControllerImplIsCaptureCompleted() {
  fbl::AutoLock lock(&capture_mutex_);
  return (current_capture_target_image_ == nullptr);
}

void AmlogicDisplay::CaptureThreadEntryPoint() {
  while (true) {
    zx::time timestamp;
    zx_status_t status = capture_finished_irq_.wait(&timestamp);
    if (status == ZX_ERR_CANCELED) {
      zxlogf(INFO,
             "Capture finished (VD1_WR) interrupt wait is cancelled. "
             "Stopping capture thread.");
      break;
    }
    if (status != ZX_OK) {
      zxlogf(ERROR, "Capture finished (VD1_WR) interrupt wait failed: %s",
             zx_status_get_string(status));
      break;
    }
    if (!fully_initialized()) {
      zxlogf(WARNING, "Capture interrupt fired before the display was initialized");
      continue;
    }
    vpu_->CaptureDone();
    fbl::AutoLock lock(&capture_mutex_);
    if (capture_intf_.is_valid()) {
      capture_intf_.OnCaptureComplete();
    }
    current_capture_target_image_ = nullptr;
  }
}

void AmlogicDisplay::VSyncThreadEntryPoint() {
  while (true) {
    zx::time timestamp;
    zx_status_t status = vsync_irq_.wait(&timestamp);
    if (status == ZX_ERR_CANCELED) {
      zxlogf(INFO, "Vsync interrupt wait is cancelled. Stopping Vsync thread.");
      break;
    }
    if (status != ZX_OK) {
      zxlogf(ERROR, "VSync interrupt wait failed: %s", zx_status_get_string(status));
      break;
    }
    display::ConfigStamp current_config_stamp = display::kInvalidConfigStamp;
    if (fully_initialized()) {
      current_config_stamp = osd_->GetLastConfigStampApplied();
    }
    fbl::AutoLock lock(&display_mutex_);
    if (dc_intf_.is_valid() && display_attached_) {
      const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(current_config_stamp);
      dc_intf_.OnDisplayVsync(display::ToBanjoDisplayId(display_id_), timestamp.get(),
                              &banjo_config_stamp);
    }
  }
}

void AmlogicDisplay::HpdThreadEntryPoint() {
  while (true) {
    zx_status_t status = hpd_irq_.wait(nullptr);
    if (status == ZX_ERR_CANCELED) {
      zxlogf(INFO, "Hotplug interrupt wait is cancelled. Stopping hotplug thread.");
      break;
    }
    if (status != ZX_OK) {
      zxlogf(ERROR, "Hotplug interrupt wait failed: %s", zx_status_get_string(status));
      break;
    }
    usleep(500000);
    fidl::WireResult<fuchsia_hardware_gpio::Gpio::Read> hpd_result = hpd_gpio_->Read();
    if (!hpd_result.ok()) {
      zxlogf(ERROR, "Failed to send Read request to hpd gpio: %s", hpd_result.status_string());
      continue;
    }
    fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::Read>& hpd_response =
        hpd_result.value();
    if (hpd_response.is_error()) {
      zxlogf(ERROR, "Failed to read hpd gpio: %s",
             zx_status_get_string(hpd_response.error_value()));
      continue;
    }
    uint8_t has_hotplug_display = hpd_response.value()->value;

    fbl::AutoLock lock(&display_mutex_);

    bool display_added = false;
    added_display_args_t added_display_args;
    added_display_info_t added_display_info;

    bool display_removed = false;
    display::DisplayId removed_display_id;

    if (has_hotplug_display && !display_attached_) {
      zxlogf(INFO, "Display is connected");

      display_attached_ = true;
      vout_->DisplayConnected();
      vout_->PopulateAddedDisplayArgs(&added_display_args, display_id_,
                                      kSupportedBanjoPixelFormats);
      display_added = true;
      fidl::WireResult result = hpd_gpio_->SetPolarity(fuchsia_hardware_gpio::GpioPolarity::kLow);
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send SetPolarity request to hpd gpio: %s", result.status_string());
        break;
      }
      fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::SetPolarity>& response =
          result.value();
      if (response.is_error()) {
        zxlogf(ERROR, "Failed to set polarity of hpd gpio: %s",
               zx_status_get_string(response.error_value()));
        break;
      }
    } else if (!has_hotplug_display && display_attached_) {
      zxlogf(INFO, "Display Disconnected!");
      vout_->DisplayDisconnected();

      display_removed = true;
      removed_display_id = display_id_;
      display_id_++;
      display_attached_ = false;

      fidl::WireResult result = hpd_gpio_->SetPolarity(fuchsia_hardware_gpio::GpioPolarity::kHigh);
      if (!result.ok()) {
        zxlogf(ERROR, "Failed to send SetPolarity request to hpd gpio: %s", result.status_string());
        break;
      }
      fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::SetPolarity>& response =
          result.value();
      if (response.is_error()) {
        zxlogf(ERROR, "Failed to set polarity of hpd gpio: %s",
               zx_status_get_string(response.error_value()));
        break;
      }
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
        zx_status_t status = vout_->OnDisplaysChanged(added_display_info);
        if (status != ZX_OK) {
          zxlogf(ERROR, "Failed to change Vout display configuration: %s",
                 zx_status_get_string(status));
          break;
        }
      }
    }
  }
}

zx_status_t AmlogicDisplay::SetupHotplugDisplayDetection() {
  ZX_ASSERT(!hpd_gpio_.is_valid());
  ZX_ASSERT(!hpd_irq_.is_valid());
  ZX_ASSERT(!hpd_thread_.has_value());

  const char* kHpdGpioFragmentName = "gpio-hdmi-hotplug-detect";
  zx::result<fidl::ClientEnd<fuchsia_hardware_gpio::Gpio>> hpd_gpio =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(parent_,
                                                                             kHpdGpioFragmentName);
  if (hpd_gpio.is_error()) {
    zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", kHpdGpioFragmentName,
           hpd_gpio.status_string());
    return hpd_gpio.status_value();
  }
  hpd_gpio_.Bind(std::move(hpd_gpio.value()));

  fidl::WireResult config_in_result =
      hpd_gpio_->ConfigIn(fuchsia_hardware_gpio::GpioFlags::kPullDown);
  if (!config_in_result.ok()) {
    zxlogf(ERROR, "Failed to send ConfigIn request to hpd gpio: %s",
           config_in_result.status_string());
    return config_in_result.status();
  }
  fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::ConfigIn>& config_in_response =
      config_in_result.value();
  if (config_in_response.is_error()) {
    zxlogf(ERROR, "Failed to configure hpd gpio to input: %s",
           zx_status_get_string(config_in_response.error_value()));
    return config_in_response.error_value();
  }

  fidl::WireResult interrupt_result = hpd_gpio_->GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_HIGH);
  if (!interrupt_result.ok()) {
    zxlogf(ERROR, "Failed to send GetInterrupt request to hpd gpio: %s",
           interrupt_result.status_string());
    return interrupt_result.status();
  }
  fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::GetInterrupt>& interrupt_response =
      interrupt_result.value();
  if (interrupt_response.is_error()) {
    zxlogf(ERROR, "Failed to get interrupt from hpd gpio: %s",
           zx_status_get_string(interrupt_response.error_value()));
    return interrupt_response.error_value();
  }
  hpd_irq_ = std::move(interrupt_result.value()->irq);

  thrd_t hpd_thread;
  int status = thrd_create_with_name(
      &hpd_thread,
      [](void* arg) {
        reinterpret_cast<AmlogicDisplay*>(arg)->HpdThreadEntryPoint();
        return 0;
      },
      /*arg=*/this,
      /*name=*/"hpd_thread");
  if (status != thrd_success) {
    zx_status_t zx_status = thrd_status_to_zx_status(status);
    zxlogf(ERROR, "Failed to create hotplug detection thread: %s", zx_status_get_string(zx_status));
    return zx_status;
  }
  hpd_thread_.emplace(hpd_thread);

  return ZX_OK;
}

zx_status_t AmlogicDisplay::InitializeHdmiVout() {
  zx::result<fidl::ClientEnd<fuchsia_hardware_hdmi::Hdmi>> hdmi_client_result =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_hdmi::Service::Device>("hdmi");
  if (hdmi_client_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to hdmi FIDL protocol: %s",
           hdmi_client_result.status_string());
    return hdmi_client_result.status_value();
  }

  zx_status_t status = vout_->InitHdmi(parent_, std::move(hdmi_client_result.value()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize HDMI Vout device: %s", zx_status_get_string(status));
    return status;
  }

  root_node_.CreateUint("vout_type", vout_->type(), &inspector_);
  return ZX_OK;
}

zx_status_t AmlogicDisplay::InitializeMipiDsiVout(display_panel_t panel_info) {
  zxlogf(INFO, "Provided Display Info: %" PRIu32 " x %" PRIu32 " with panel type %" PRIu32,
         panel_info.width, panel_info.height, panel_info.panel_type);
  {
    fbl::AutoLock lock(&display_mutex_);
    zx_status_t status =
        vout_->InitDsi(parent_, panel_info.panel_type, panel_info.width, panel_info.height);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to initialize DSI Vout device: %s", zx_status_get_string(status));
      return status;
    }
    display_attached_ = true;
  }

  root_node_.CreateUint("vout_type", vout_->type(), &inspector_);
  root_node_.CreateUint("panel_type", vout_->panel_type(), &inspector_);
  root_node_.CreateUint("input_panel_type", panel_info.panel_type, &inspector_);
  return ZX_OK;
}

zx_status_t AmlogicDisplay::InitializeVout() {
  ZX_ASSERT(vout_ != nullptr);

  display_panel_t panel_info;
  size_t actual_bytes;

  // TODO(fxbug.dev/132065): `DEVICE_METADATA_DISPLAY_CONFIG` is defined to
  // store metadata of `display_config_t` type rather than `display_panel_t`
  // type, though currently all the board drivers use display_panel_t instead,
  // which is defined on a side channel apart from the //src/lib/ddk library.
  zx_status_t status = device_get_metadata(parent_, DEVICE_METADATA_DISPLAY_CONFIG, &panel_info,
                                           sizeof(display_panel_t), &actual_bytes);
  if (status == ZX_ERR_NOT_FOUND) {
    return InitializeHdmiVout();
  }

  if (status == ZX_OK) {
    if (actual_bytes != sizeof(display_panel_t)) {
      zxlogf(ERROR, "Display panel metadata size mismatch: Got %zu bytes, expected %zu bytes",
             actual_bytes, sizeof(display_panel_t));
      return ZX_ERR_INTERNAL;
    }
    return InitializeMipiDsiVout(panel_info);
  }

  zxlogf(ERROR, "Failed to get display panel metadata: %s", zx_status_get_string(status));
  return status;
}

zx_status_t AmlogicDisplay::GetCommonProtocolsAndResources() {
  ZX_ASSERT(!pdev_.is_valid());
  ZX_ASSERT(!sysmem_.is_valid());
  ZX_ASSERT(!canvas_.is_valid());
  ZX_ASSERT(!bti_.is_valid());
  ZX_ASSERT(!vsync_irq_.is_valid());
  ZX_ASSERT(!capture_finished_irq_.is_valid());

  zx_status_t status = ddk::PDevFidl::FromFragment(parent_, &pdev_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get PDev protocol: %s", zx_status_get_string(status));
    return status;
  }

  zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>> sysmem_client_result =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::Sysmem>(
          parent_, "sysmem");
  if (sysmem_client_result.is_error()) {
    zxlogf(ERROR, "Failed to get sysmem protocol: %s", sysmem_client_result.status_string());
    return sysmem_client_result.status_value();
  }
  sysmem_.Bind(std::move(sysmem_client_result.value()));

  zx::result<fidl::ClientEnd<fuchsia_hardware_amlogiccanvas::Device>> canvas_client_result =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_amlogiccanvas::Service::Device>(parent_,
                                                                                      "canvas");
  if (canvas_client_result.is_error()) {
    zxlogf(ERROR, "Failed to get Amlogic canvas protocol: %s",
           canvas_client_result.status_string());
    return canvas_client_result.status_value();
  }
  canvas_.Bind(std::move(canvas_client_result.value()));

  status = pdev_.GetBti(0, &bti_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI handle: %s", zx_status_get_string(status));
    return status;
  }

  // Get VSync interrupt (IRQ_VSYNC)
  status = pdev_.GetInterrupt(IRQ_VSYNC, 0, &vsync_irq_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get vsync interrupt: %s", zx_status_get_string(status));
    return status;
  }

  // Get display capture finished interrupt (IRQ_VD1_WR)
  status = pdev_.GetInterrupt(IRQ_VD1_WR, 0, &capture_finished_irq_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get capture finished interrupt: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t AmlogicDisplay::InitializeSysmemAllocator() {
  ZX_ASSERT(sysmem_.is_valid());

  zx::result<fidl::Endpoints<fuchsia_sysmem::Allocator>> endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  if (!endpoints.is_ok()) {
    zxlogf(ERROR, "Failed to create sysmem allocator endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }
  auto& [client, server] = endpoints.value();

  fidl::OneWayStatus status = sysmem_->ConnectServer(std::move(endpoints->server));
  if (!status.ok()) {
    zxlogf(ERROR, "Failed to connect to sysmem Allocator protocol: %s", status.status_string());
    return status.status();
  }
  sysmem_allocator_client_ = fidl::WireSyncClient(std::move(client));

  const zx_koid_t current_process_koid = fsl::GetCurrentProcessKoid();
  const std::string debug_name = fxl::StringPrintf("amlogic-display[%lu]", current_process_koid);
  fidl::OneWayStatus set_debug_status = sysmem_allocator_client_->SetDebugClientInfo(
      fidl::StringView::FromExternal(debug_name), current_process_koid);
  if (!set_debug_status.ok()) {
    zxlogf(ERROR, "Failed to set sysmem allocator debug info: %s",
           set_debug_status.status_string());
    return set_debug_status.status();
  }
  return ZX_OK;
}

zx_status_t AmlogicDisplay::StartVsyncInterruptHandlerThread() {
  ZX_ASSERT(!vsync_thread_.has_value());
  thrd_t vsync_thread;
  int vsync_thread_create_status = thrd_create_with_name(
      &vsync_thread,
      [](void* arg) {
        reinterpret_cast<AmlogicDisplay*>(arg)->VSyncThreadEntryPoint();
        return 0;
      },
      /*arg=*/this, /*name=*/"vsync_thread");
  if (vsync_thread_create_status != thrd_success) {
    zx_status_t status = thrd_status_to_zx_status(vsync_thread_create_status);
    zxlogf(ERROR, "Failed to create Vsync thread: %s", zx_status_get_string(status));
    return status;
  }
  vsync_thread_.emplace(vsync_thread);
  // Set scheduler role for vsync thread.
  const char* kRoleName = "fuchsia.graphics.display.drivers.amlogic-display.vsync";
  zx_status_t status = device_set_profile_by_role(parent(), thrd_get_zx_handle(*vsync_thread_),
                                                  kRoleName, strlen(kRoleName));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to apply role: %s", zx_status_get_string(status));
  }
  return ZX_OK;
}

zx_status_t AmlogicDisplay::StartDisplayCaptureInterruptHandlerThread() {
  ZX_ASSERT(!capture_thread_.has_value());
  thrd_t capture_thread;
  int capture_thread_create_status = thrd_create_with_name(
      &capture_thread,
      [](void* arg) {
        reinterpret_cast<AmlogicDisplay*>(arg)->CaptureThreadEntryPoint();
        return 0;
      },
      this, "capture_thread");
  if (capture_thread_create_status != thrd_success) {
    zx_status_t status = thrd_status_to_zx_status(capture_thread_create_status);
    zxlogf(ERROR, "Failed to create capture_thread: %s",
           zx_status_get_string(capture_thread_create_status));
    return status;
  }
  capture_thread_.emplace(capture_thread);
  return ZX_OK;
}

// TODO(payamm): make sure unbind/release are called if we return error
zx_status_t AmlogicDisplay::Bind() {
  SetFormatSupportCheck(
      [](fuchsia_images2::wire::PixelFormat format) { return IsFormatSupported(format); });

  // Set up inspect first, since other components may add inspect children
  // during initialization.
  root_node_ = inspector_.GetRoot().CreateChild("amlogic-display");

  zx_status_t status = GetCommonProtocolsAndResources();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get common protocols resources from parent devices: %s",
           zx_status_get_string(status));
    return status;
  }

  fbl::AllocChecker ac;
  vout_ = fbl::make_unique_checked<Vout>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  status = InitializeVout();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initalize Vout: %s", zx_status_get_string(status));
    return status;
  }

  vpu_ = fbl::make_unique_checked<Vpu>(&ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  status = vpu_->Init(pdev_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize VPU object: %s", zx_status_get_string(status));
    return status;
  }

  status = InitializeSysmemAllocator();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize sysmem allocator: %s", zx_status_get_string(status));
    return status;
  }

  status = StartVsyncInterruptHandlerThread();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start Vsync interrupt handler threads: %s",
           zx_status_get_string(status));
    return status;
  }

  status = StartDisplayCaptureInterruptHandlerThread();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start Vsync interrupt handler threads: %s",
           zx_status_get_string(status));
    return status;
  }

  if (vout_->supports_hpd()) {
    if (zx_status_t status = SetupHotplugDisplayDetection(); status != ZX_OK) {
      zxlogf(ERROR, "Failed to set up hotplug display: %s", zx_status_get_string(status));
      return status;
    }
  }

  auto cleanup = fit::defer([&]() { DdkRelease(); });

  if (zx_status_t status = DdkAdd(ddk::DeviceAddArgs("amlogic-display")
                                      .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                                      .set_inspect_vmo(inspector_.DuplicateVmo()));
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
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
