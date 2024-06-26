// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fit/function.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/clock.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <atomic>
#include <memory>

#include <fbl/mutex.h>

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

namespace simple_display {

struct DisplayProperties {
  int32_t width_px;
  int32_t height_px;
  int32_t row_stride_px;
  fuchsia_images2::wire::PixelFormat pixel_format;
};

class SimpleDisplay;
using HeapServer = fidl::WireServer<fuchsia_hardware_sysmem::Heap>;
using BufferKey = std::pair<uint64_t, uint32_t>;
class SimpleDisplay : public HeapServer, public ddk::DisplayControllerImplProtocol<SimpleDisplay> {
 public:
  SimpleDisplay(fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> hardware_sysmem,
                fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem,
                fdf::MmioBuffer framebuffer_mmio, const DisplayProperties& properties);
  ~SimpleDisplay() = default;

  // Initialization logic not suitable in the constructor.
  zx::result<> Initialize();

  void AllocateVmo(AllocateVmoRequestView request, AllocateVmoCompleter::Sync& completer) override;
  void DeleteVmo(DeleteVmoRequestView request, DeleteVmoCompleter::Sync& completer) override;

  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* intf);
  void DisplayControllerImplResetDisplayControllerInterface();
  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplImportImage(const image_metadata_t* banjo_image_metadata,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index, uint64_t* out_image_handle);
  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  void DisplayControllerImplReleaseImage(uint64_t image_handle);
  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t* display_configs, size_t display_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual);
  void DisplayControllerImplApplyConfiguration(const display_config_t* display_config,
                                               size_t display_count,
                                               const config_stamp_t* banjo_config_stamp);
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  bool DisplayControllerImplIsCaptureSupported() { return false; }
  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  bool DisplayControllerImplIsCaptureCompleted() { return false; }
  zx_status_t DisplayControllerImplSetMinimumRgb(uint8_t minimum_rgb) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const std::unordered_map<display::DriverBufferCollectionId,
                           fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>>&
  GetBufferCollectionsForTesting() const {
    return buffer_collections_;
  }

  display_controller_impl_protocol_t GetProtocol();

 private:
  bool IsBanjoDisplayConfigSupported(const display_config_t& banjo_display_config);

  void OnPeriodicVSync();

  fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> hardware_sysmem_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>>
      buffer_collections_;

  async::Loop loop_;

  // protects only framebuffer_key_
  fbl::Mutex framebuffer_key_mtx_;
  std::optional<BufferKey> framebuffer_key_ TA_GUARDED(framebuffer_key_mtx_);

  static_assert(std::atomic<bool>::is_always_lock_free);
  std::atomic<bool> has_image_;

  // A lock is required to ensure the atomicity when setting |config_stamp| in
  // |ApplyConfiguration()| and passing |&config_stamp_| to |OnDisplayVsync()|.
  fbl::Mutex mtx_;
  display::ConfigStamp config_stamp_ TA_GUARDED(mtx_) = display::kInvalidConfigStamp;

  const fdf::MmioBuffer framebuffer_mmio_;
  const DisplayProperties properties_;

  const fuchsia_images2::wire::PixelFormatModifier kFormatModifier =
      fuchsia_images2::wire::PixelFormatModifier::kLinear;

  // Only used on the vsync thread.
  zx::time next_vsync_time_;
  ddk::DisplayControllerInterfaceProtocolClient intf_;
};

}  // namespace simple_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_SIMPLE_SIMPLE_DISPLAY_H_
