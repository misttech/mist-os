// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_FRAMEBUFFER_DISPLAY_FRAMEBUFFER_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_LIB_FRAMEBUFFER_DISPLAY_FRAMEBUFFER_DISPLAY_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fit/function.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <atomic>
#include <cstdint>
#include <mutex>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-interface.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-layer.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/layer-composition-operations.h"
#include "src/graphics/display/lib/api-types/cpp/mode-id.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace framebuffer_display {

struct DisplayProperties {
  int32_t width_px;
  int32_t height_px;
  int32_t row_stride_px;
  display::PixelFormat pixel_format;
};

class FramebufferDisplay;
using HeapServer = fidl::WireServer<fuchsia_hardware_sysmem::Heap>;
using BufferKey = std::pair<uint64_t, uint32_t>;
class FramebufferDisplay : public HeapServer, public display::DisplayEngineInterface {
 public:
  // `dispatcher` must be non-null and outlive the newly created instance.
  FramebufferDisplay(display::DisplayEngineEventsInterface* engine_events,
                     fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_client,
                     fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem_hardware_client,
                     fdf::MmioBuffer framebuffer_mmio, const DisplayProperties& properties,
                     async_dispatcher_t* dispatcher);
  ~FramebufferDisplay() = default;

  // Initialization logic not suitable in the constructor.
  zx::result<> Initialize();

  void AllocateVmo(AllocateVmoRequestView request, AllocateVmoCompleter::Sync& completer) override;
  void DeleteVmo(DeleteVmoRequestView request, DeleteVmoCompleter::Sync& completer) override;

  // DisplayEngineInterface:
  void OnCoordinatorConnected() override;
  zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> buffer_collection_token) override;
  zx::result<> ReleaseBufferCollection(
      display::DriverBufferCollectionId buffer_collection_id) override;
  zx::result<display::DriverImageId> ImportImage(
      const display::ImageMetadata& image_metadata,
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) override;
  zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId buffer_collection_id, uint32_t buffer_index) override;
  void ReleaseImage(display::DriverImageId image_id) override;
  display::ConfigCheckResult CheckConfiguration(
      display::DisplayId display_id, display::ModeId display_mode_id,
      cpp20::span<const display::DriverLayer> layers,
      cpp20::span<display::LayerCompositionOperations> layer_composition_operations) override;
  void ApplyConfiguration(display::DisplayId display_id, display::ModeId display_mode_id,
                          cpp20::span<const display::DriverLayer> layers,
                          display::ConfigStamp config_stamp) override;
  zx::result<> SetBufferCollectionConstraints(
      const display::ImageBufferUsage& image_buffer_usage,
      display::DriverBufferCollectionId buffer_collection_id) override;
  zx::result<> SetDisplayPower(display::DisplayId display_id, bool power_on) override;
  bool IsCaptureSupported() override;
  zx::result<> StartCapture(display::DriverCaptureImageId capture_image_id) override;
  zx::result<> ReleaseCapture(display::DriverCaptureImageId capture_image_id) override;
  zx::result<> SetMinimumRgb(uint8_t minimum_rgb) override;

  const std::unordered_map<display::DriverBufferCollectionId,
                           fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>>&
  GetBufferCollectionsForTesting() const {
    return buffer_collections_;
  }

 private:
  void OnPeriodicVSync(async_dispatcher_t* dispatcher, async::TaskBase* task, zx_status_t status);

  fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem_hardware_client_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_client_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>>
      buffer_collections_;

  async_dispatcher_t& dispatcher_;
  async::TaskMethod<FramebufferDisplay, &FramebufferDisplay::OnPeriodicVSync> vsync_task_{this};

  // protects only framebuffer_key_
  std::mutex framebuffer_key_mtx_;
  std::optional<BufferKey> framebuffer_key_ TA_GUARDED(framebuffer_key_mtx_);

  static_assert(std::atomic<bool>::is_always_lock_free);
  std::atomic<bool> has_image_;

  // A lock is required to ensure the atomicity when setting |config_stamp| in
  // |ApplyConfiguration()| and passing |&config_stamp_| to |OnDisplayVsync()|.
  std::mutex mtx_;
  display::ConfigStamp config_stamp_ TA_GUARDED(mtx_) = display::kInvalidConfigStamp;

  const fdf::MmioBuffer framebuffer_mmio_;
  const DisplayProperties properties_;

  const fuchsia_images2::wire::PixelFormatModifier kFormatModifier =
      fuchsia_images2::wire::PixelFormatModifier::kLinear;

  // Only used on the vsync thread.
  zx::time next_vsync_time_;

  display::DisplayEngineEventsInterface& engine_events_;
};

}  // namespace framebuffer_display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_FRAMEBUFFER_DISPLAY_FRAMEBUFFER_DISPLAY_H_
