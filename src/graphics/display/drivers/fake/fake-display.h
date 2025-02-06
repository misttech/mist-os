// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_H_

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/channel.h>
#include <lib/zx/vmo.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <mutex>
#include <optional>
#include <thread>
#include <unordered_map>

#include "src/graphics/display/drivers/fake/image-info.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"

namespace fake_display {

struct FakeDisplayDeviceConfig {
  // Enables periodically-generated VSync events.
  //
  // By default, this member is false. Tests must call `FakeDisplay::TriggerVsync()`
  // explicitly to get VSync events.
  //
  // If set to true, the `FakeDisplay` implementation will periodically generate
  // VSync events. These periodically-generated VSync events are a source of
  // non-determinism. They can lead to flaky tests, when coupled with overly
  // strict assertions around event timing.
  bool periodic_vsync = false;

  // If true, the fake display device will never access imported image buffers,
  // and it will not add extra image format constraints to the imported buffer
  // collection.
  // Otherwise, it may add extra BufferCollection constraints to ensure that the
  // allocated image buffers support CPU access, and may access the imported
  // image buffers for capturing.
  // Display capture is supported iff this field is false.
  //
  // TODO(https://fxbug.dev/42079320): This is a temporary workaround to support fake
  // display device for GPU devices that cannot render into CPU-accessible
  // formats directly. Remove this option when we have a fake Vulkan
  // implementation.
  bool no_buffer_access = false;
};

class FakeDisplay : public ddk::DisplayEngineProtocol<FakeDisplay> {
 public:
  explicit FakeDisplay(FakeDisplayDeviceConfig device_config,
                       fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_allocator,
                       inspect::Inspector inspector);

  FakeDisplay(const FakeDisplay&) = delete;
  FakeDisplay& operator=(const FakeDisplay&) = delete;

  ~FakeDisplay();

