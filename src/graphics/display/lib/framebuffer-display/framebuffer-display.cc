// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/framebuffer-display/framebuffer-display.h"

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/device-protocol/pci.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/image-format/image_format.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zbi-format/graphics.h>
#include <unistd.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <atomic>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <utility>

#include <bind/fuchsia/sysmem/heap/cpp/bind.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/rectangle.h"

namespace framebuffer_display {

namespace {

static constexpr display::DisplayId kDisplayId(1);

static constexpr uint64_t kImageHandle = 0xdecafc0ffee;

// Just guess that it's 30fps
static constexpr int kRefreshRateHz = 30;

static constexpr auto kVSyncInterval = zx::usec(1000000 / kRefreshRateHz);

fuchsia_hardware_sysmem::wire::HeapProperties GetHeapProperties(fidl::AnyArena& arena) {
  fuchsia_hardware_sysmem::wire::CoherencyDomainSupport coherency_domain_support =
      fuchsia_hardware_sysmem::wire::CoherencyDomainSupport::Builder(arena)
          .cpu_supported(false)
          .ram_supported(true)
          .inaccessible_supported(false)
          .Build();

  fuchsia_hardware_sysmem::wire::HeapProperties heap_properties =
      fuchsia_hardware_sysmem::wire::HeapProperties::Builder(arena)
          .coherency_domain_support(std::move(coherency_domain_support))
          .need_clear(false)
          .Build();
  return heap_properties;
}

void OnHeapServerClose(fidl::UnbindInfo info, zx::channel channel) {
  if (info.is_dispatcher_shutdown()) {
    // Pending wait is canceled because the display device that the heap belongs
    // to has been destroyed.
    FDF_LOG(INFO, "Framebuffer display destroyed: status: %s", info.status_string());
    return;
  }

  if (info.is_peer_closed()) {
    FDF_LOG(INFO, "Client closed heap connection");
    return;
  }

  FDF_LOG(ERROR, "Channel internal error: status: %s", info.FormatDescription().c_str());
}

zx_koid_t GetCurrentProcessKoid() {
  zx_handle_t handle = zx_process_self();
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

}  // namespace

// implement display controller protocol:

void FramebufferDisplay::DisplayEngineRegisterDisplayEngineListener(
    const display_engine_listener_protocol_t* engine_listener) {
  engine_listener_ = ddk::DisplayEngineListenerProtocolClient(engine_listener);

  const int64_t pixel_clock_hz =
      int64_t{properties_.width_px} * properties_.height_px * kRefreshRateHz;
  ZX_DEBUG_ASSERT(pixel_clock_hz >= 0);
  ZX_DEBUG_ASSERT(pixel_clock_hz <= display::kMaxPixelClockHz);
  const display::DisplayTiming timing = {
      .horizontal_active_px = properties_.width_px,
      .horizontal_front_porch_px = 0,
      .horizontal_sync_width_px = 0,
      .horizontal_back_porch_px = 0,
      .vertical_active_lines = properties_.height_px,
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

  // fuchsia.images2.PixelFormat can always cast to AnyPixelFormat safely.
  fuchsia_images2_pixel_format_enum_value_t pixel_format =
      static_cast<fuchsia_images2_pixel_format_enum_value_t>(properties_.pixel_format);

  const display_mode_t banjo_display_mode = display::ToBanjoDisplayMode(timing);
  const raw_display_info_t banjo_display_info = {
      .display_id = display::ToBanjoDisplayId(kDisplayId),
      .preferred_modes_list = &banjo_display_mode,
      .preferred_modes_count = 1,
      .edid_bytes_list = nullptr,
      .edid_bytes_count = 0,
      .eddc_client = {.ops = nullptr, .ctx = nullptr},
      .pixel_formats_list = &pixel_format,
      .pixel_formats_count = 1,
  };
  engine_listener_.OnDisplayAdded(&banjo_display_info);
}

void FramebufferDisplay::DisplayEngineDeregisterDisplayEngineListener() {
  engine_listener_ = ddk::DisplayEngineListenerProtocolClient();
}

zx_status_t FramebufferDisplay::DisplayEngineImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    FDF_LOG(ERROR, "Buffer Collection (id=%lu) already exists",
            driver_buffer_collection_id.value());
    return ZX_ERR_ALREADY_EXISTS;
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_.is_valid(), "sysmem allocator is not initialized");

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

