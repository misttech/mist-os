// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/goldfish-display/display.h"

#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/goldfish/control/cpp/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/time.h>
#include <lib/async/cpp/wait.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/trace/event.h>
#include <lib/fidl/cpp/channel.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fit/defer.h>
#include <lib/image-format/image_format.h>
#include <lib/zircon-internal/align.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <algorithm>
#include <limits>
#include <memory>
#include <optional>
#include <sstream>
#include <vector>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>

#include "src/devices/lib/goldfish/pipe_headers/include/base.h"
#include "src/graphics/display/drivers/goldfish-display/render_control.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace goldfish {
namespace {

const char* kTag = "goldfish-display";

constexpr display::DisplayId kPrimaryDisplayId(1);

constexpr fuchsia_images2_pixel_format_enum_value_t kPixelFormats[] = {
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kBgra32),
    static_cast<fuchsia_images2_pixel_format_enum_value_t>(
        fuchsia_images2::wire::PixelFormat::kR8G8B8A8),
};

constexpr uint32_t FB_WIDTH = 1;
constexpr uint32_t FB_HEIGHT = 2;
constexpr uint32_t FB_FPS = 5;

constexpr uint32_t GL_RGBA = 0x1908;
constexpr uint32_t GL_BGRA_EXT = 0x80E1;

}  // namespace

// static
zx_status_t Display::Create(void* ctx, zx_device_t* device) {
  auto display = std::make_unique<Display>(device);

  zx_status_t status = display->Bind();
  if (status == ZX_OK) {
    // devmgr now owns device.
    [[maybe_unused]] auto* dev = display.release();
  }
  return status;
}

Display::Display(zx_device_t* parent)
    : DisplayType(parent), loop_(&kAsyncLoopConfigNeverAttachToThread) {
  if (parent) {
    control_ = parent;
  }
}

Display::~Display() {
  loop_.Shutdown();

  for (auto& it : devices_) {
    TeardownDisplay(it.first);
  }
}

zx_status_t Display::Bind() {
  fbl::AutoLock lock(&lock_);

  if (!control_.is_valid()) {
    zxlogf(ERROR, "%s: no control protocol", kTag);
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_goldfish_pipe::GoldfishPipe>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  auto channel = endpoints->server.TakeChannel();

  zx_status_t status = control_.ConnectToPipeDevice(std::move(channel));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not connect to pipe device: %s", kTag, zx_status_get_string(status));
    return status;
  }

  pipe_ = fidl::WireSyncClient(std::move(endpoints->client));
  if (!pipe_.is_valid()) {
    zxlogf(ERROR, "%s: no pipe protocol", kTag);
    return ZX_ERR_NOT_SUPPORTED;
  }

  status = InitSysmemAllocatorClientLocked();
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: cannot initialize sysmem allocator: %s", kTag, zx_status_get_string(status));
    return status;
  }

  // Create a second FIDL connection for use by RenderControl.
  endpoints = fidl::CreateEndpoints<fuchsia_hardware_goldfish_pipe::GoldfishPipe>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  channel = endpoints->server.TakeChannel();

  status = control_.ConnectToPipeDevice(std::move(channel));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not connect to pipe device: %s", kTag, zx_status_get_string(status));
    return status;
  }

  fidl::WireSyncClient pipe_client{std::move(endpoints->client)};

  rc_ = std::make_unique<RenderControl>();
  status = rc_->InitRcPipe(std::move(pipe_client));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: RenderControl failed to initialize: %d", kTag, status);
    return ZX_ERR_NOT_SUPPORTED;
  }

  display::DisplayId next_display_id = kPrimaryDisplayId;

  // Parse optional display params. This is comma seperated list of
  // display devices. The format is:
  //
  // widthxheight[-xpos+ypos][@refresh][%scale]
  char* flag = getenv("driver.goldfish.displays");
  if (flag) {
    std::stringstream devices_stream(flag);
    std::string device_string;
    while (std::getline(devices_stream, device_string, ',')) {
      Device device;
      char delim = 0;
      std::stringstream device_stream(device_string);
      do {
        switch (delim) {
          case 0:
            device_stream >> device.width;
            break;
          case 'x':
            device_stream >> device.height;
            break;
          case '-':
            device_stream >> device.x;
            break;
          case '+':
            device_stream >> device.y;
            break;
          case '@':
            device_stream >> device.refresh_rate_hz;
            break;
          case '%':
            device_stream >> device.scale;
            break;
        }
      } while (device_stream >> delim);

      if (!device.width || !device.height) {
        zxlogf(ERROR, "%s: skip device=%s, missing size", kTag, device_string.c_str());
        continue;
      }
      if (!device.refresh_rate_hz) {
        zxlogf(ERROR, "%s: skip device=%s, refresh rate is zero", kTag, device_string.c_str());
        continue;
      }
      if (device.scale < 0.1f || device.scale > 100.f) {
        zxlogf(ERROR, "%s: skip device=%s, scale is not in range 0.1-100", kTag,
               device_string.c_str());
        continue;
      }

      auto& new_device = devices_[next_display_id++];
      new_device.width = device.width;
      new_device.height = device.height;
      new_device.x = device.x;
      new_device.y = device.y;
      new_device.refresh_rate_hz = device.refresh_rate_hz;
      new_device.scale = device.scale;
    }
  }

  // Create primary device if needed.
  if (devices_.empty()) {
    auto& device = devices_[kPrimaryDisplayId];
    device.width = static_cast<uint32_t>(rc_->GetFbParam(FB_WIDTH, 1024));
    device.height = static_cast<uint32_t>(rc_->GetFbParam(FB_HEIGHT, 768));
    device.refresh_rate_hz = static_cast<uint32_t>(rc_->GetFbParam(FB_FPS, 60));
  }

  // Set up display and set up flush task for each device.
  for (auto& it : devices_) {
    zx_status_t status = SetupDisplay(it.first);
    ZX_DEBUG_ASSERT(status == ZX_OK);

    async::PostTask(loop_.dispatcher(), [this, display_id = it.first] {
      FlushDisplay(loop_.dispatcher(), display_id);
    });
  }

  // Start async event thread.
  loop_.StartThread("goldfish_display_event_thread");

  return DdkAdd("goldfish-display");
}

