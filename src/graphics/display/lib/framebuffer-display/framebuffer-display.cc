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

#include <cinttypes>
#include <cstdint>
#include <memory>
#include <mutex>
#include <utility>

#include <bind/fuchsia/sysmem/heap/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-interface.h"
#include "src/graphics/display/lib/api-types/cpp/alpha-mode.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/coordinate-transformation.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"
#include "src/graphics/display/lib/api-types/cpp/layer-composition-operations.h"
#include "src/graphics/display/lib/api-types/cpp/mode-and-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"
#include "src/graphics/display/lib/api-types/cpp/mode.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"
#include "src/graphics/display/lib/api-types/cpp/rectangle.h"

namespace framebuffer_display {

namespace {

static constexpr display::DisplayId kDisplayId(1);
static constexpr display::ModeId kDisplayModeId(1);
static constexpr int kRefreshRateHz = 30;

static constexpr uint64_t kImageHandle = 0xdecafc0ffee;

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

void FramebufferDisplay::OnCoordinatorConnected() {
  const display::ModeAndId mode_and_id({
      .id = kDisplayModeId,
      .mode = display::Mode({
          .active_width = properties_.width_px,
          .active_height = properties_.height_px,
          .refresh_rate_millihertz = kRefreshRateHz * 1'000,
      }),
  });

  const cpp20::span<const display::ModeAndId> preferred_modes(&mode_and_id, 1);
  const cpp20::span<const display::PixelFormat> pixel_formats(&properties_.pixel_format, 1);
  engine_events_.OnDisplayAdded(kDisplayId, preferred_modes, pixel_formats);
}

zx::result<> FramebufferDisplay::ImportBufferCollection(
    display::DriverBufferCollectionId buffer_collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) {
  if (buffer_collections_.find(buffer_collection_id) != buffer_collections_.end()) {
    FDF_LOG(ERROR, "Buffer Collection (id=%lu) already exists", buffer_collection_id.value());
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_client_.is_valid(), "sysmem allocator is not initialized");

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

  fidl::Arena arena;
  fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest bind_request =
      fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena)
          .token(std::move(buffer_collection_token))
          .buffer_collection_request(std::move(collection_server_endpoint))
          .Build();
  auto bind_result = sysmem_client_->BindSharedCollection(std::move(bind_request));
  if (!bind_result.ok()) {
    FDF_LOG(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
            bind_result.status_string());
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  buffer_collections_[buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));

