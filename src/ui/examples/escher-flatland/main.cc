// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.element/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/fidl.h>
#include <fidl/fuchsia.ui.views/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include <chrono>
#include <cstdint>
#include <ctime>
#include <sstream>

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

// We might want to detect changes in the view size, and recreate the swapchain when it happens.
constexpr vk::Extent2D kExtent = vk::Extent2D(800, 600);

escher::VulkanInstance::Params GetVulkanInstanceParams() {
  escher::VulkanInstance::Params instance_params{
      {"VK_LAYER_FUCHSIA_imagepipe_swapchain"},
      {VK_EXT_DEBUG_UTILS_EXTENSION_NAME, VK_FUCHSIA_IMAGEPIPE_SURFACE_EXTENSION_NAME,
       VK_KHR_EXTERNAL_MEMORY_CAPABILITIES_EXTENSION_NAME,
       VK_KHR_EXTERNAL_SEMAPHORE_CAPABILITIES_EXTENSION_NAME, VK_KHR_SURFACE_EXTENSION_NAME,
       VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME},
  };

  auto validation_layer_name = escher::VulkanInstance::GetValidationLayerName();
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

escher::VulkanSwapchain CreateSwapchain(escher::Escher& escher, vk::SurfaceKHR surface,
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
  std::vector<escher::ImagePtr> escher_images;
  escher_images.reserve(images.size());
  for (auto& im : images) {
    escher::ImageInfo image_info;
    image_info.format = kFormat;
    image_info.width = kExtent.width;
    image_info.height = kExtent.height;
    image_info.usage = kImageUsage;

    auto escher_image = escher::Image::WrapVkImage(escher.resource_recycler(), image_info, im,
                                                   vk::ImageLayout::eUndefined);
    FX_CHECK(escher_image);
    escher_images.push_back(escher_image);
  }

  return escher::VulkanSwapchain(swapchain_return.value, escher_images, kExtent.width,
                                 kExtent.height, kFormat, kColorSpace);
}

std::string GetCurrentTimeString(void) {
  // TODO(https://fxbug.dev/344700148): This clock is broken, and always returns 21:10:26.
  // In the interim, consider replacing the following code with native Zircon calls.
  std::time_t now = std::chrono::system_clock::to_time_t(std::chrono::system_clock::now());
  std::tm* time_struct = std::localtime(&now);
  std::ostringstream oss;
  oss << time_struct->tm_hour << ":" << time_struct->tm_min << ":" << time_struct->tm_sec;
  return oss.str();
}

void RenderFrame(escher::Escher& escher, escher::VulkanSwapchainHelper& swapchain_helper,
                 escher::DebugFont& debug_font) {
  static uint64_t frame_number = 1;
  static int32_t dir_x = 3;
  static int32_t dir_y = 1;
  static int32_t pos_x = 0;
  static int32_t pos_y = 0;

  swapchain_helper.DrawFrame([&](const escher::ImagePtr& output_image,
                                 const escher::SemaphorePtr& framebuffer_acquired,
                                 const escher::SemaphorePtr& render_finished) {
    auto frame = escher.NewFrame("escher-flatland", frame_number++, false);

    frame->cmds()->AddWaitSemaphore(framebuffer_acquired, vk::PipelineStageFlagBits::eTransfer);

    // It is OK (and possibly more efficient) to transition from eUndefined, since we will be
    // clearing the whole image anyway.
    frame->cmds()->TransitionImageLayout(output_image, vk::ImageLayout::eUndefined,
                                         vk::ImageLayout::eTransferDstOptimal);
    frame->cmds()->vk().clearColorImage(
        output_image->vk(), vk::ImageLayout::eTransferDstOptimal,
        vk::ClearColorValue(std::array<float, 4>{0.f, 0.f, 0.f, 0.f}),
        {vk::ImageSubresourceRange(vk::ImageAspectFlagBits::eColor, 0, 1, 0, 1)});

    auto time_string = GetCurrentTimeString();

    constexpr int32_t kScale = 4;
    const int32_t kWidth =
        static_cast<int32_t>(time_string.length()) * escher::DebugFont::kGlyphWidth * kScale;
    constexpr int32_t kHeight = escher::DebugFont::kGlyphHeight * kScale;

    if (dir_x < 0 && pos_x <= 0)
      dir_x *= -1;
    if (dir_y < 0 && pos_y <= 0)
      dir_y *= -1;
    if (dir_x > 0 && (pos_x + kWidth) >= static_cast<int32_t>(kExtent.width))
      dir_x *= -1;
    if (dir_y > 0 && (pos_y + kHeight) >= static_cast<int32_t>(kExtent.height))
      dir_y *= -1;

    pos_x += dir_x;
    pos_y += dir_y;

    debug_font.Blit(frame->cmds(), time_string.c_str(), output_image, {pos_x, pos_y}, 4);

    frame->cmds()->TransitionImageLayout(output_image, vk::ImageLayout::eTransferDstOptimal,
                                         vk::ImageLayout::ePresentSrcKHR);

    frame->EndFrame(render_finished, []() {});
  });

  async::PostTask(async_get_default_dispatcher(),
                  [&] { RenderFrame(escher, swapchain_helper, debug_font); });
}

int main(int argc, char** argv) {
  auto instance = escher::VulkanInstance::New(GetVulkanInstanceParams());
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

  auto device = escher::VulkanDeviceQueues::New(
      instance,
      {{VK_FUCHSIA_EXTERNAL_MEMORY_EXTENSION_NAME, VK_KHR_MAINTENANCE1_EXTENSION_NAME,
        VK_KHR_BIND_MEMORY_2_EXTENSION_NAME, VK_KHR_SAMPLER_YCBCR_CONVERSION_EXTENSION_NAME},
       {},
       std::move(surface)});

  auto filesystem = escher::HackFilesystem::New();
  auto escher = std::make_unique<escher::Escher>(device, filesystem, nullptr);

  auto swapchain = CreateSwapchain(*escher, surface, vk::SwapchainKHR());
  auto swapchain_helper = std::make_unique<escher::VulkanSwapchainHelper>(
      swapchain, device->vk_device(), device->vk_main_queue());

  zx::result presenter_client_end = component::Connect<fuchsia_element::GraphicalPresenter>();

  FX_CHECK(presenter_client_end.is_ok());
  fidl::SyncClient presenter_client{std::move(*presenter_client_end)};

  // Connect the swapchain view to the graphical presenter.
  {
    fuchsia_element::ViewSpec spec;
    spec.viewport_creation_token(std::move(viewport_creation_token));

    fidl::Result result = presenter_client->PresentView({{.view_spec = std::move(spec)}});
    FX_CHECK(result.is_ok()) << "error value: " << result.error_value();
  }

  std::unique_ptr<escher::DebugFont> debug_font;
  {
    auto frame = escher->NewFrame("escher-flatland-init", 1, false);
    auto gpu_uploader =
        std::make_shared<escher::BatchGpuUploader>(escher->GetWeakPtr(), frame->frame_number());

    debug_font = escher::DebugFont::New(gpu_uploader.get(), escher->image_cache());
    gpu_uploader->Submit();

    frame->EndFrame(escher::SemaphorePtr(), []() {});

    // Normally we don't want to wait synchronously for the GPU to finish work,
    // but this is just once at init time.
    auto result = escher->vk_device().waitIdle();
    FX_CHECK(result == vk::Result::eSuccess);
  }

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  trace::TraceProviderWithFdio trace_provider(loop.dispatcher());

  async::PostTask(loop.dispatcher(),
                  [&] { RenderFrame(*escher.get(), *swapchain_helper.get(), *debug_font.get()); });

  loop.Run();

  return 0;
}