void Display::DdkRelease() { delete this; }

void Display::DisplayControllerImplSetDisplayControllerInterface(
    const display_controller_interface_protocol_t* interface) {
  std::vector<added_display_args_t> args;
  for (auto& it : devices_) {
    added_display_args_t display = {
        .display_id = display::ToBanjoDisplayId(it.first),
        .edid_present = false,
        .panel =
            {
                .params =
                    {
                        .width = it.second.width,
                        .height = it.second.height,
                        .refresh_rate_e2 = it.second.refresh_rate_hz * 100,
                    },
            },
        .pixel_format_list = kPixelFormats,
        .pixel_format_count = sizeof(kPixelFormats) / sizeof(kPixelFormats[0]),
        .cursor_info_list = nullptr,
        .cursor_info_count = 0,
    };
    args.push_back(display);
  }

  {
    fbl::AutoLock lock(&flush_lock_);
    dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient(interface);
    dc_intf_.OnDisplaysChanged(args.data(), args.size(), nullptr, 0, nullptr, 0, nullptr);
  }
}

zx_status_t Display::InitSysmemAllocatorClientLocked() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  if (!endpoints.is_ok()) {
    zxlogf(ERROR, "Cannot create sysmem allocator endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }
  auto& [client, server] = endpoints.value();
  auto connect_result = pipe_->ConnectSysmem(server.TakeChannel());
  if (!connect_result.ok()) {
    zxlogf(ERROR, "Cannot connect to sysmem Allocator protocol: %s",
           connect_result.status_string());
    return connect_result.status();
  }
  sysmem_allocator_client_ = fidl::WireSyncClient(std::move(client));

  std::string debug_name = fxl::StringPrintf("goldfish-display[%lu]", fsl::GetCurrentProcessKoid());
  auto set_debug_status = sysmem_allocator_client_->SetDebugClientInfo(
      fidl::StringView::FromExternal(debug_name), fsl::GetCurrentProcessKoid());
  if (!set_debug_status.ok()) {
    zxlogf(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
    return set_debug_status.status();
  }

  return ZX_OK;
}

namespace {

uint32_t GetColorBufferFormatFromSysmemPixelFormat(
    const fuchsia_sysmem::PixelFormat& pixel_format) {
  switch (pixel_format.type()) {
    case fuchsia_sysmem::PixelFormatType::kR8G8B8A8:
      return GL_BGRA_EXT;
    case fuchsia_sysmem::PixelFormatType::kBgra32:
      return GL_RGBA;
    default:
      // This should not happen. The sysmem-negotiated pixel format must be supported.
      ZX_ASSERT_MSG(false, "Import unsupported image: %u",
                    static_cast<uint32_t>(pixel_format.type()));
  }
}

}  // namespace

zx_status_t Display::ImportVmoImage(image_t* image, const fuchsia_sysmem::PixelFormat& pixel_format,
                                    zx::vmo vmo, size_t offset) {
  auto color_buffer = std::make_unique<ColorBuffer>();
  const uint32_t color_buffer_format = GetColorBufferFormatFromSysmemPixelFormat(pixel_format);

  fidl::Arena unused_arena;
  const uint32_t bytes_per_pixel =
      ImageFormatStrideBytesPerWidthPixel(fidl::ToWire(unused_arena, pixel_format));
  color_buffer->size = fbl::round_up(image->width * image->height * bytes_per_pixel,
                                     static_cast<uint32_t>(PAGE_SIZE));

  // Linear images must be pinned.
  color_buffer->pinned_vmo =
      rc_->pipe_io()->PinVmo(vmo, ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS, offset, color_buffer->size);

  color_buffer->vmo = std::move(vmo);
  color_buffer->width = image->width;
  color_buffer->height = image->height;
  color_buffer->format = color_buffer_format;

  auto status = rc_->CreateColorBuffer(image->width, image->height, color_buffer_format);
  if (status.is_error()) {
    zxlogf(ERROR, "%s: failed to create color buffer", kTag);
    return status.error_value();
  }
  color_buffer->host_color_buffer_id = status.value();

  image->handle = reinterpret_cast<uint64_t>(color_buffer.release());
  return ZX_OK;
}

zx_status_t Display::DisplayControllerImplImportBufferCollection(
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
      fidl::SyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t Display::DisplayControllerImplReleaseBufferCollection(
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

zx_status_t Display::DisplayControllerImplImportImage(image_t* image,
                                                      uint64_t banjo_driver_buffer_collection_id,
                                                      uint32_t index) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }

  const fidl::SyncClient<fuchsia_sysmem::BufferCollection>& collection_client = it->second;
  fidl::Result check_result = collection_client->CheckBuffersAllocated();
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
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

  fidl::Result wait_result = collection_client->WaitForBuffersAllocated();
  // TODO(fxbug.dev/121691): The sysmem FIDL error logging patterns are
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

  zx::vmo vmo;
  if (index < collection_info.buffer_count()) {
    vmo = std::move(collection_info.buffers()[index].vmo());
    ZX_DEBUG_ASSERT(!collection_info.buffers()[index].vmo().is_valid());
  }

  if (!vmo.is_valid()) {
    zxlogf(ERROR, "%s: invalid index", kTag);
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!collection_info.settings().has_image_format_constraints()) {
    zxlogf(ERROR, "Buffer collection doesn't have valid image format constraints");
    return ZX_ERR_NOT_SUPPORTED;
  }

  uint64_t offset = collection_info.buffers()[index].vmo_usable_start();
  if (collection_info.settings().buffer_settings().heap() !=
      fuchsia_sysmem::HeapType::kGoldfishDeviceLocal) {
    const auto& pixel_format = collection_info.settings().image_format_constraints().pixel_format();
    return ImportVmoImage(image, pixel_format, std::move(vmo), offset);
  }

  if (offset != 0) {
    zxlogf(ERROR, "VMO offset (%lu) not supported for Goldfish device local color buffers", offset);
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto color_buffer = std::make_unique<ColorBuffer>();
  color_buffer->vmo = std::move(vmo);
  image->handle = reinterpret_cast<uint64_t>(color_buffer.release());
  return ZX_OK;
}

void Display::DisplayControllerImplReleaseImage(image_t* image) {
  auto color_buffer = reinterpret_cast<ColorBuffer*>(image->handle);

  // Color buffer is owned by image in the linear case.
  if (image->type == IMAGE_TYPE_SIMPLE) {
    rc_->CloseColorBuffer(color_buffer->host_color_buffer_id);
  }

  async::PostTask(loop_.dispatcher(), [this, color_buffer] {
    for (auto& kv : devices_) {
      if (kv.second.incoming_config.has_value() &&
          kv.second.incoming_config->color_buffer == color_buffer) {
        kv.second.incoming_config = std::nullopt;
      }
    }
    delete color_buffer;
  });
}

config_check_result_t Display::DisplayControllerImplCheckConfiguration(
    const display_config_t** display_configs, size_t display_count, uint32_t** layer_cfg_results,
    size_t* layer_cfg_result_count) {
  if (display_count == 0) {
    return CONFIG_CHECK_RESULT_OK;
  }
  for (unsigned i = 0; i < display_count; i++) {
    const size_t layer_count = display_configs[i]->layer_count;
    const display::DisplayId display_id = display::ToDisplayId(display_configs[i]->display_id);
    if (layer_count > 0) {
      ZX_DEBUG_ASSERT(devices_.find(display_id) != devices_.end());
      const Device& device = devices_[display_id];

      if (display_configs[i]->cc_flags != 0) {
        // Color Correction is not supported, but we will pretend we do.
        // TODO(fxbug.dev/36184): Returning error will cause blank screen if scenic requests
        // color correction. For now, lets pretend we support it, until a proper
        // fix is done (either from scenic or from core display)
        zxlogf(WARNING, "%s: Color Correction not support. No error reported", __func__);
      }

      if (display_configs[i]->layer_list[0]->type != LAYER_TYPE_PRIMARY) {
        // We only support PRIMARY layer. Notify client to convert layer to
        // primary type.
        layer_cfg_results[i][0] |= CLIENT_USE_PRIMARY;
        layer_cfg_result_count[i] = 1;
      } else {
        primary_layer_t* layer = &display_configs[i]->layer_list[0]->cfg.primary;
        // Scaling is allowed if destination frame match display and
        // source frame match image.
        frame_t dest_frame = {
            .x_pos = 0,
            .y_pos = 0,
            .width = device.width,
            .height = device.height,
        };
        frame_t src_frame = {
            .x_pos = 0,
            .y_pos = 0,
            .width = layer->image.width,
            .height = layer->image.height,
        };
        if (memcmp(&layer->dest_frame, &dest_frame, sizeof(frame_t)) != 0) {
          // TODO(fxbug.dev/36222): Need to provide proper flag to indicate driver only
          // accepts full screen dest frame.
          layer_cfg_results[i][0] |= CLIENT_FRAME_SCALE;
        }
        if (memcmp(&layer->src_frame, &src_frame, sizeof(frame_t)) != 0) {
          layer_cfg_results[i][0] |= CLIENT_SRC_FRAME;
        }

        if (layer->alpha_mode != ALPHA_DISABLE) {
          // Alpha is not supported.
          layer_cfg_results[i][0] |= CLIENT_ALPHA;
        }

        if (layer->transform_mode != FRAME_TRANSFORM_IDENTITY) {
          // Transformation is not supported.
          layer_cfg_results[i][0] |= CLIENT_TRANSFORM;
        }

        // Check if any changes to the base layer were required.
        if (layer_cfg_results[i][0] != 0) {
          layer_cfg_result_count[i] = 1;
        }
      }
      // If there is more than one layer, the rest need to be merged into the base layer.
      if (layer_count > 1) {
        layer_cfg_results[i][0] |= CLIENT_MERGE_BASE;
        for (unsigned j = 1; j < layer_count; j++) {
          layer_cfg_results[i][j] |= CLIENT_MERGE_SRC;
        }
        layer_cfg_result_count[i] = layer_count;
      }
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

zx_status_t Display::PresentDisplayConfig(display::DisplayId display_id,
                                          const DisplayConfig& display_config) {
  auto* color_buffer = display_config.color_buffer;
  if (!color_buffer) {
    return ZX_OK;
  }

  zx::eventpair event_display, event_sync_device;
  zx_status_t status = zx::eventpair::create(0u, &event_display, &event_sync_device);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: zx_eventpair_create failed: %d", kTag, status);
    return status;
  }

  auto& device = devices_[display_id];

  // Set up async wait for the goldfish sync event. The zx::eventpair will be
  // stored in the async wait callback, which will be destroyed only when the
  // event is signaled or the wait is cancelled.
  device.pending_config_waits.emplace_back(event_display.get(), ZX_EVENTPAIR_SIGNALED, 0);
  auto& wait = device.pending_config_waits.back();

  wait.Begin(loop_.dispatcher(), [event = std::move(event_display), &device,
                                  pending_config_stamp = display_config.config_stamp](
                                     async_dispatcher_t* dispatcher, async::WaitOnce* current_wait,
                                     zx_status_t status, const zx_packet_signal_t*) {
    TRACE_DURATION("gfx", "Display::SyncEventHandler", "config_stamp",
                   pending_config_stamp.value());
    if (status == ZX_ERR_CANCELED) {
      zxlogf(INFO, "Wait for config stamp %lu cancelled.", pending_config_stamp.value());
      return;
    }
    ZX_DEBUG_ASSERT_MSG(status == ZX_OK, "Invalid wait status: %d\n", status);

    // When the eventpair in |current_wait| is signalled, all the pending waits
    // that are queued earlier than that eventpair will be removed from the list
    // and the async WaitOnce will be cancelled.
    // Note that the cancelled waits will return early and will not reach here.
    ZX_DEBUG_ASSERT(std::any_of(device.pending_config_waits.begin(),
                                device.pending_config_waits.end(),
                                [current_wait](const async::WaitOnce& wait) {
                                  return wait.object() == current_wait->object();
                                }));
    // Remove all the pending waits that are queued earlier than the current
    // wait, and the current wait itself. In WaitOnce, the callback is moved to
    // stack before current wait is removed, so it's safe to remove any item in
    // the list.
    for (auto it = device.pending_config_waits.begin(); it != device.pending_config_waits.end();) {
      if (it->object() == current_wait->object()) {
        device.pending_config_waits.erase(it);
        break;
      }
      it = device.pending_config_waits.erase(it);
    }
    device.latest_config_stamp = std::max(device.latest_config_stamp, pending_config_stamp);
  });

  // Update host-writeable display buffers before presenting.
  if (color_buffer->pinned_vmo.region_count() > 0) {
    auto status = rc_->UpdateColorBuffer(
        color_buffer->host_color_buffer_id, color_buffer->pinned_vmo, color_buffer->width,
        color_buffer->height, color_buffer->format, color_buffer->size);
    if (status.is_error() || status.value()) {
      zxlogf(ERROR, "%s : color buffer update failed: %d:%u", kTag, status.status_value(),
             status.value_or(0));
      return status.is_error() ? status.status_value() : ZX_ERR_INTERNAL;
    }
  }

  // Present the buffer.
  {
    HostDisplayId host_display_id = devices_[display_id].host_display_id;
    if (host_display_id != kInvalidHostDisplayId) {
      // Set color buffer for secondary displays.
      auto status = rc_->SetDisplayColorBuffer(host_display_id, color_buffer->host_color_buffer_id);
      if (status.is_error() || status.value()) {
        zxlogf(ERROR, "%s: failed to set display color buffer", kTag);
        return status.is_error() ? status.status_value() : ZX_ERR_INTERNAL;
      }
    } else {
      status = rc_->FbPost(color_buffer->host_color_buffer_id);
      if (status != ZX_OK) {
        zxlogf(ERROR, "%s: FbPost failed: %d", kTag, status);
        return status;
      }
    }

    fbl::AutoLock lock(&lock_);
    status = control_.CreateSyncFence(std::move(event_sync_device));
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: CreateSyncFence failed: %d", kTag, status);
      return status;
    }
  }

  return ZX_OK;
}

void Display::DisplayControllerImplApplyConfiguration(const display_config_t** display_configs,
                                                      size_t display_count,
                                                      const config_stamp_t* banjo_config_stamp) {
  ZX_DEBUG_ASSERT(banjo_config_stamp != nullptr);
  display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);
  for (const auto& it : devices_) {
    uint64_t handle = 0;
    for (unsigned i = 0; i < display_count; i++) {
      if (display::ToDisplayId(display_configs[i]->display_id) == it.first) {
        if (display_configs[i]->layer_count) {
          handle = display_configs[i]->layer_list[0]->cfg.primary.image.handle;
        }
        break;
      }
    }

    if (handle == 0u) {
      // The display doesn't have any active layers right now. For layers that
      // previously existed, we should cancel waiting events on the pending
      // color buffer and remove references to both pending and current color
      // buffers.
      async::PostTask(loop_.dispatcher(), [this, display_id = it.first, config_stamp] {
        if (devices_.find(display_id) != devices_.end()) {
          auto& device = devices_[display_id];
          device.pending_config_waits.clear();
          device.incoming_config = std::nullopt;
          device.latest_config_stamp = std::max(device.latest_config_stamp, config_stamp);
        }
      });
      return;
    }

    auto color_buffer = reinterpret_cast<ColorBuffer*>(handle);
    if (color_buffer && color_buffer->host_color_buffer_id == kInvalidHostColorBufferId) {
      zx::vmo vmo;

      zx_status_t status = color_buffer->vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
      if (status != ZX_OK) {
        zxlogf(ERROR, "%s: failed to duplicate vmo: %d", kTag, status);
      } else {
        fbl::AutoLock lock(&lock_);

        uint32_t render_control_encoded_color_buffer_id = kInvalidHostColorBufferId.value();
        status = control_.GetColorBuffer(std::move(vmo), &render_control_encoded_color_buffer_id);
        if (status != ZX_OK) {
          zxlogf(ERROR, "%s: failed to get color buffer: %d", kTag, status);
        }
        color_buffer->host_color_buffer_id =
            ToHostColorBufferId(render_control_encoded_color_buffer_id);

        // Color buffers are in vulkan-only mode by default as that avoids
        // unnecessary copies on the host in some cases. The color buffer
        // needs to be moved out of vulkan-only mode before being used for
        // presentation.
        if (color_buffer->host_color_buffer_id != kInvalidHostColorBufferId) {
          auto status = rc_->SetColorBufferVulkanMode(color_buffer->host_color_buffer_id, 0);
          if (status.is_error() || status.value()) {
            zxlogf(ERROR, "%s: failed to set vulkan mode: %d %d", kTag, status.status_value(),
                   status.value_or(0));
          }
        }
      }
    }

    if (color_buffer) {
      async::PostTask(loop_.dispatcher(),
                      [this, config_stamp, color_buffer, display_id = it.first] {
                        devices_[display_id].incoming_config = {
                            .color_buffer = color_buffer,
                            .config_stamp = config_stamp,
                        };
                      });
    }
  }
}

zx_status_t Display::DisplayControllerImplGetSysmemConnection(zx::channel connection) {
  fbl::AutoLock lock(&lock_);
  auto result = pipe_->ConnectSysmem(std::move(connection));
  zx_status_t status = result.status();
  if (!result.ok()) {
    zxlogf(ERROR, "%s: failed to connect to sysmem: %s", kTag, result.status_string());
  }
  return status;
}

zx_status_t Display::DisplayControllerImplSetBufferCollectionConstraints(
    const image_t* config, uint64_t banjo_driver_buffer_collection_id) {
  const display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    zxlogf(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
           driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::SyncClient<fuchsia_sysmem::BufferCollection>& collection = it->second;

  fuchsia_sysmem::BufferCollectionConstraints constraints;
  constraints.usage().display() = fuchsia_sysmem::kDisplayUsageLayer;
  constraints.has_buffer_memory_constraints() = true;
  auto& buffer_constraints = constraints.buffer_memory_constraints();
  buffer_constraints.min_size_bytes() = 0;
  buffer_constraints.max_size_bytes() = 0xffffffff;
  buffer_constraints.physically_contiguous_required() = true;
  buffer_constraints.secure_required() = false;
  buffer_constraints.ram_domain_supported() = true;
  buffer_constraints.cpu_domain_supported() = true;
  buffer_constraints.inaccessible_domain_supported() = true;
  buffer_constraints.heap_permitted_count() = 2;
  buffer_constraints.heap_permitted()[0] = fuchsia_sysmem::HeapType::kSystemRam;
  buffer_constraints.heap_permitted()[1] = fuchsia_sysmem::HeapType::kGoldfishDeviceLocal;
  constraints.image_format_constraints_count() = 4;
  for (uint32_t i = 0; i < constraints.image_format_constraints_count(); i++) {
    auto& image_constraints = constraints.image_format_constraints()[i];
    image_constraints.pixel_format().type() = i & 0b01 ? fuchsia_sysmem::PixelFormatType::kR8G8B8A8
                                                       : fuchsia_sysmem::PixelFormatType::kBgra32;
    image_constraints.pixel_format().has_format_modifier() = true;
    image_constraints.pixel_format().format_modifier().value() =
        i & 0b10 ? fuchsia_sysmem::kFormatModifierLinear
                 : fuchsia_sysmem::kFormatModifierGoogleGoldfishOptimal;
    image_constraints.color_spaces_count() = 1;
    image_constraints.color_space()[0].type() = fuchsia_sysmem::ColorSpaceType::kSrgb;
    image_constraints.min_coded_width() = 0;
    image_constraints.max_coded_width() = 0xffffffff;
    image_constraints.min_coded_height() = 0;
    image_constraints.max_coded_height() = 0xffffffff;
    image_constraints.min_bytes_per_row() = 0;
    image_constraints.max_bytes_per_row() = 0xffffffff;
    image_constraints.max_coded_width_times_coded_height() = 0xffffffff;
    image_constraints.layers() = 1;
    image_constraints.coded_width_divisor() = 1;
    image_constraints.coded_height_divisor() = 1;
    image_constraints.bytes_per_row_divisor() = 1;
    image_constraints.start_offset_divisor() = 1;
    image_constraints.display_width_divisor() = 1;
    image_constraints.display_height_divisor() = 1;
  }

  auto set_result = collection->SetConstraints({true, std::move(constraints)});
  if (set_result.is_error()) {
    zxlogf(ERROR, "%s: failed to set constraints", kTag);
    return set_result.error_value().status();
  }

  return ZX_OK;
}

zx_status_t Display::SetupDisplay(display::DisplayId display_id) {
  Device& device = devices_[display_id];

  // Create secondary displays.
  if (display_id != kPrimaryDisplayId) {
    auto status = rc_->CreateDisplay();
    if (status.is_error()) {
      return status.error_value();
    }
    device.host_display_id = status.value();
  }
  uint32_t width = static_cast<uint32_t>(static_cast<float>(device.width) * device.scale);
  uint32_t height = static_cast<uint32_t>(static_cast<float>(device.height) * device.scale);
  auto status = rc_->SetDisplayPose(device.host_display_id, device.x, device.y, width, height);
  if (status.is_error() || status.value()) {
    zxlogf(ERROR, "%s: failed to set display pose: %d %d", kTag, status.status_value(),
           status.value_or(0));
    return status.is_error() ? status.error_value() : ZX_ERR_INTERNAL;
  }
  device.expected_next_flush = async::Now(loop_.dispatcher());

  return ZX_OK;
}

void Display::TeardownDisplay(display::DisplayId display_id) {
  Device& device = devices_[display_id];

  if (device.host_display_id != kInvalidHostDisplayId) {
    zx::result<uint32_t> status = rc_->DestroyDisplay(device.host_display_id);
    ZX_DEBUG_ASSERT(status.is_ok());
    ZX_DEBUG_ASSERT(!status.value());
  }
}

void Display::FlushDisplay(async_dispatcher_t* dispatcher, display::DisplayId display_id) {
  Device& device = devices_[display_id];

  zx::duration period = zx::sec(1) / device.refresh_rate_hz;
  zx::time expected_next_flush = device.expected_next_flush + period;

  if (device.incoming_config.has_value()) {
    zx_status_t status = PresentDisplayConfig(display_id, *device.incoming_config);
    ZX_DEBUG_ASSERT(status == ZX_OK || status == ZX_ERR_SHOULD_WAIT);
  }

  {
    fbl::AutoLock lock(&flush_lock_);

    if (dc_intf_.is_valid()) {
      zx::time now = async::Now(dispatcher);
      const uint64_t banjo_display_id = display::ToBanjoDisplayId(display_id);
      const config_stamp_t banjo_config_stamp =
          display::ToBanjoConfigStamp(device.latest_config_stamp);
      dc_intf_.OnDisplayVsync(banjo_display_id, now.get(), &banjo_config_stamp);
    }
  }

  // If we've already passed the |expected_next_flush| deadline, skip the
  // Vsync and adjust the deadline to the earliest next available frame.
  zx::time now = async::Now(dispatcher);
  if (now > expected_next_flush) {
    expected_next_flush +=
        period * (((now - expected_next_flush + period).get() - 1L) / period.get());
  }

  device.expected_next_flush = expected_next_flush;
  async::PostTaskForTime(
      dispatcher, [this, dispatcher, display_id] { FlushDisplay(dispatcher, display_id); },
      expected_next_flush);
}

}  // namespace goldfish

static constexpr zx_driver_ops_t goldfish_display_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = goldfish::Display::Create;
  return ops;
}();

ZIRCON_DRIVER(goldfish_display, goldfish_display_driver_ops, "zircon", "0.1");