  // DisplayEngineProtocol implementation:
  void DisplayEngineSetListener(const display_engine_listener_protocol_t* engine_listener);
  void DisplayEngineUnsetListener();
  zx_status_t DisplayEngineImportBufferCollection(uint64_t banjo_driver_buffer_collection_id,
                                                  zx::channel collection_token);
  zx_status_t DisplayEngineReleaseBufferCollection(uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayEngineImportImage(const image_metadata_t* image_metadata,
                                       uint64_t banjo_driver_buffer_collection_id, uint32_t index,
                                       uint64_t* out_image_handle);
  void DisplayEngineReleaseImage(uint64_t image_handle);
  config_check_result_t DisplayEngineCheckConfiguration(
      const display_config_t* display_configs, size_t display_count,
      layer_composition_operations_t* out_layer_composition_operations_list,
      size_t layer_composition_operations_count, size_t* out_layer_composition_operations_actual);
  void DisplayEngineApplyConfiguration(const display_config_t* display_configs,
                                       size_t display_count,
                                       const config_stamp_t* banjo_config_stamp);
  zx_status_t DisplayEngineSetBufferCollectionConstraints(
      const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayEngineSetDisplayPower(uint64_t display_id, bool power_on);
  zx_status_t DisplayEngineImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                 uint32_t index, uint64_t* out_capture_handle)
      __TA_EXCLUDES(capture_mutex_);
  bool DisplayEngineIsCaptureSupported();
  zx_status_t DisplayEngineStartCapture(uint64_t capture_handle) __TA_EXCLUDES(capture_mutex_);
  zx_status_t DisplayEngineReleaseCapture(uint64_t capture_handle) __TA_EXCLUDES(capture_mutex_);

  zx_status_t DisplayEngineSetMinimumRgb(uint8_t minimum_rgb);

  const display_engine_protocol_t* display_engine_banjo_protocol() const {
    return &display_engine_banjo_protocol_;
  }

  bool IsCaptureSupported() const;

  // Dispatches an OnVsync() event to the Display Coordinator.
  void SendVsync() __TA_EXCLUDES(engine_listener_mutex_);

  // Just for display core unittests.
  zx::result<display::DriverImageId> ImportVmoImageForTesting(zx::vmo vmo, size_t offset);

  uint8_t GetClampRgbValue() const {
    std::lock_guard capture_lock(capture_mutex_);
    return clamp_rgb_value_;
  }

  const inspect::Inspector& inspector() const { return inspector_; }

 private:
  enum class BufferCollectionUsage : int32_t;

  // The loop executed by the VSync thread.
  void VSyncThread() __TA_EXCLUDES(capture_mutex_, image_mutex_);

  // The loop executed by the capture thread.
  void CaptureThread() __TA_EXCLUDES(capture_mutex_, image_mutex_);

  // Simulates a display capture, if a capture was requested.
  zx::result<> ServiceAnyCaptureRequest();

  // Simulates a display capture for a single-layer image configuration.
  static zx::result<> DoImageCapture(DisplayImageInfo& source_info,
                                     CaptureImageInfo& destination_info);

  // Dispatches an OnCaptureComplete() event to the Display Coordinator.
  void SendCaptureComplete() __TA_EXCLUDES(engine_listener_mutex_);

  // Dispatches an OnDisplayAdded() event to the Display Coordinator.
  void SendDisplayInformation() __TA_EXCLUDES(engine_listener_mutex_);

  // Must be called exactly once before using the Sysmem client.
  void InitializeSysmemClient();

  fuchsia_sysmem2::BufferCollectionConstraints CreateBufferCollectionConstraints(
      BufferCollectionUsage usage);

  // Constraints applicable to all buffers used for display images.
  void SetBufferMemoryConstraints(fuchsia_sysmem2::BufferMemoryConstraints& constraints);

  // Constraints applicable to all image buffers used in Display.
  void SetCommonImageFormatConstraints(fuchsia_images2::PixelFormat pixel_format,
                                       fuchsia_images2::PixelFormatModifier format_modifier,
                                       fuchsia_sysmem2::ImageFormatConstraints& constraints);

  // Constraints applicable to images buffers used in image capture.
  void SetCaptureImageFormatConstraints(fuchsia_sysmem2::ImageFormatConstraints& constraints);

  // Constraints applicable to image buffers that will be bound to layers.
  void SetLayerImageFormatConstraints(fuchsia_sysmem2::ImageFormatConstraints& constraints);

  // Records the display config to the inspector's root node. The root node must
  // be already initialized.
  void RecordDisplayConfigToInspectRootNode();

  // Banjo vtable for fuchsia.hardware.display.controller.DisplayEngine.
  const display_engine_protocol_t display_engine_banjo_protocol_;

  FakeDisplayDeviceConfig device_config_;

  // Checked by the VSync thread on every loop iteration.
  std::atomic_bool vsync_thread_shutdown_requested_ = false;

  // Checked by the Capture thread on every loop iteration.
  std::atomic_bool capture_thread_shutdown_requested_ = false;

  // Only accessed by the constructor and destructor.
  std::optional<std::thread> vsync_thread_;
  std::optional<std::thread> capture_thread_;

  // Guards imported images and references to imported images.
  mutable std::mutex image_mutex_;

  // Guards imported capture buffers, capture interface and state.
  // `capture_mutex_` must never be acquired when `image_mutex_` is already
  // held.
  mutable std::mutex capture_mutex_;

  // Guards the Display Coordinator's event listener connection.
  //
  // No other mutex may be acquired while this mutex is acquired. In other words,
  // event dispatch logic is expected to capture this mutex, dispatch the event,
  // and immediately release the mutex. This ordering keeps the driver ready
  // for a migration to the api-protocols library.
  mutable std::mutex engine_listener_mutex_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::SyncClient<fuchsia_sysmem2::Allocator> sysmem_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::SyncClient<fuchsia_sysmem2::BufferCollection>>
      buffer_collections_;

  // Imported display images, keyed by image ID.
  DisplayImageInfo::HashTable imported_images_ TA_GUARDED(image_mutex_);

  // ID of the current image to be displayed and captured.
  // Stores `kInvalidDriverImageId` if there is no image displaying on the fake
  // display.
  display::DriverImageId current_image_to_capture_id_ TA_GUARDED(image_mutex_) =
      display::kInvalidDriverImageId;

  // The driver image ID for the next display image to be imported to the
  // device.
  // Note: we cannot use std::atomic here, since std::atomic only allows
  // built-in integral and floating types to do atomic arithmetics.
  display::DriverImageId next_imported_display_driver_image_id_ TA_GUARDED(image_mutex_) =
      display::DriverImageId(1);

  // Imported capture images, keyed by image ID.
  CaptureImageInfo::HashTable imported_captures_ TA_GUARDED(capture_mutex_);

  // ID of the next capture target image imported to the fake display device
  // to capture displayed contents into.
  // Stores `kInvalidDriverCaptureImageId` if capture is not going to be
  // performed.
  display::DriverCaptureImageId current_capture_target_image_id_ TA_GUARDED(capture_mutex_) =
      display::kInvalidDriverCaptureImageId;

  // The driver capture image ID for the next image to be imported to the
  // device.
  display::DriverCaptureImageId next_imported_driver_capture_image_id_ TA_GUARDED(capture_mutex_) =
      display::DriverCaptureImageId(1);

  // The most recently applied config stamp.
  std::atomic<display::DriverConfigStamp> current_config_stamp_ =
      display::kInvalidDriverConfigStamp;

  // Capture complete is signaled at vsync time. This counter introduces a bit of delay
  // for signal capture complete
  uint64_t capture_complete_signal_count_ TA_GUARDED(capture_mutex_) = 0;

  // Minimum value of RGB channels, via the SetMinimumRgb() method.
  //
  // This is associated with the display capture lock so we have the option to
  // reflect the clamping when we simulate display capture.
  uint8_t clamp_rgb_value_ TA_GUARDED(capture_mutex_) = 0;

  // Display controller related data
  ddk::DisplayEngineListenerProtocolClient engine_listener_client_
      TA_GUARDED(engine_listener_mutex_);

  inspect::Inspector inspector_;
};

}  // namespace fake_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_H_
