// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_H_

#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fuchsia/hardware/display/clamprgb/c/banjo.h>
#include <fuchsia/hardware/display/clamprgb/cpp/banjo.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <sys/types.h>
#include <unistd.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <atomic>
#include <cstdint>
#include <memory>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

namespace fake_display {

class FakeDisplay;
using DeviceType = ddk::Device<FakeDisplay, ddk::GetProtocolable, ddk::ChildPreReleaseable>;

struct FakeDisplayDeviceConfig {
  // If enabled, the fake display device will not automatically emit Vsync
  // events. `SendVsync()` must be called to emit a Vsync event manually.
  bool manual_vsync_trigger = false;

  // If true, the fake display device will never access imported image buffers,
  // and it will not add extra image format constraints to the imported buffer
  // collection.
  // Otherwise, it may add extra BufferCollection constraints to ensure that the
  // allocated image buffers support CPU access, and may access the imported
  // image buffers for capturing.
  // Display capture is supported iff this field is false.
  //
  // TODO(fxbug.dev/128891): This is a temporary workaround to support fake
  // display device for GPU devices that cannot render into CPU-accessible
  // formats directly. Remove this option when we have a fake Vulkan
  // implementation.
  bool no_buffer_access = false;
};

class FakeDisplay : public DeviceType,
                    public ddk::DisplayControllerImplProtocol<FakeDisplay, ddk::base_protocol>,
                    public ddk::DisplayClampRgbImplProtocol<FakeDisplay> {
 public:
  explicit FakeDisplay(zx_device_t* parent, FakeDisplayDeviceConfig device_config);

  FakeDisplay(const FakeDisplay&) = delete;
  FakeDisplay& operator=(const FakeDisplay&) = delete;

  ~FakeDisplay();

  // This function is called from the c-bind function upon driver matching.
  zx_status_t Bind();

  // DisplayControllerImplProtocol implementation:
  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* intf);
  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplImportImage(image_t* image,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index);
  void DisplayControllerImplReleaseImage(image_t* image);
  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t** display_configs, size_t display_count, uint32_t** layer_cfg_results,
      size_t* layer_cfg_result_count);
  void DisplayControllerImplApplyConfiguration(const display_config_t** display_config,
                                               size_t display_count,
                                               const config_stamp_t* banjo_config_stamp);
  void DisplayControllerImplSetEld(uint64_t display_id, const uint8_t* raw_eld_list,
                                   size_t raw_eld_count);
  zx_status_t DisplayControllerImplGetSysmemConnection(zx::channel connection);
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_t* config, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on);
  zx_status_t DisplayControllerImplSetDisplayCaptureInterface(
      const display_capture_interface_protocol_t* intf);
  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle)
      __TA_EXCLUDES(capture_lock_);
  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle)
      __TA_EXCLUDES(capture_lock_);
  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle)
      __TA_EXCLUDES(capture_lock_);
  bool DisplayControllerImplIsCaptureCompleted() __TA_EXCLUDES(capture_lock_);

  zx_status_t DisplayClampRgbImplSetMinimumRgb(uint8_t minimum_rgb);

  // Required functions for DeviceType
  void DdkRelease();
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_protocol);
  void DdkChildPreRelease(void* child_ctx) {
    fbl::AutoLock lock(&interface_lock_);
    controller_interface_client_ = ddk::DisplayControllerInterfaceProtocolClient();
  }

  const display_controller_impl_protocol_t* display_controller_impl_banjo_protocol() const {
    return &display_controller_impl_banjo_protocol_;
  }
  const display_clamp_rgb_impl_protocol_t* display_clamp_rgb_impl_banjo_protocol() const {
    return &display_clamp_rgb_impl_banjo_protocol_;
  }

  void SendVsync();

  // Just for display core unittests.
  zx_status_t ImportVmoImage(image_t* image, zx::vmo vmo, size_t offset);

  size_t TEST_imported_images_count() const {
    fbl::AutoLock lock(&image_lock_);
    return imported_images_.size_slow();
  }

  uint8_t GetClampRgbValue() const {
    fbl::AutoLock lock(&capture_lock_);
    return clamp_rgb_value_;
  }

 private:
  struct ImageInfo : public fbl::DoublyLinkedListable<std::unique_ptr<ImageInfo>> {
    char pixel_format;
    bool ram_domain;
    zx::vmo vmo;
  };

  enum class BufferCollectionUsage : int32_t;

  zx_status_t SetupDisplayInterface();
  int VSyncThread();
  int CaptureThread() __TA_EXCLUDES(capture_lock_, image_lock_);
  void PopulateAddedDisplayArgs(added_display_args_t* args);

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  fuchsia_sysmem::BufferCollectionConstraints CreateBufferCollectionConstraints(
      BufferCollectionUsage usage);

  // Constraints applicable to all buffers used for display images.
  void SetBufferMemoryConstraints(fuchsia_sysmem::BufferMemoryConstraints& constraints);

  // Constraints applicable to all image buffers used in Display.
  void SetCommonImageFormatConstraints(fuchsia_sysmem::PixelFormatType pixel_format_type,
                                       fuchsia_sysmem::FormatModifier format_modifier,
                                       fuchsia_sysmem::ImageFormatConstraints& constraints);

  // Constraints applicable to images buffers used in image capture.
  void SetCaptureImageFormatConstraints(fuchsia_sysmem::ImageFormatConstraints& constraints);

  // Constraints applicable to image buffers that will be bound to layers.
  void SetLayerImageFormatConstraints(fuchsia_sysmem::ImageFormatConstraints& constraints);

  // Banjo vtable for fuchsia.hardware.display.controller.DisplayControllerImpl.
  const display_controller_impl_protocol_t display_controller_impl_banjo_protocol_;

  // Banjo vtable for fuchsia.hardware.display.clamprgb.DisplayClampRgbImpl.
  const display_clamp_rgb_impl_protocol_t display_clamp_rgb_impl_banjo_protocol_;

  FakeDisplayDeviceConfig device_config_;

  ddk::PDevFidl pdev_;
  ddk::SysmemProtocolClient sysmem_;

  std::atomic_bool vsync_shutdown_flag_ = false;
  std::atomic_bool capture_shutdown_flag_ = false;

  // Thread handles. Only used on the thread that starts/stops us.
  bool vsync_thread_running_ = false;
  thrd_t vsync_thread_;
  thrd_t capture_thread_;

  // Guards display coordinator interface.
  mutable fbl::Mutex interface_lock_;

  // Guards imported images and references to imported images.
  mutable fbl::Mutex image_lock_;

  // Guards imported capture buffers, capture interface and state.
  mutable fbl::Mutex capture_lock_;

  // Points to the next capture target image to capture displayed contents into.
  // Stores nullptr if capture is not going to be performed.
  ImageInfo* current_capture_target_image_ TA_GUARDED(capture_lock_);

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::SyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  // Imported Images
  fbl::DoublyLinkedList<std::unique_ptr<ImageInfo>> imported_images_ TA_GUARDED(image_lock_);
  fbl::DoublyLinkedList<std::unique_ptr<ImageInfo>> imported_captures_ TA_GUARDED(capture_lock_);

  // Points to the current image to be displayed and captured.
  // Stores nullptr if there is no image displaying on the fake display.
  ImageInfo* current_image_to_capture_ TA_GUARDED(image_lock_);

  // The most recently applied config stamp.
  std::atomic<display::ConfigStamp> current_config_stamp_ = display::kInvalidConfigStamp;

  // Capture complete is signaled at vsync time. This counter introduces a bit of delay
  // for signal capture complete
  uint64_t capture_complete_signal_count_ TA_GUARDED(capture_lock_) = 0;

  // Minimum value of RGB channels, via the DisplayClampRgbImpl protocol.
  //
  // This is associated with the display capture lock so we have the option to
  // reflect the clamping when we simulate display capture.
  uint8_t clamp_rgb_value_ TA_GUARDED(capture_lock_) = 0;

  // Display controller related data
  ddk::DisplayControllerInterfaceProtocolClient controller_interface_client_
      TA_GUARDED(interface_lock_);

  // Display Capture interface protocol
  ddk::DisplayCaptureInterfaceProtocolClient capture_interface_client_ TA_GUARDED(capture_lock_);
};

}  // namespace fake_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_H_
