// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_VULKAN_SWAPCHAIN_IMAGE_PIPE_SURFACE_DISPLAY_H_
#define SRC_LIB_VULKAN_SWAPCHAIN_IMAGE_PIPE_SURFACE_DISPLAY_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <unordered_set>

#include "image_pipe_surface.h"

namespace image_pipe_swapchain {

// An implementation of ImagePipeSurface based on the display and sysmem APIs.
class ImagePipeSurfaceDisplay : public ImagePipeSurface {
 public:
  ImagePipeSurfaceDisplay();

  bool Init() override;

  bool CreateImage(VkDevice device, VkLayerDispatchTable* pDisp, VkFormat format,
                   VkImageUsageFlags usage, VkSwapchainCreateFlagsKHR swapchain_flags,
                   VkExtent2D extent, uint32_t image_count, const VkAllocationCallbacks* pAllocator,
                   std::vector<ImageInfo>* image_info_out) override;

  bool CanPresentPendingImage() override { return false; }

  bool GetSize(uint32_t* width_out, uint32_t* height_out) override;

  void RemoveImage(uint32_t image_id) override;

  void PresentImage(uint32_t image_id, std::vector<std::unique_ptr<PlatformEvent>> acquire_fences,
                    std::vector<std::unique_ptr<PlatformEvent>> release_fences,
                    VkQueue queue) override;

  SupportedImageProperties& GetSupportedImageProperties() override;

 private:
  void ControllerError(zx_status_t status);
  void ControllerOnDisplaysChanged(std::vector<fuchsia::hardware::display::Info>,
                                   std::vector<fuchsia::hardware::display::DisplayId>);

  bool WaitForAsyncMessage();

  // This loop is manually pumped in method calls and doesn't have its own
  // thread.
  async::Loop loop_;
  std::unordered_set<uint64_t> image_ids;

  bool display_connection_exited_ = false;
  bool got_message_response_ = false;
  bool have_display_ = false;
  uint32_t width_ = 0;
  uint32_t height_ = 0;
  fuchsia::hardware::display::DisplayId display_id_ = {
      .value = fuchsia::hardware::display::INVALID_DISP_ID};
  fuchsia::hardware::display::LayerId layer_id_{fuchsia::hardware::display::INVALID_DISP_ID};
  fuchsia::hardware::display::CoordinatorPtr display_coordinator_;
  fuchsia::sysmem::AllocatorSyncPtr sysmem_allocator_;
  SupportedImageProperties supported_image_properties_;
};

}  // namespace image_pipe_swapchain

#endif  // SRC_LIB_VULKAN_SWAPCHAIN_IMAGE_PIPE_SURFACE_DISPLAY_H_