  fidl::Arena arena;
  fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest bind_request =
      fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena)
          .token(
              fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>(std::move(collection_token)))
          .buffer_collection_request(std::move(collection_server_endpoint))
          .Build();
  auto bind_result = sysmem_->BindSharedCollection(std::move(bind_request));
  if (!bind_result.ok()) {
    FDF_LOG(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
            bind_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));

  return ZX_OK;
}

zx_status_t FramebufferDisplay::DisplayEngineReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    const display::DriverBufferCollectionId driver_buffer_collection_id =
        display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
    FDF_LOG(ERROR, "Cannot release buffer collection %lu: buffer collection doesn't exist",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  buffer_collections_.erase(driver_buffer_collection_id);
  return ZX_OK;
}

zx_status_t FramebufferDisplay::DisplayEngineImportImage(
    const image_metadata_t* banjo_image_metadata, uint64_t banjo_driver_buffer_collection_id,
    uint32_t index, uint64_t* out_image_handle) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  fidl::WireResult check_result = collection->CheckAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    FDF_LOG(ERROR, "failed to check buffers allocated, %s",
            check_result.FormatDescription().c_str());
    return check_result.status();
  }
  const auto& check_response = check_result.value();
  if (check_response.is_error()) {
    if (check_response.error_value() == fuchsia_sysmem2::Error::kPending) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return sysmem::V1CopyFromV2Error(check_response.error_value());
  }

  fidl::WireResult wait_result = collection->WaitForAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    FDF_LOG(ERROR, "failed to wait for buffers allocated, %s",
            wait_result.FormatDescription().c_str());
    return wait_result.status();
  }
  auto& wait_response = wait_result.value();
  if (wait_response.is_error()) {
    return sysmem::V1CopyFromV2Error(wait_response.error_value());
  }
  fuchsia_sysmem2::wire::BufferCollectionInfo& collection_info =
      wait_response->buffer_collection_info();

  if (!collection_info.settings().has_image_format_constraints()) {
    FDF_LOG(ERROR, "no image format constraints");
    return ZX_ERR_INVALID_ARGS;
  }

  if (index > 0) {
    FDF_LOG(ERROR, "invalid index %d, greater than 0", index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  auto sysmem2_collection_format =
      collection_info.settings().image_format_constraints().pixel_format();
  if (sysmem2_collection_format != properties_.pixel_format) {
    FDF_LOG(ERROR,
            "Image format from sysmem (%" PRIu32 ") doesn't match expected format (%" PRIu32 ")",
            static_cast<uint32_t>(sysmem2_collection_format),
            static_cast<uint32_t>(properties_.pixel_format));
    return ZX_ERR_INVALID_ARGS;
  }

  // We only need the VMO temporarily to get the BufferKey. The BufferCollection client_end in
  // buffer_collections_ is not SetWeakOk (and therefore is known to be strong at this point), so
  // it's not necessary to keep this VMO for the buffer to remain alive.
  zx::vmo vmo = std::move(collection_info.buffers()[0].vmo());

  fidl::Arena arena;
  auto vmo_info_result =
      sysmem_->GetVmoInfo(fuchsia_sysmem2::wire::AllocatorGetVmoInfoRequest::Builder(arena)
                              .vmo(std::move(vmo))
                              .Build());
  if (!vmo_info_result.ok()) {
    return vmo_info_result.error().status();
  }
  if (!vmo_info_result->is_ok()) {
    return sysmem::V1CopyFromV2Error(vmo_info_result->error_value());
  }
  auto& vmo_info = vmo_info_result->value();
  BufferKey buffer_key(vmo_info->buffer_collection_id(), vmo_info->buffer_index());

  bool key_matched;
  {
    fbl::AutoLock lock(&framebuffer_key_mtx_);
    key_matched = framebuffer_key_.has_value() && (*framebuffer_key_ == buffer_key);
  }
  if (!key_matched) {
    return ZX_ERR_INVALID_ARGS;
  }

  display::ImageMetadata image_metadata(*banjo_image_metadata);
  if (image_metadata.width() != properties_.width_px ||
      image_metadata.height() != properties_.height_px) {
    return ZX_ERR_INVALID_ARGS;
  }

  *out_image_handle = kImageHandle;
  return ZX_OK;
}

void FramebufferDisplay::DisplayEngineReleaseImage(uint64_t image_handle) {
  // noop
}

config_check_result_t FramebufferDisplay::DisplayEngineCheckConfiguration(
    const display_config_t* display_configs, size_t display_count,
    layer_composition_operations_t* out_layer_composition_operations_list,
    size_t layer_composition_operations_count, size_t* out_layer_composition_operations_actual) {
  if (out_layer_composition_operations_actual != nullptr) {
    *out_layer_composition_operations_actual = 0;
  }

  if (display_count != 1) {
    ZX_DEBUG_ASSERT(display_count == 0);
    return CONFIG_CHECK_RESULT_OK;
  }
  ZX_DEBUG_ASSERT(display::ToDisplayId(display_configs[0].display_id) == kDisplayId);

  ZX_DEBUG_ASSERT(layer_composition_operations_count >= display_configs[0].layer_count);
  cpp20::span<layer_composition_operations_t> layer_composition_operations(
      out_layer_composition_operations_list, display_configs[0].layer_count);
  std::fill(layer_composition_operations.begin(), layer_composition_operations.end(), 0);
  if (out_layer_composition_operations_actual != nullptr) {
    *out_layer_composition_operations_actual = layer_composition_operations.size();
  }

  bool success = IsBanjoDisplayConfigSupported(display_configs[0]);
  if (!success) {
    layer_composition_operations[0] = LAYER_COMPOSITION_OPERATIONS_MERGE_BASE;
    for (unsigned i = 1; i < display_configs[0].layer_count; i++) {
      layer_composition_operations[i] = LAYER_COMPOSITION_OPERATIONS_MERGE_SRC;
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

bool FramebufferDisplay::IsBanjoDisplayConfigSupported(
    const display_config_t& banjo_display_config) {
  if (banjo_display_config.layer_count != 1) {
    return false;
  }

  const layer_t& banjo_layer = banjo_display_config.layer_list[0];
  if (banjo_layer.image_source.width == 0 || banjo_layer.image_source.height == 0) {
    // Solid color fill layers are not supported.
    return false;
  }

  if (banjo_layer.image_source_transformation != COORDINATE_TRANSFORMATION_IDENTITY) {
    return false;
  }

  const display::ImageMetadata image_metadata(banjo_layer.image_metadata);
  if (image_metadata.width() != properties_.width_px) {
    return false;
  }
  if (image_metadata.height() != properties_.height_px) {
    return false;
  }

  const display::Rectangle display_area({
      .x = 0,
      .y = 0,
      .width = properties_.width_px,
      .height = properties_.height_px,
  });
  const auto actual_display_destination = display::Rectangle::From(banjo_layer.display_destination);
  if (actual_display_destination != display_area) {
    return false;
  }

  const auto actual_image_source = display::Rectangle::From(banjo_layer.image_source);
  if (actual_image_source != display_area) {
    return false;
  }

  if (banjo_display_config.cc_flags != 0) {
    return false;
  }

  if (banjo_layer.alpha_mode != ALPHA_DISABLE) {
    return false;
  }

  return true;
}

void FramebufferDisplay::DisplayEngineApplyConfiguration(const display_config_t* display_config,
                                                         size_t display_count,
                                                         const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(banjo_config_stamp != nullptr);
  has_image_ = display_count != 0 && display_config[0].layer_count != 0;
  {
    fbl::AutoLock lock(&mtx_);
    config_stamp_ = display::ToConfigStamp(*banjo_config_stamp);
  }
}

zx_status_t FramebufferDisplay::DisplayEngineSetBufferCollectionConstraints(
    const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR,
            "SetBufferCollectionConstraints: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(
      PixelFormatAndModifier(properties_.pixel_format, kFormatModifier));
  uint32_t bytes_per_row = properties_.row_stride_px * bytes_per_pixel;

  fidl::Arena arena;
  auto constraints = fuchsia_sysmem2::wire::BufferCollectionConstraints::Builder(arena);
  auto buffer_usage = fuchsia_sysmem2::wire::BufferUsage::Builder(arena);
  buffer_usage.display(fuchsia_sysmem2::wire::kDisplayUsageLayer);
  constraints.usage(buffer_usage.Build());
  auto buffer_constraints = fuchsia_sysmem2::wire::BufferMemoryConstraints::Builder(arena);
  buffer_constraints.min_size_bytes(0);
  buffer_constraints.max_size_bytes(properties_.height_px * bytes_per_row);
  buffer_constraints.physically_contiguous_required(false);
  buffer_constraints.secure_required(false);
  buffer_constraints.ram_domain_supported(true);
  buffer_constraints.cpu_domain_supported(true);
  auto heap = fuchsia_sysmem2::wire::Heap::Builder(arena);
  heap.heap_type(bind_fuchsia_sysmem_heap::HEAP_TYPE_FRAMEBUFFER);
  heap.id(0);
  buffer_constraints.permitted_heaps(std::array{heap.Build()});
  constraints.buffer_memory_constraints(buffer_constraints.Build());
  auto image_constraints = fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena);
  image_constraints.pixel_format(properties_.pixel_format);
  image_constraints.pixel_format_modifier(kFormatModifier);
  image_constraints.color_spaces(std::array{fuchsia_images2::ColorSpace::kSrgb});
  image_constraints.min_size({.width = static_cast<uint32_t>(properties_.width_px),
                              .height = static_cast<uint32_t>(properties_.height_px)});
  image_constraints.max_size({.width = static_cast<uint32_t>(properties_.width_px),
                              .height = static_cast<uint32_t>(properties_.height_px)});
  image_constraints.min_bytes_per_row(bytes_per_row);
  image_constraints.max_bytes_per_row(bytes_per_row);
  constraints.image_format_constraints(std::array{image_constraints.Build()});

  auto set_request = fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena);
  set_request.constraints(constraints.Build());
  auto result = collection->SetConstraints(set_request.Build());

  if (!result.ok()) {
    FDF_LOG(ERROR, "failed to set constraints, %s", result.FormatDescription().c_str());
    return result.status();
  }

  return ZX_OK;
}

// implement sysmem heap protocol:

void FramebufferDisplay::AllocateVmo(AllocateVmoRequestView request,
                                     AllocateVmoCompleter::Sync& completer) {
  BufferKey buffer_key(request->buffer_collection_id, request->buffer_index);

  zx_info_handle_count handle_count;
  zx_status_t status = framebuffer_mmio_.get_vmo()->get_info(
      ZX_INFO_HANDLE_COUNT, &handle_count, sizeof(handle_count), nullptr, nullptr);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  if (handle_count.handle_count != 1) {
    completer.ReplyError(ZX_ERR_NO_RESOURCES);
    return;
  }
  zx::vmo vmo;
  status = framebuffer_mmio_.get_vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  }

  bool had_framebuffer_key;
  {
    fbl::AutoLock lock(&framebuffer_key_mtx_);
    had_framebuffer_key = framebuffer_key_.has_value();
    if (!had_framebuffer_key) {
      framebuffer_key_ = buffer_key;
    }
  }
  if (had_framebuffer_key) {
    completer.ReplyError(ZX_ERR_NO_RESOURCES);
    return;
  }

  completer.ReplySuccess(std::move(vmo));
}