  return zx::ok();
}

zx::result<> FramebufferDisplay::ReleaseBufferCollection(
    display::DriverBufferCollectionId buffer_collection_id) {
  if (buffer_collections_.find(buffer_collection_id) == buffer_collections_.end()) {
    FDF_LOG(ERROR, "Cannot release buffer collection %lu: buffer collection doesn't exist",
            buffer_collection_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  buffer_collections_.erase(buffer_collection_id);
  return zx::ok();
}

zx::result<display::DriverImageId> FramebufferDisplay::ImportImage(
    const display::ImageMetadata& image_metadata,
    display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) {
  const auto it = buffer_collections_.find(buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
            buffer_collection_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  fidl::WireResult check_result = collection->CheckAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    FDF_LOG(ERROR, "failed to check buffers allocated, %s",
            check_result.FormatDescription().c_str());
    return zx::error(check_result.status());
  }
  const auto& check_response = check_result.value();
  if (check_response.is_error()) {
    if (check_response.error_value() == fuchsia_sysmem2::Error::kPending) {
      return zx::error(ZX_ERR_SHOULD_WAIT);
    }
    return zx::error(sysmem::V1CopyFromV2Error(check_response.error_value()));
  }

  fidl::WireResult wait_result = collection->WaitForAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    FDF_LOG(ERROR, "failed to wait for buffers allocated, %s",
            wait_result.FormatDescription().c_str());
    return zx::error(wait_result.status());
  }
  auto& wait_response = wait_result.value();
  if (wait_response.is_error()) {
    return zx::error(sysmem::V1CopyFromV2Error(wait_response.error_value()));
  }
  fuchsia_sysmem2::wire::BufferCollectionInfo& collection_info =
      wait_response->buffer_collection_info();

  if (!collection_info.settings().has_image_format_constraints()) {
    FDF_LOG(ERROR, "no image format constraints");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (buffer_index > 0) {
    FDF_LOG(ERROR, "invalid index %d, greater than 0", buffer_index);
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  fuchsia_images2::wire::PixelFormat sysmem2_collection_format =
      collection_info.settings().image_format_constraints().pixel_format();
  if (sysmem2_collection_format != properties_.pixel_format.ToFidl()) {
    FDF_LOG(ERROR,
            "Image format from sysmem (%" PRIu32 ") doesn't match expected format (%" PRIu32 ")",
            static_cast<uint32_t>(sysmem2_collection_format),
            properties_.pixel_format.ValueForLogging());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // We only need the VMO temporarily to get the BufferKey. The BufferCollection client_end in
  // buffer_collections_ is not SetWeakOk (and therefore is known to be strong at this point), so
  // it's not necessary to keep this VMO for the buffer to remain alive.
  zx::vmo vmo = std::move(collection_info.buffers()[0].vmo());

  fidl::Arena arena;
  auto vmo_info_result =
      sysmem_client_->GetVmoInfo(fuchsia_sysmem2::wire::AllocatorGetVmoInfoRequest::Builder(arena)
                                     .vmo(std::move(vmo))
                                     .Build());
  if (!vmo_info_result.ok()) {
    return zx::error(vmo_info_result.error().status());
  }
  if (!vmo_info_result->is_ok()) {
    return zx::error(sysmem::V1CopyFromV2Error(vmo_info_result->error_value()));
  }
  auto& vmo_info = vmo_info_result->value();
  BufferKey buffer_key(vmo_info->buffer_collection_id(), vmo_info->buffer_index());

  bool key_matched;
  {
    std::lock_guard lock(framebuffer_key_mtx_);
    key_matched = framebuffer_key_.has_value() && (*framebuffer_key_ == buffer_key);
  }
  if (!key_matched) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (image_metadata.width() != properties_.width_px ||
      image_metadata.height() != properties_.height_px) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(kImageHandle);
}

zx::result<display::DriverCaptureImageId> FramebufferDisplay::ImportImageForCapture(
    display::DriverBufferCollectionId driver_buffer_collection_id, uint32_t index) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

void FramebufferDisplay::ReleaseImage(display::DriverImageId image_id) {
  // noop
}

display::ConfigCheckResult FramebufferDisplay::CheckConfiguration(
    display::DisplayId display_id, display::ModeId display_mode_id,
    cpp20::span<const display::DriverLayer> layers,
    cpp20::span<display::LayerCompositionOperations> layer_composition_operations) {
  ZX_DEBUG_ASSERT(display_id == kDisplayId);

  ZX_DEBUG_ASSERT(layer_composition_operations.size() == layers.size());
  ZX_DEBUG_ASSERT(layers.size() == 1);

  if (display_mode_id != kDisplayModeId) {
    return display::ConfigCheckResult::kUnsupportedDisplayModes;
  }

  const display::DriverLayer& layer = layers[0];
  const display::Rectangle display_area({
      .x = 0,
      .y = 0,
      .width = properties_.width_px,
      .height = properties_.height_px,
  });

  display::ConfigCheckResult result = display::ConfigCheckResult::kOk;
  if (layer.display_destination() != display_area) {
    // TODO(https://fxbug.dev/388602122): Revise the definition of MERGE_BASE to
    // include this case, or replace with a different opcode.
    layer_composition_operations[0] = layer_composition_operations[0].WithMergeBase();
    result = display::ConfigCheckResult::kUnsupportedConfig;
  }
  if (layer.image_source() != layer.display_destination()) {
    layer_composition_operations[0] = layer_composition_operations[0].WithFrameScale();
    result = display::ConfigCheckResult::kUnsupportedConfig;
  }
  if (layer.image_metadata().dimensions() != layer.image_source().dimensions()) {
    layer_composition_operations[0] = layer_composition_operations[0].WithSrcFrame();
    result = display::ConfigCheckResult::kUnsupportedConfig;
  }
  if (layer.alpha_mode() != display::AlphaMode::kDisable) {
    layer_composition_operations[0] = layer_composition_operations[0].WithAlpha();
    result = display::ConfigCheckResult::kUnsupportedConfig;
  }
  if (layer.image_source_transformation() != display::CoordinateTransformation::kIdentity) {
    layer_composition_operations[0] = layer_composition_operations[0].WithTransform();
    result = display::ConfigCheckResult::kUnsupportedConfig;
  }
  return result;
}

void FramebufferDisplay::ApplyConfiguration(display::DisplayId display_id,
                                            display::ModeId display_mode_id,
                                            cpp20::span<const display::DriverLayer> layers,
                                            display::DriverConfigStamp config_stamp) {
  ZX_DEBUG_ASSERT(display_id == kDisplayId);
  ZX_DEBUG_ASSERT(display_mode_id == kDisplayModeId);

  ZX_DEBUG_ASSERT(layers.size() == 1);
  has_image_ = true;
  {
    std::lock_guard lock(mtx_);
    config_stamp_ = config_stamp;
  }
}

zx::result<> FramebufferDisplay::SetBufferCollectionConstraints(
    const display::ImageBufferUsage& image_buffer_usage,
    display::DriverBufferCollectionId buffer_collection_id) {
  const auto it = buffer_collections_.find(buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR,
            "SetBufferCollectionConstraints: Cannot find imported buffer collection (id=%lu)",
            buffer_collection_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  const uint32_t bytes_per_pixel = ImageFormatStrideBytesPerWidthPixel(
      PixelFormatAndModifier(properties_.pixel_format.ToFidl(), kFormatModifier));
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
  image_constraints.pixel_format(properties_.pixel_format.ToFidl());
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
    return zx::error(result.status());
  }

  return zx::ok();
}

bool FramebufferDisplay::IsCaptureSupported() { return false; }

zx::result<> FramebufferDisplay::SetDisplayPower(display::DisplayId display_id, bool power_on) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> FramebufferDisplay::StartCapture(display::DriverCaptureImageId capture_image_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> FramebufferDisplay::ReleaseCapture(display::DriverCaptureImageId capture_image_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx::result<> FramebufferDisplay::SetMinimumRgb(uint8_t minimum_rgb) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
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
    std::lock_guard lock(framebuffer_key_mtx_);
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
    std::lock_guard lock(framebuffer_key_mtx_);
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

  auto result = sysmem_hardware_client_->RegisterHeap(
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
          properties_.pixel_format.ValueForLogging());

  return zx::ok();
}

FramebufferDisplay::FramebufferDisplay(
    display::DisplayEngineEventsInterface* engine_events,
    fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_client,
    fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem_hardware_client,
    fdf::MmioBuffer framebuffer_mmio, const DisplayProperties& properties,
    async_dispatcher_t* dispatcher)
    : sysmem_hardware_client_(std::move(sysmem_hardware_client)),
      sysmem_client_(std::move(sysmem_client)),
      dispatcher_(*dispatcher),
      has_image_(false),
      framebuffer_mmio_(std::move(framebuffer_mmio)),
      properties_(properties),
      next_vsync_time_(zx::clock::get_monotonic()),
      engine_events_(*engine_events) {
  ZX_DEBUG_ASSERT(dispatcher != nullptr);
  ZX_DEBUG_ASSERT(engine_events != nullptr);

  if (sysmem_client_) {
    zx_koid_t current_process_koid = GetCurrentProcessKoid();
    std::string debug_name = "framebuffer-display[" + std::to_string(current_process_koid) + "]";
    fidl::Arena arena;
    auto set_debug_request =
        fuchsia_sysmem2::wire::AllocatorSetDebugClientInfoRequest::Builder(arena);
    set_debug_request.name(debug_name);
    set_debug_request.id(current_process_koid);
    auto set_debug_status = sysmem_client_->SetDebugClientInfo(set_debug_request.Build());
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

  display::DriverConfigStamp vsync_config_stamp;
  {
    std::lock_guard lock(mtx_);
    vsync_config_stamp = config_stamp_;
  }
  engine_events_.OnDisplayVsync(kDisplayId, next_vsync_time_, vsync_config_stamp);

  next_vsync_time_ += kVSyncInterval;
  zx_status_t post_status = vsync_task_.PostForTime(&dispatcher_, next_vsync_time_);
  if (post_status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to post Vsync task for the next Vsync: %s",
            zx_status_get_string(status));
  }
}

}  // namespace framebuffer_display
