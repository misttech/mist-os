// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_DISPLAY_COMPOSITOR_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_DISPLAY_COMPOSITOR_H_

#include <fuchsia/sysmem/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/time.h>

#include <cstdint>
#include <deque>
#include <memory>
#include <unordered_map>

#include "src/lib/fxl/synchronization/thread_annotations.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"
#include "src/ui/scenic/lib/display/display.h"
#include "src/ui/scenic/lib/display/util.h"
#include "src/ui/scenic/lib/flatland/engine/color_conversion_state_machine.h"
#include "src/ui/scenic/lib/flatland/engine/engine_types.h"
#include "src/ui/scenic/lib/flatland/engine/release_fence_manager.h"
#include "src/ui/scenic/lib/flatland/renderer/renderer.h"

namespace flatland {

namespace test {
class DisplayCompositorSmokeTest;
class DisplayCompositorPixelTest;
class DisplayCompositorTest;
}  // namespace test

using allocation::BufferCollectionUsage;

// The DisplayCompositor is responsible for compositing Flatland render data onto the display(s).
// It accomplishes this either by direct hardware compositing via the display coordinator
// interface, or rendering on the GPU via a custom renderer API. It also handles the
// registration of sysmem buffer collections and importation of images to both the
// display coordinator and the renderer via the BufferCollectionImporter interface. The
// BufferCollectionImporter interface is how Flatland instances communicate with the
// DisplayCompositor, providing it with the necessary data to render without exposing to Flatland
// the DisplayCoordinator or other dependencies.
//
// TODO(fxbug.dev/77414): we use a weak ptr to safely post a task that might outlive this
// DisplayCompositor, see RenderFrame().  This task simulates a vsync callback that we aren't yet
// receiving because the display coordinator doesn't yet implement the ApplyConfig2() method. It's
// likely that shared_from_this will become unnecessary at that time.
class DisplayCompositor final : public allocation::BufferCollectionImporter,
                                public std::enable_shared_from_this<DisplayCompositor> {
 public:
  // Describes the result of RenderFrame().  If it succeeds it is either by showing client images
  // directly on the display, or by first using the GPU to composite them into a single image.
  enum class RenderFrameResult { kDirectToDisplay, kGpuComposition, kFailure };
  // Args which can be passed to customize RenderFrame() behavior in tests.  The default values are
  // the ones used in production.
  struct RenderFrameTestArgs {
    bool force_gpu_composition = false;

    // This is a workaround so that RenderFrame() can provide a default value, while still allowing
    // callers to use aggregate initialization syntax.  Adding a default constructor would sacrifice
    // this ability.  See:
    // https://stackoverflow.com/questions/53408962/try-to-understand-compiler-error-message-default-member-initializer-required-be
    static RenderFrameTestArgs Default() { return {}; }
  };

  // TODO(fxbug.dev/66807): The DisplayCompositor has multiple parts of its code where usage of the
  // display coordinator is protected by locks, because of the multithreaded environment of
  // flatland. Ideally, we'd want the DisplayCompositor to have sole ownership of the display
  // coordinator - meaning that it would require a unique_ptr instead of a shared_ptr. But since
  // access to the real display coordinator is provided to clients via a shared_ptr, we take in a
  // shared_ptr as a parameter here. However, this could cause problems with our locking mechanisms,
  // as other display-coordinator clients could be accessing the same functions and/or state at the
  // same time as the DisplayCompositor without making use of locks.
  DisplayCompositor(
      async_dispatcher_t* main_dispatcher,
      std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> display_coordinator,
      const std::shared_ptr<Renderer>& renderer, fuchsia::sysmem::AllocatorSyncPtr sysmem_allocator,
      bool enable_display_composition);

  ~DisplayCompositor() override;

  // |BufferCollectionImporter|
  // Only called from the main thread.
  bool ImportBufferCollection(allocation::GlobalBufferCollectionId collection_id,
                              fuchsia::sysmem::Allocator_Sync* sysmem_allocator,
                              fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken> token,
                              BufferCollectionUsage usage,
                              std::optional<fuchsia::math::SizeU> size) override
      FXL_LOCKS_EXCLUDED(lock_);

  // |BufferCollectionImporter|
  // Only called from the main thread.
  void ReleaseBufferCollection(allocation::GlobalBufferCollectionId collection_id,
                               BufferCollectionUsage usage_type) override FXL_LOCKS_EXCLUDED(lock_);

  // |BufferCollectionImporter|
  // Called from main thread or Flatland threads.
  bool ImportBufferImage(const allocation::ImageMetadata& metadata,
                         BufferCollectionUsage usage_type) override FXL_LOCKS_EXCLUDED(lock_);

  // |BufferCollectionImporter|
  // Called from main thread or Flatland threads.
  void ReleaseBufferImage(allocation::GlobalImageId image_id) override FXL_LOCKS_EXCLUDED(lock_);

  // Generates frame and presents it to display.  This may involve directly scanning out client
  // images, or it may involve first using the GPU to composite (some of) these images into a single
  // image which is then scanned out.
  //
  // |args| can be used to customize behavior in tests.  Production code should omit this arg; the
  // default values are correct for production use cases.
  //
  // Only called from the main thread.
  RenderFrameResult RenderFrame(
      uint64_t frame_number, zx::time presentation_time,
      const std::vector<RenderData>& render_data_list, std::vector<zx::event> release_fences,
      scheduling::FramePresentedCallback callback,
      // Allows customization of behavior for tests.  Default values are used in production.
      RenderFrameTestArgs test_args = RenderFrameTestArgs::Default()) FXL_LOCKS_EXCLUDED(lock_);

  // Register a new display to the DisplayCompositor, which also generates the render targets to be
  // presented on the display when compositing on the GPU. If |num_render_targets| is 0, this
  // function will not create any render targets for GPU composition for that display. The buffer
  // collection info is also returned back to the caller via an output parameter
  // |out_collection_info|. This out parameter is only allowed to be nullptr when
  // |num_render_targets| is 0. Otherwise, a valid handle to return the buffer collection data is
  // required.
  // TODO(fxbug.dev/59646): We need to figure out exactly how we want the display to anchor
  // to the Flatland hierarchy.
  // Only called from the main thread.
  void AddDisplay(scenic_impl::display::Display* display, DisplayInfo info,
                  uint32_t num_render_targets,
                  fuchsia::sysmem::BufferCollectionInfo_2* out_collection_info)
      FXL_LOCKS_EXCLUDED(lock_);

  // Values needed to adjust the color of the framebuffer as a postprocessing effect.
  // Only called from the main thread.
  void SetColorConversionValues(const std::array<float, 9>& coefficients,
                                const std::array<float, 3>& preoffsets,
                                const std::array<float, 3>& postoffsets);

  // Clamps the minimum value for all channels on all pixels on the display to this number.
  // Only called from the main thread.
  bool SetMinimumRgb(uint8_t minimum_rgb) FXL_LOCKS_EXCLUDED(lock_);

 private:
  friend class test::DisplayCompositorSmokeTest;
  friend class test::DisplayCompositorPixelTest;
  friend class test::DisplayCompositorTest;

  struct DisplayConfigResponse {
    // Whether or not the config can be successfully applied or not.
    fuchsia::hardware::display::ConfigResult result;
    // If the config is invalid, this vector will list all the operations
    // that need to be performed to make the config valid again.
    std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
  };

  struct FrameEventData {
    scenic_impl::DisplayEventId wait_id;
    scenic_impl::DisplayEventId signal_id;
    zx::event wait_event;
    zx::event signal_event;
  };

  struct ImageEventData {
    scenic_impl::DisplayEventId signal_id;
    zx::event signal_event;
  };

  struct DisplayEngineData {
    // The hardware layers we've created to use on this display.
    std::vector<uint64_t> layers;

    // The number of vmos we are using in the case of software composition
    // (1 for each render target).
    uint32_t vmo_count = 0;

    // The current target that is being rendererd to by the software renderer.
    uint32_t curr_vmo = 0;

    // The information used to create images for each render target from the vmo data.
    std::vector<allocation::ImageMetadata> render_targets;

    // The information used to create images for each render target from the vmo data.
    std::vector<allocation::ImageMetadata> protected_render_targets;

    // Used to synchronize buffer rendering with setting the buffer on the display.
    std::vector<FrameEventData> frame_event_datas;
  };

  // Notifies the compositor that a vsync has occurred, in response to a display configuration
  // applied by the compositor.  It is the compositor's responsibility to signal any release fences
  // corresponding to the frame identified by |frame_number|.
  void OnVsync(zx::time timestamp, fuchsia::hardware::display::ConfigStamp applied_config_stamp);

  std::vector<allocation::ImageMetadata> AllocateDisplayRenderTargets(
      bool use_protected_memory, uint32_t num_render_targets, const fuchsia::math::SizeU& size,
      fuchsia_images2::PixelFormat pixel_format,
      fuchsia::sysmem::BufferCollectionInfo_2* out_collection_info) FXL_LOCKS_EXCLUDED(lock_);

  // Generates a new FrameEventData struct to be used with a render target on a display.
  FrameEventData NewFrameEventData() FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Generates a new ImageEventData struct to be used with a client image on a display.
  ImageEventData NewImageEventData() FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  fuchsia::hardware::display::ImageConfig CreateImageConfig(
      const allocation::ImageMetadata& metadata) const FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Generates a hardware layer for direct compositing on the display. Returns the ID used
  // to reference that layer in the display coordinator API.
  uint64_t CreateDisplayLayer() FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Moves a token out of |display_buffer_collection_ptrs_| and returns it.
  fuchsia::sysmem::BufferCollectionSyncPtr TakeDisplayBufferCollectionPtr(
      allocation::GlobalBufferCollectionId collection_id) FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Used when we're forced to fall back to GPU rendering.
  bool PerformGpuComposition(uint64_t frame_number, zx::time presentation_time,
                             const std::vector<RenderData>& render_data_list,
                             std::vector<zx::event> release_fences,
                             scheduling::FramePresentedCallback callback)
      FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Does all the setup for applying the render data, which includes images and rectangles,
  // onto the display via the display coordinator interface. Returns false if this cannot
  // be completed.
  bool SetRenderDataOnDisplay(const RenderData& data) FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Calls SetRenderData for each item in |render_data_list| and applies direct-to-display color
  // conversion. Return false if this fails for any RenderData.
  bool SetRenderDatasOnDisplay(const std::vector<RenderData>& render_data_list)
      FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Sets the provided layers onto the display referenced by the given display_id.
  void SetDisplayLayers(uint64_t display_id, const std::vector<uint64_t>& layers)
      FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Takes a solid color rectangle and directly composites it to a hardware layer on the display.
  void ApplyLayerColor(uint64_t layer_id, ImageRect rectangle, allocation::ImageMetadata image)
      FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Takes an image and directly composites it to a hardware layer on the display.
  void ApplyLayerImage(uint64_t layer_id, ImageRect rectangle, allocation::ImageMetadata image,
                       scenic_impl::DisplayEventId wait_id, scenic_impl::DisplayEventId signal_id)
      FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Checks if the display coordinator is capable of applying the configuration settings that
  // have been set up until that point.
  bool CheckConfig() FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Erases the configuration that has been set on the display coordinator.
  void DiscardConfig() FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // Applies the config to the display coordinator and returns the ConfigStamp associated with this
  // config. ConfigStamp is provided by the display coordinator. This should only be called after
  // CheckConfig has verified that the config is okay, since ApplyConfig does not return any errors.
  fuchsia::hardware::display::ConfigStamp ApplyConfig() FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  bool ImportBufferCollectionToDisplayCoordinator(
      allocation::GlobalBufferCollectionId identifier,
      fuchsia::sysmem::BufferCollectionTokenSyncPtr token,
      const fuchsia::hardware::display::ImageConfig& image_config)
      FXL_EXCLUSIVE_LOCKS_REQUIRED(lock_);

  // This mutex protects access to class members that are accessed on main thread and the Flatland
  // threads. All the methods of this class are run of |main_dispatcher_| except for
  // ImportBufferImage() and ReleaseBufferImage(), where the shared data structures are guarded by
  // this.
  //
  // TODO(fxbug.dev/44335): Convert this to a lock-free structure. This is a unique
  // case since we are talking to a FIDL interface (display_coordinator_) through a lock.
  // We either need lock-free threadsafe FIDL bindings, multiple channels to the display
  // coordinator, or something else.
  mutable std::mutex lock_;

  // Handle to the display coordinator interface.
  std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> display_coordinator_
      FXL_GUARDED_BY(lock_);

  // Maps the flatland global image id to the events used by the display coordinator.
  std::unordered_map<allocation::GlobalImageId, ImageEventData> image_event_map_
      FXL_GUARDED_BY(lock_);

  // Maps a buffer collection ID to a BufferCollectionSyncPtr in the same domain as the token with
  // display constraints set. This is used as a bridge between ImportBufferCollection() and
  // ImportBufferImage() calls, so that we can check if the existing allocation is
  // display-compatible.
  std::unordered_map<allocation::GlobalBufferCollectionId, fuchsia::sysmem::BufferCollectionSyncPtr>
      display_buffer_collection_ptrs_ FXL_GUARDED_BY(lock_);

  // Maps a buffer collection ID to a boolean indicating if it can be imported into display.
  std::unordered_map<allocation::GlobalBufferCollectionId, bool> buffer_collection_supports_display_
      FXL_GUARDED_BY(lock_);

  // Maps a buffer collection ID to a collection pixel format struct.
  // TODO(fxbug.dev/71344): Delete after we don't need the pixel format anymore.
  std::unordered_map<allocation::GlobalBufferCollectionId, fuchsia::sysmem::PixelFormat>
      buffer_collection_pixel_format_ FXL_GUARDED_BY(lock_);

  /// The below members are either thread-safe or only manipulated from the main thread and
  /// therefore don't need locks.

  // Software renderer used when render data cannot be directly composited to the display.
  const std::shared_ptr<Renderer> renderer_;

  // Pending images in the current config that hasn't been applied yet.
  std::vector<allocation::GlobalImageId> pending_images_in_config_;

  // Maps a display ID to the the DisplayInfo struct. This is kept separate from the
  // display_DisplayCompositor_data_map_ since this only this data is needed for the
  // render_data_func_.
  std::unordered_map<uint64_t, DisplayInfo> display_info_map_;

  // Maps a display ID to a struct of all the information needed to properly render to
  // that display in both the hardware and software composition paths.
  std::unordered_map<uint64_t, DisplayEngineData> display_engine_data_map_;

  ReleaseFenceManager release_fence_manager_;

  // Stores information about the last ApplyConfig() call to display.
  struct ApplyConfigInfo {
    fuchsia::hardware::display::ConfigStamp config_stamp;
    uint64_t frame_number;
  };

  // A queue storing all display frame configurations that are applied but not yet shown on the
  // display device.
  std::deque<ApplyConfigInfo> pending_apply_configs_;

  // Stores the ConfigStamp information of the latest frame shown on the display. If no frame
  // has been presented, its value will be nullopt.
  std::optional<fuchsia::hardware::display::ConfigStamp> last_presented_config_stamp_ =
      std::nullopt;

  fuchsia::sysmem::AllocatorSyncPtr sysmem_allocator_;

  // Whether to attempt display composition at all. If false we always fall back to GPU-compositing.
  // Constant except for in tests.
  bool enable_display_composition_ = true;

  ColorConversionStateMachine cc_state_machine_;

  const async_dispatcher_t* const main_dispatcher_;
};

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_DISPLAY_COMPOSITOR_H_
