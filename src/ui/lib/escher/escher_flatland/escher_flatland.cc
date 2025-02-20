// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "./escher_flatland.h"

#include <fidl/fuchsia.element/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.views/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>

#include "src/ui/lib/escher/debug/debug_font.h"
#include "src/ui/lib/escher/escher.h"
#include "src/ui/lib/escher/fs/hack_filesystem.h"
#include "src/ui/lib/escher/renderer/batch_gpu_uploader.h"
#include "src/ui/lib/escher/renderer/semaphore.h"
#include "src/ui/lib/escher/vk/image.h"
#include "src/ui/lib/escher/vk/vulkan_device_queues.h"
#include "src/ui/lib/escher/vk/vulkan_instance.h"
#include "src/ui/lib/escher/vk/vulkan_swapchain.h"
#include "src/ui/lib/escher/vk/vulkan_swapchain_helper.h"
#include "vulkan/vulkan_core.h"

#include "vulkan/vulkan_enums.hpp"
#include "vulkan/vulkan_structs.hpp"

namespace escher {

// We might want to detect changes in the view size, and recreate the swapchain when it happens.
constexpr vk::Extent2D kExtent = vk::Extent2D(800, 600);

VulkanInstance::Params GetVulkanInstanceParams() {
  VulkanInstance::Params instance_params{
      {"VK_LAYER_FUCHSIA_imagepipe_swapchain"},
      {VK_EXT_DEBUG_UTILS_EXTENSION_NAME, VK_FUCHSIA_IMAGEPIPE_SURFACE_EXTENSION_NAME,
       VK_KHR_EXTERNAL_MEMORY_CAPABILITIES_EXTENSION_NAME,
       VK_KHR_EXTERNAL_SEMAPHORE_CAPABILITIES_EXTENSION_NAME, VK_KHR_SURFACE_EXTENSION_NAME,
       VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME},
  };

  auto validation_layer_name = VulkanInstance::GetValidationLayerName();
  if (validation_layer_name) {
    instance_params.layer_names.insert(*validation_layer_name);
  }

  return instance_params;
}

vk::SurfaceKHR CreateSurface(const vk::Instance& instance,
                             fuchsia_ui_views::ViewCreationToken view_creation_token) {
  vk::ImagePipeSurfaceCreateInfoFUCHSIA create_info;
  FX_CHECK(view_creation_token.value());
  create_info.imagePipeHandle = view_creation_token.value().release();
  vk::SurfaceKHR surface;

  auto result = instance.createImagePipeSurfaceFUCHSIA(&create_info, nullptr, &surface);
  FX_CHECK(result == vk::Result::eSuccess);

  return surface;
}

VulkanSwapchain CreateSwapchain(Escher& escher, vk::SurfaceKHR surface,
                                vk::SwapchainKHR old_swapchain) {
  vk::PhysicalDevice physical_device = escher.vk_physical_device();
  vk::Device device = escher.vk_device();

  constexpr uint32_t kDesiredNumOfSwapchainImages = 2;
  constexpr vk::Format kFormat = vk::Format::eB8G8R8A8Unorm;
  constexpr vk::ColorSpaceKHR kColorSpace = vk::ColorSpaceKHR::eSrgbNonlinear;

  constexpr vk::SurfaceTransformFlagBitsKHR kPreTransform =
      vk::SurfaceTransformFlagBitsKHR::eIdentity;

  // FIFO is always available, and is good enough for this example.
  constexpr vk::PresentModeKHR kPresentMode = vk::PresentModeKHR::eFifo;

  // We're not using any render passes, only blitting, therefore it is OK to omit eColorAttachment.
  constexpr vk::ImageUsageFlags kImageUsage = vk::ImageUsageFlagBits::eTransferDst;

  vk::SurfaceCapabilitiesKHR surface_caps;
  {
    auto result = physical_device.getSurfaceCapabilitiesKHR(surface, &surface_caps);
    FX_CHECK(result == vk::Result::eSuccess);
  }

  // TODO(https://fxbug.dev/345269489): VK_LAYER_FUCHSIA_imagepipe_swapchain only supports eOpaque.
  // For some use cases, we want to respect translucency in the presented image.
  const vk::CompositeAlphaFlagBitsKHR kCompositeAlpha =
      static_cast<bool>(surface_caps.supportedCompositeAlpha &
                        vk::CompositeAlphaFlagBitsKHR::ePostMultiplied)
          ? vk::CompositeAlphaFlagBitsKHR::ePostMultiplied
          : vk::CompositeAlphaFlagBitsKHR::eOpaque;

  // Verify that hard-coded surface format and color space are supported.
  {
    auto result = physical_device.getSurfaceFormatsKHR(surface);
    FX_CHECK(result.result == vk::Result::eSuccess);

    bool found_supported_format = false;
    for (vk::SurfaceFormatKHR& supported_format : result.value) {
      if (supported_format.format == kFormat && supported_format.colorSpace == kColorSpace) {
        found_supported_format = true;
      }
    }
    FX_CHECK(found_supported_format);
  }

  // Create swapchain.
  auto swapchain_return =
      device.createSwapchainKHR(vk::SwapchainCreateInfoKHR()
                                    .setSurface(surface)
                                    .setMinImageCount(kDesiredNumOfSwapchainImages)
                                    .setImageFormat(kFormat)
                                    .setImageColorSpace(kColorSpace)
                                    .setImageExtent(kExtent)
                                    .setImageArrayLayers(1)
                                    .setImageUsage(kImageUsage)
                                    .setImageSharingMode(vk::SharingMode::eExclusive)
                                    .setPreTransform(kPreTransform)
                                    .setCompositeAlpha(kCompositeAlpha)
                                    .setPresentMode(kPresentMode)
                                    .setClipped(true)
                                    .setOldSwapchain(old_swapchain));

  if (old_swapchain) {
    // Note: destroying the swapchain also cleans up all its associated
    // presentable images once the platform is done with them.
    device.destroySwapchainKHR(old_swapchain);
  }

  FX_CHECK(swapchain_return.result == vk::Result::eSuccess)
      << vk::to_string(swapchain_return.result);

  // Obtain swapchain images and buffers.
  auto result = device.getSwapchainImagesKHR(swapchain_return.value);
  FX_CHECK(result.result == vk::Result::eSuccess);

  std::vector<vk::Image> images(std::move(result.value));
  std::vector<ImagePtr> escher_images;
  escher_images.reserve(images.size());
  for (auto& im : images) {
    ImageInfo image_info;
    image_info.format = kFormat;
    image_info.width = kExtent.width;
    image_info.height = kExtent.height;
    image_info.usage = kImageUsage;

    auto escher_image =
        Image::WrapVkImage(escher.resource_recycler(), image_info, im, vk::ImageLayout::eUndefined);
    FX_CHECK(escher_image);
    escher_images.push_back(escher_image);
  }

  return VulkanSwapchain(swapchain_return.value, escher_images, kExtent.width, kExtent.height,
                         kFormat, kColorSpace);
}

void escher::EscherFlatland::RenderFrame(RenderFrameFn render_frame) {
  static uint64_t frame_number = 1;
  swapchain_helper_->DrawFrame([&](const escher::ImagePtr& output_image,
                                   const escher::SemaphorePtr& framebuffer_acquired,
                                   const escher::SemaphorePtr& render_finished) {
    auto frame = escher_->NewFrame("escher-flatland", frame_number++, false);

    frame->cmds()->AddWaitSemaphore(framebuffer_acquired, vk::PipelineStageFlagBits::eTransfer);

    // It is OK (and possibly more efficient) to transition from eUndefined, since we will be
    // clearing the whole image anyway.
    frame->cmds()->TransitionImageLayout(output_image, vk::ImageLayout::eUndefined,
                                         vk::ImageLayout::eTransferDstOptimal);
    frame->cmds()->vk().clearColorImage(
        output_image->vk(), vk::ImageLayout::eTransferDstOptimal,
        vk::ClearColorValue(std::array<float, 4>{0.f, 0.f, 0.f, 0.f}),
        {vk::ImageSubresourceRange(vk::ImageAspectFlagBits::eColor, 0, 1, 0, 1)});

    render_frame(output_image, frame);

    frame->cmds()->TransitionImageLayout(output_image, vk::ImageLayout::eTransferDstOptimal,
                                         vk::ImageLayout::ePresentSrcKHR);

    frame->EndFrame(render_finished, []() {});
  });

  async::PostTask(async_get_default_dispatcher(), [&] { RenderFrame(render_frame); });
}

EscherFlatland::EscherFlatland() {
  auto instance = VulkanInstance::New(GetVulkanInstanceParams());
  FX_CHECK(instance);

  // Create a channel, and put the endpoints into a view token and a view-creation token.
  fuchsia_ui_views::ViewCreationToken view_creation_token;
  fuchsia_ui_views::ViewportCreationToken viewport_creation_token;
  {
    zx::channel view_creation_channel, viewport_creation_channel;
    zx::channel::create(0, &view_creation_channel, &viewport_creation_channel);
    FX_CHECK(view_creation_channel && viewport_creation_channel);
    view_creation_token.value(std::move(view_creation_channel));
    viewport_creation_token.value(std::move(viewport_creation_channel));
  }

  auto surface = CreateSurface(instance->vk_instance(), std::move(view_creation_token));

  auto device = VulkanDeviceQueues::New(
      instance,
      {{VK_FUCHSIA_EXTERNAL_MEMORY_EXTENSION_NAME, VK_KHR_MAINTENANCE1_EXTENSION_NAME,
        VK_KHR_BIND_MEMORY_2_EXTENSION_NAME, VK_KHR_SAMPLER_YCBCR_CONVERSION_EXTENSION_NAME},
       {},
       std::move(surface)});

  auto filesystem = HackFilesystem::New();
  escher_ = std::make_unique<Escher>(device, filesystem, nullptr);

  auto swapchain = CreateSwapchain(*escher_, surface, vk::SwapchainKHR());
  swapchain_helper_ = std::make_unique<VulkanSwapchainHelper>(swapchain, device->vk_device(),
                                                              device->vk_main_queue());

  zx::result presenter_client_end = component::Connect<fuchsia_element::GraphicalPresenter>();

  FX_CHECK(presenter_client_end.is_ok());
  fidl::SyncClient presenter_client{std::move(*presenter_client_end)};

  // Connect the swapchain view to the graphical presenter.
  {
    fuchsia_element::ViewSpec spec;
    spec.viewport_creation_token(std::move(viewport_creation_token));

    auto controller_endpoints = fidl::CreateEndpoints<fuchsia_element::ViewController>();
    FX_CHECK(controller_endpoints.is_ok());

    fidl::Result result = presenter_client->PresentView(
        {{.view_spec = std::move(spec),
          .view_controller_request = std::move(controller_endpoints->server)}});
    FX_CHECK(result.is_ok()) << "error value: " << result.error_value();
  }

  {
    auto frame = escher_->NewFrame("escher_flatland-init", 1, false);
    auto gpu_uploader =
        std::make_shared<BatchGpuUploader>(escher_->GetWeakPtr(), frame->frame_number());

    debug_font_ = DebugFont::New(gpu_uploader.get(), escher_->image_cache());
    gpu_uploader->Submit();

    frame->EndFrame(SemaphorePtr(), []() {});

    // Normally we don't want to wait synchronously for the GPU to finish work,
    // but this is just once at init time.
    auto result = escher_->vk_device().waitIdle();
    FX_CHECK(result == vk::Result::eSuccess);
  }
}

}  // namespace escher