void FramebufferDisplay::DeleteVmo(DeleteVmoRequestView request,
                                   DeleteVmoCompleter::Sync& completer) {
  {
    fbl::AutoLock lock(&framebuffer_key_mtx_);
    framebuffer_key_.reset();
  }

  // Semantics of DeleteVmo are to recycle all resources tied to the sysmem allocation before
  // replying, so we close the VMO handle here before replying. Even if it shares an object and
  // pages with a VMO handle we're not closing, this helps clarify wrt semantics of DeleteVmo.
  request->vmo.reset();

  completer.Reply();
}

// implement driver object:

zx::result<> FramebufferDisplay::Initialize() {
  auto [heap_client, heap_server] = fidl::Endpoints<fuchsia_hardware_sysmem::Heap>::Create();

  auto result = hardware_sysmem_->RegisterHeap(
      static_cast<uint64_t>(fuchsia_sysmem::wire::HeapType::kFramebuffer), std::move(heap_client));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to register sysmem heap: %s", result.status_string());
    return zx::error(result.status());
  }

  // Start heap server.
  auto arena = std::make_unique<fidl::Arena<512>>();
  fuchsia_hardware_sysmem::wire::HeapProperties heap_properties = GetHeapProperties(*arena.get());
  async::PostTask(&dispatcher_, [server_end = std::move(heap_server), arena = std::move(arena),
                                 heap_properties = std::move(heap_properties), this]() mutable {
    auto binding = fidl::BindServer(&dispatcher_, std::move(server_end), this,
                                    [](FramebufferDisplay* self, fidl::UnbindInfo info,
                                       fidl::ServerEnd<fuchsia_hardware_sysmem::Heap> server_end) {
                                      OnHeapServerClose(info, server_end.TakeChannel());
                                    });
    auto result = fidl::WireSendEvent(binding)->OnRegister(std::move(heap_properties));
    if (!result.ok()) {
      FDF_LOG(ERROR, "OnRegister() failed: %s", result.FormatDescription().c_str());
    }
  });

  // Start vsync loop.
  vsync_task_.Post(&dispatcher_);

  FDF_LOG(INFO,
          "Initialized display, %" PRId32 " x %" PRId32 " (stride=%" PRId32 " format=%" PRIu32 ")",
          properties_.width_px, properties_.height_px, properties_.row_stride_px,
          static_cast<uint32_t>(properties_.pixel_format));

  return zx::ok();
}

