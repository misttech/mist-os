// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_LIB_ESCHER_ESCHER_FLATLAND_ESCHER_FLATLAND_H_
#define SRC_UI_LIB_ESCHER_ESCHER_FLATLAND_ESCHER_FLATLAND_H_

#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.views/cpp/fidl.h>

#include "src/ui/lib/escher/debug/debug_font.h"
#include "src/ui/lib/escher/escher.h"
#include "src/ui/lib/escher/vk/vulkan_instance.h"
#include "src/ui/lib/escher/vk/vulkan_swapchain_helper.h"

namespace escher {

using RenderFrameFn = std::function<void(const escher::ImagePtr& /*output_image*/,
                                         const vk::Extent2D, const escher::FramePtr&)>;

VulkanInstance::Params GetVulkanInstanceParams();
vk::SurfaceKHR CreateSurface(const vk::Instance& instance,
                             fuchsia_ui_views::ViewCreationToken view_creation_token);
VulkanSwapchain CreateSwapchain(Escher& escher, vk::SurfaceKHR surface, vk::Extent2D extent,
                                vk::SwapchainKHR old_swapchain);

// EscherFlatland is the main class in the escher_flatland library.
// It manages:
// - the lifecycles of the Vulkan instance, logical device, and swapchain
// - the Escher instance lifecycle
// - using the above to a render a frame.
class EscherFlatland {
 public:
  // Connects to mandatory resources on initialization, including getting the
  // size provided by the parent viewport watcher. It does not watch for future
  // size changes.
  EscherFlatland(async_dispatcher_t* dispatcher);

  // Renders a frame using the `render_frame_fn`.
  //
  // The target user is not an expert in Vulkan programming (it is a
  // notoriously complicated API). `RenderFrame()` makes assumptions that work
  // for existing clients, such as those that render text via `DebugFont`.
  // However, you'll run into problems if want to do something fancier like
  // rasterizing triangle meshes. But if you know how to do that, you probably
  // don't want to use this class anyway, except perhaps as a reference.
  void RenderFrame(RenderFrameFn render_frame);

  // Renders a frame using the `render_frame_fn`, and then immediately
  // schedules another frame to be rendered using the default async dispatcher.
  void RenderLoop(RenderFrameFn render_frame);

  // Presents one of two default transforms:
  // * if `is_visible`, the Transform associated with Content containing a
  //   rendered output image
  // * otherwise, the Transform associated with no Content
  void SetVisible(bool is_visible);

  DebugFont debug_font() { return *debug_font_.get(); };

 private:
  async_dispatcher_t* dispatcher_;
  std::unique_ptr<Escher> escher_;
  std::unique_ptr<DebugFont> debug_font_;
  fidl::SyncClient<fuchsia_ui_composition::Flatland> flatland_;
  vk::Extent2D image_extent_;
  fidl::WireClient<fuchsia_ui_composition::ParentViewportWatcher> parent_viewport_watcher_;
  std::unique_ptr<VulkanSwapchainHelper> swapchain_helper_;
};

}  // namespace escher

#endif  // SRC_UI_LIB_ESCHER_ESCHER_FLATLAND_ESCHER_FLATLAND_H_