FramebufferDisplay::FramebufferDisplay(
    fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> hardware_sysmem,
    fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem, fdf::MmioBuffer framebuffer_mmio,
    const DisplayProperties& properties, async_dispatcher_t* dispatcher)
    : hardware_sysmem_(std::move(hardware_sysmem)),
      sysmem_(std::move(sysmem)),
      dispatcher_(*dispatcher),
      has_image_(false),
      framebuffer_mmio_(std::move(framebuffer_mmio)),
      properties_(properties),
      next_vsync_time_(zx::clock::get_monotonic()) {
  ZX_DEBUG_ASSERT(dispatcher != nullptr);

  if (sysmem_) {
    zx_koid_t current_process_koid = GetCurrentProcessKoid();
    std::string debug_name = "framebuffer-display[" + std::to_string(current_process_koid) + "]";
    fidl::Arena arena;
    auto set_debug_request =
        fuchsia_sysmem2::wire::AllocatorSetDebugClientInfoRequest::Builder(arena);
    set_debug_request.name(debug_name);
    set_debug_request.id(current_process_koid);
    auto set_debug_status = sysmem_->SetDebugClientInfo(set_debug_request.Build());
    if (!set_debug_status.ok()) {
      FDF_LOG(ERROR, "Cannot set sysmem allocator debug info: %s",
              set_debug_status.status_string());
    }
  }
}

void FramebufferDisplay::OnPeriodicVSync(async_dispatcher_t* dispatcher, async::TaskBase* task,
                                         zx_status_t status) {
  if (status != ZX_OK) {
    if (status == ZX_ERR_CANCELED) {
      FDF_LOG(INFO, "Vsync task is canceled.");
    } else {
      FDF_LOG(ERROR, "Failed to run Vsync task: %s", zx_status_get_string(status));
    }
    return;
  }

  if (engine_listener_.is_valid()) {
    fbl::AutoLock lock(&mtx_);
    const uint64_t banjo_display_id = display::ToBanjoDisplayId(kDisplayId);
    const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(config_stamp_);
    engine_listener_.OnDisplayVsync(banjo_display_id, next_vsync_time_.get(), &banjo_config_stamp);
  }
  next_vsync_time_ += kVSyncInterval;
  zx_status_t post_status = vsync_task_.PostForTime(&dispatcher_, next_vsync_time_);
  if (post_status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to post Vsync task for the next Vsync: %s",
            zx_status_get_string(status));
  }
}

display_engine_protocol_t FramebufferDisplay::GetProtocol() {
  return {
      .ops = &display_engine_protocol_ops_,
      .ctx = this,
  };
}

}  // namespace framebuffer_display
