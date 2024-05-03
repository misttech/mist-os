// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "image_pipe_surface_display.h"

#include <dirent.h>
#include <errno.h>
#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <fuchsia/images2/cpp/fidl.h>
#include <fuchsia/sysmem2/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <vk_dispatch_table_helper.h>
#include <zircon/status.h>

#include <deque>

#include <fbl/unique_fd.h>
#include <vulkan/vk_layer.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/vulkan/swapchain/vulkan_utils.h"
#include "vulkan/vulkan_fuchsia.h"
#include "zircon/rights.h"

namespace image_pipe_swapchain {

namespace {

const char* const kTag = "ImagePipeSurfaceDisplay";

constexpr inline fuchsia::hardware::display::ImageId ToFidlImageId(uint32_t image_id) {
  return {.value = image_id};
}

}  // namespace

ImagePipeSurfaceDisplay::ImagePipeSurfaceDisplay()
    : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

// Attempt to connect to /svc/fuchsia.hardware.display.Provider (not the device
// node) in case that was injected for testing.
static fuchsia::hardware::display::CoordinatorPtr ConnectToCoordinatorFromService(
    async_dispatcher_t* dispatcher) {
  fuchsia::hardware::display::ProviderSyncPtr provider;
  zx_status_t status = fdio_service_connect("/svc/fuchsia.hardware.display.Provider",
                                            provider.NewRequest().TakeChannel().release());

  if (status != ZX_OK) {
    return {};
  }

  zx_status_t status2 = ZX_OK;
  fuchsia::hardware::display::CoordinatorPtr coordinator;
  status = provider->OpenCoordinatorForPrimary(coordinator.NewRequest(dispatcher), &status2);
  if (status != ZX_OK) {
    // If the path isn't injected the failure will happen at this point.
    return {};
  }

  if (status2 != ZX_OK) {
    fprintf(stderr, "Couldn't connect to display controller: %s\n", zx_status_get_string(status2));
    return {};
  }
  return coordinator;
}

bool ImagePipeSurfaceDisplay::Init() {
  zx_status_t status = fdio_service_connect("/svc/fuchsia.sysmem2.Allocator",
                                            sysmem_allocator_.NewRequest().TakeChannel().release());

  if (status != ZX_OK) {
    fprintf(stderr, "Couldn't connect to sysmem service\n");
    return false;
  }

  fuchsia::sysmem2::AllocatorSetDebugClientInfoRequest debug_client_request;
  debug_client_request.set_name(fsl::GetCurrentProcessName());
  debug_client_request.set_id(fsl::GetCurrentProcessKoid());
  sysmem_allocator_->SetDebugClientInfo(std::move(debug_client_request));

  display_coordinator_ = ConnectToCoordinatorFromService(loop_.dispatcher());
  if (!display_coordinator_) {
    // Probe /dev/class/display-coordinator/ for a display coordinator name.
    // When the display driver restarts it comes up with a new one (e.g. '001'
    // instead of '000'). For now, simply take the first file found in the
    // directory.
    const char kDir[] = "/dev/class/display-coordinator";
    std::string filename;

    {
      DIR* dir = opendir(kDir);
      if (!dir) {
        fprintf(stderr, "%s: Can't open directory: %s: %s\n", kTag, kDir, strerror(errno));
        return false;
      }

      errno = 0;
      for (;;) {
        dirent* entry = readdir(dir);
        if (!entry) {
          if (errno != 0) {
            // An error occurred while reading the directory.
            fprintf(stderr, "%s: Warning: error while reading %s: %s\n", kTag, kDir,
                    strerror(errno));
          }
          break;
        }
        // Skip over '.' and '..' if present.
        if (entry->d_name[0] == '.' &&
            (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, "..")))
          continue;

        filename = std::string(kDir) + "/" + entry->d_name;
        break;
      }
      closedir(dir);
    }

    if (filename.empty()) {
      fprintf(stderr, "%s: No display controller.\n", kTag);
      return false;
    }

    zx::result provider = fidl::CreateEndpoints<fuchsia_hardware_display::Provider>();
    if (provider.is_error()) {
      fprintf(stderr, "%s: Failed to create provider channel %d (%s)\n", kTag,
              provider.error_value(), provider.status_string());
    }

    // TODO(https://fxbug.dev/42064416): Use Component::Connect here when it's possible to use this
    // without depending on libsvc.so
    status = fdio_service_connect(filename.c_str(), provider->server.TakeChannel().release());
    if (status != ZX_OK) {
      fprintf(stderr, "%s: Could not open display coordinator: %s\n", kTag,
              zx_status_get_string(status));
      return false;
    }

    zx::result dc_endpoints = fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
    if (dc_endpoints.is_error()) {
      fprintf(stderr, "%s: Failed to create coordinator channel %d (%s)\n", kTag,
              dc_endpoints.error_value(), dc_endpoints.status_string());
      return false;
    }

    fidl::WireResult result = fidl::WireCall(provider->client)
                                  ->OpenCoordinatorForPrimary(std::move(dc_endpoints->server));
    if (!result.ok()) {
      fprintf(stderr, "%s: Failed to call service handle %d (%s)\n", kTag, result.status(),
              result.status_string());
      return false;
    }
    if (result->s != ZX_OK) {
      fprintf(stderr, "%s: Failed to open coordinator %d (%s)\n", kTag, result->s,
              zx_status_get_string(result->s));
      return false;
    }

    display_coordinator_.Bind(dc_endpoints->client.TakeChannel(), loop_.dispatcher());
  }

  display_coordinator_.set_error_handler(
      fit::bind_member(this, &ImagePipeSurfaceDisplay::ControllerError));

  display_coordinator_.events().OnDisplaysChanged =
      fit::bind_member(this, &ImagePipeSurfaceDisplay::ControllerOnDisplaysChanged);
  while (!have_display_) {
    loop_.Run(zx::time::infinite(), true);
    if (display_connection_exited_)
      return false;
  }
  return true;
}

void ImagePipeSurfaceDisplay::ControllerError(zx_status_t status) {
  display_connection_exited_ = true;
}

bool ImagePipeSurfaceDisplay::WaitForAsyncMessage() {
  got_message_response_ = false;
  while (!got_message_response_ && !display_connection_exited_) {
    loop_.Run(zx::time::infinite(), true);
  }
  return !display_connection_exited_;
}

void ImagePipeSurfaceDisplay::ControllerOnDisplaysChanged(
    std::vector<fuchsia::hardware::display::Info> info,
    std::vector<fuchsia::hardware::display::types::DisplayId>) {
  if (info.size() == 0)
    return;
  width_ = info[0].modes[0].horizontal_resolution;
  height_ = info[0].modes[0].vertical_resolution;
  display_id_ = info[0].id;
  std::deque<VkSurfaceFormatKHR> formats;

  for (fuchsia::images2::PixelFormat pixel_format : info[0].pixel_format) {
    switch (pixel_format) {
      case fuchsia::images2::PixelFormat::B8G8R8A8:
        formats.push_back({VK_FORMAT_B8G8R8A8_UNORM, VK_COLORSPACE_SRGB_NONLINEAR_KHR});
        formats.push_back({VK_FORMAT_B8G8R8A8_SRGB, VK_COLORSPACE_SRGB_NONLINEAR_KHR});
        break;
      case fuchsia::images2::PixelFormat::R8G8B8A8:
        // Push front to prefer R8G8B8A8 formats.
        formats.push_front({VK_FORMAT_R8G8B8A8_SRGB, VK_COLORSPACE_SRGB_NONLINEAR_KHR});
        formats.push_front({VK_FORMAT_R8G8B8A8_UNORM, VK_COLORSPACE_SRGB_NONLINEAR_KHR});
        break;
      default:
        // Ignore unknown formats.
        break;
    }
  }
  if (formats.empty()) {
    fprintf(stderr, "OnDisplaysChanged: No pixel format available. Cannot use this display.\n");
    return;
  }
  supported_image_properties_ =
      SupportedImageProperties{.formats = {formats.begin(), formats.end()}};
  have_display_ = true;
}

bool ImagePipeSurfaceDisplay::CreateImage(VkDevice device, VkLayerDispatchTable* pDisp,
                                          VkFormat format, VkImageUsageFlags usage,
                                          VkSwapchainCreateFlagsKHR swapchain_flags,
                                          VkExtent2D extent, uint32_t image_count,
                                          const VkAllocationCallbacks* pAllocator,
                                          std::vector<ImageInfo>* image_info_out) {
  // To create BufferCollection, the image must have a valid format.
  if (format == VK_FORMAT_UNDEFINED) {
    fprintf(stderr, "%s: Invalid format: %d\n", kTag, format);
    return false;
  }

  VkResult result;
  fuchsia::sysmem2::BufferCollectionTokenSyncPtr local_token;
  fuchsia::sysmem2::AllocatorAllocateSharedCollectionRequest local_allocate_request;
  local_allocate_request.set_token_request(local_token.NewRequest());
  zx_status_t status =
      sysmem_allocator_->AllocateSharedCollection(std::move(local_allocate_request));
  if (status != ZX_OK) {
    fprintf(stderr, "%s: AllocateSharedCollection failed: %d\n", kTag, status);
    return false;
  }

  fuchsia::sysmem2::BufferCollectionTokenSyncPtr vulkan_token;
  fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest vulkan_duplicate_request;
  vulkan_duplicate_request.set_token_request(vulkan_token.NewRequest());
  vulkan_duplicate_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
  status = local_token->Duplicate(std::move(vulkan_duplicate_request));
  if (status != ZX_OK) {
    fprintf(stderr, "%s: Duplicate failed: %d\n", kTag, status);
    return false;
  }

  fuchsia::sysmem2::BufferCollectionTokenSyncPtr display_token;
  fuchsia::sysmem2::BufferCollectionTokenDuplicateRequest display_duplicate_request;
  vulkan_duplicate_request.set_token_request(display_token.NewRequest());
  vulkan_duplicate_request.set_rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
  status = local_token->Duplicate(std::move(display_duplicate_request));
  if (status != ZX_OK) {
    fprintf(stderr, "%s: Duplicate failed: %d\n", kTag, status);
    return false;
  }
  fuchsia::sysmem2::Node_Sync_Result sync_result;
  status = local_token->Sync(&sync_result);
  if (status != ZX_OK) {
    fprintf(stderr, "%s: Sync failed: %d\n", kTag, status);
    return false;
  }
  if (sync_result.is_framework_err()) {
    fprintf(stderr, "%s: Sync failed with framework_err\n", kTag);
    return false;
  }

  constexpr fuchsia::hardware::display::BufferCollectionId kBufferCollectionId = {.value = 1};

  display_coordinator_->ImportBufferCollection(
      kBufferCollectionId,
      fidl::InterfaceHandle<fuchsia::sysmem::BufferCollectionToken>(
          display_token.Unbind().TakeChannel()),
      [this,
       &status](fuchsia::hardware::display::Coordinator_ImportBufferCollection_Result result) {
        status = result.is_err() ? result.err() : ZX_OK;
        got_message_response_ = true;
      });
  if (!WaitForAsyncMessage()) {
    fprintf(stderr, "%s: Display Disconnected\n", kTag);
    return false;
  }
  if (status != ZX_OK) {
    fprintf(stderr, "%s: ImportBufferCollection failed: %d\n", kTag, status);
    return false;
  }

#if defined(__x86_64__)
  // Must be consistent with intel-gpu-core.h
  static constexpr uint32_t kImageTilingTypeXTiled = 1;
  static constexpr uint32_t kImageTilingType = kImageTilingTypeXTiled;
#elif defined(__aarch64__)
  static constexpr uint32_t kImageTilingType =
      fuchsia_hardware_display_types::wire::kImageTilingTypeLinear;
#else
  static constexpr uint32_t kImageTilingType =
      fuchsia_hardware_display_types::wire::kImageTilingTypeLinear;
  // Unsupported display.
  return false;
#endif

  const fuchsia::hardware::display::types::ImageBufferUsage image_buffer_usage = {
      .tiling_type = kImageTilingType,
  };
  const fuchsia::hardware::display::types::ImageMetadata image_metadata = {
      .width = extent.width,
      .height = extent.height,
      .tiling_type = image_buffer_usage.tiling_type,
  };

  display_coordinator_->SetBufferCollectionConstraints(
      kBufferCollectionId, image_buffer_usage,
      [this, &status](
          fuchsia::hardware::display::Coordinator_SetBufferCollectionConstraints_Result result) {
        status = result.is_err() ? result.err() : ZX_OK;
        got_message_response_ = true;
      });
  if (!WaitForAsyncMessage()) {
    fprintf(stderr, "%s: Display Disconnected\n", kTag);
    return false;
  }
  if (status != ZX_OK) {
    fprintf(stderr, "%s: SetBufferCollectionConstraints failed: %d\n", kTag, status);
    return false;
  }

  uint32_t image_flags = 0;
  if (swapchain_flags & VK_SWAPCHAIN_CREATE_MUTABLE_FORMAT_BIT_KHR)
    image_flags |= VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
  if (swapchain_flags & VK_SWAPCHAIN_CREATE_PROTECTED_BIT_KHR)
    image_flags |= VK_IMAGE_CREATE_PROTECTED_BIT;

  VkImageCreateInfo image_create_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
      .pNext = nullptr,
      .flags = image_flags,
      .imageType = VK_IMAGE_TYPE_2D,
      .format = format,
      .extent = VkExtent3D{extent.width, extent.height, 1},
      .mipLevels = 1,
      .arrayLayers = 1,
      .samples = VK_SAMPLE_COUNT_1_BIT,
      .tiling = VK_IMAGE_TILING_OPTIMAL,
      .usage = usage,
      .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
      .queueFamilyIndexCount = 0,      // not used since not sharing
      .pQueueFamilyIndices = nullptr,  // not used since not sharing
      .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
  };
  const VkSysmemColorSpaceFUCHSIA kSrgbColorSpace = {
      .sType = VK_STRUCTURE_TYPE_SYSMEM_COLOR_SPACE_FUCHSIA,
      .pNext = nullptr,
      .colorSpace = static_cast<uint32_t>(fuchsia::images2::ColorSpace::SRGB)};
  const VkSysmemColorSpaceFUCHSIA kYuvColorSpace = {
      .sType = VK_STRUCTURE_TYPE_SYSMEM_COLOR_SPACE_FUCHSIA,
      .pNext = nullptr,
      .colorSpace = static_cast<uint32_t>(fuchsia::images2::ColorSpace::REC709)};

  VkImageFormatConstraintsInfoFUCHSIA format_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_CONSTRAINTS_INFO_FUCHSIA,
      .pNext = nullptr,
      .imageCreateInfo = image_create_info,
      .requiredFormatFeatures = GetFormatFeatureFlagsFromUsage(usage),
      .sysmemPixelFormat = 0u,
      .colorSpaceCount = 1,
      .pColorSpaces = IsYuvFormat(format) ? &kYuvColorSpace : &kSrgbColorSpace,
  };
  VkImageConstraintsInfoFUCHSIA image_constraints_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_CONSTRAINTS_INFO_FUCHSIA,
      .pNext = nullptr,
      .formatConstraintsCount = 1,
      .pFormatConstraints = &format_info,
      .bufferCollectionConstraints =
          VkBufferCollectionConstraintsInfoFUCHSIA{
              .sType = VK_STRUCTURE_TYPE_BUFFER_COLLECTION_CONSTRAINTS_INFO_FUCHSIA,
              .pNext = nullptr,
              .minBufferCount = 1,
              .maxBufferCount = 0,
              .minBufferCountForCamping = 0,
              .minBufferCountForDedicatedSlack = 0,
              .minBufferCountForSharedSlack = 0,
          },
      .flags = 0u,
  };

  VkBufferCollectionCreateInfoFUCHSIA import_info = {
      .sType = VK_STRUCTURE_TYPE_BUFFER_COLLECTION_CREATE_INFO_FUCHSIA,
      .pNext = nullptr,
      .collectionToken = vulkan_token.Unbind().TakeChannel().release(),
  };
  VkBufferCollectionFUCHSIA collection;
  result = pDisp->CreateBufferCollectionFUCHSIA(device, &import_info, pAllocator, &collection);
  if (result != VK_SUCCESS) {
    fprintf(stderr, "%s: Failed to import buffer collection: %d\n", kTag, result);
    return false;
  }

  result = pDisp->SetBufferCollectionImageConstraintsFUCHSIA(device, collection,
                                                             &image_constraints_info);

  if (result != VK_SUCCESS) {
    fprintf(stderr, "%s: Failed to import buffer collection: %d\n", kTag, result);
    return false;
  }

  fuchsia::sysmem2::BufferCollectionSyncPtr sysmem_collection;
  fuchsia::sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.set_token(std::move(local_token));
  bind_shared_request.set_buffer_collection_request(sysmem_collection.NewRequest());
  status = sysmem_allocator_->BindSharedCollection(std::move(bind_shared_request));
  if (status != ZX_OK) {
    fprintf(stderr, "%s: BindSharedCollection failed: %d\n", kTag, status);
    return false;
  }
  // 1000 should override the generic Magma name.
  constexpr uint32_t kNamePriority = 1000u;
  const char* kImageName = "ImagePipeSurfaceDisplay";
  fuchsia::sysmem2::NodeSetNameRequest set_name_request;
  set_name_request.set_name(kImageName);
  set_name_request.set_priority(kNamePriority);
  sysmem_collection->SetName(std::move(set_name_request));
  fuchsia::sysmem2::BufferCollectionSetConstraintsRequest constraints_request;
  constraints_request.mutable_constraints()->set_min_buffer_count(image_count);
  // Used because every constraints need to have a usage.
  constraints_request.mutable_constraints()->mutable_usage()->set_display(
      fuchsia::sysmem2::DISPLAY_USAGE_LAYER);
  status = sysmem_collection->SetConstraints(std::move(constraints_request));
  if (status != ZX_OK) {
    fprintf(stderr, "%s: SetConstraints failed: %d\n", kTag, status);
    return false;
  }

  fuchsia::sysmem2::BufferCollection_WaitForAllBuffersAllocated_Result wait_for_buffers_result;
  status = sysmem_collection->WaitForAllBuffersAllocated(&wait_for_buffers_result);
  if (status != ZX_OK) {
    fprintf(stderr, "%s: WaitForBuffersAllocated failed: %d\n", kTag, status);
    return false;
  }
  if (wait_for_buffers_result.is_framework_err()) {
    fprintf(stderr, "%s: WaitForBuffersAllocated failed with framework_err\n", kTag);
    return false;
  }
  if (wait_for_buffers_result.is_err()) {
    fprintf(stderr, "%s: WaitForBuffersAllocated failed: %u\n", kTag,
            static_cast<uint32_t>(wait_for_buffers_result.err()));
    return false;
  }
  sysmem_collection->Release();

  auto& buffer_collection_info = wait_for_buffers_result.response().buffer_collection_info();
  if (buffer_collection_info.buffers().size() != image_count) {
    fprintf(stderr, "%s: incorrect image count %lu allocated vs. %d requested\n", kTag,
            buffer_collection_info.buffers().size(), image_count);
    return false;
  }

  for (uint32_t i = 0; i < image_count; ++i) {
    VkExternalMemoryImageCreateInfo external_image_create_info = {
        .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
        .pNext = nullptr,
        .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_ZIRCON_VMO_BIT_FUCHSIA,
    };
    VkBufferCollectionImageCreateInfoFUCHSIA image_format_fuchsia = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_COLLECTION_IMAGE_CREATE_INFO_FUCHSIA,
        .pNext = &external_image_create_info,
        .collection = collection,
        .index = i};
    image_create_info.pNext = &image_format_fuchsia;

    VkImage image;
    result = pDisp->CreateImage(device, &image_create_info, pAllocator, &image);
    if (result != VK_SUCCESS) {
      fprintf(stderr, "%s: vkCreateImage failed: %d\n", kTag, result);
      return false;
    }

    VkMemoryRequirements memory_requirements;
    pDisp->GetImageMemoryRequirements(device, image, &memory_requirements);

    VkBufferCollectionPropertiesFUCHSIA properties = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_COLLECTION_PROPERTIES_FUCHSIA};
    result = pDisp->GetBufferCollectionPropertiesFUCHSIA(device, collection, &properties);
    if (result != VK_SUCCESS) {
      fprintf(stderr, "%s: GetBufferCollectionPropertiesFUCHSIA failed: %d\n", kTag, status);
      return false;
    }

    // Find lowest usable index.
    uint32_t memory_type_index =
        __builtin_ctz(memory_requirements.memoryTypeBits & properties.memoryTypeBits);

    VkMemoryDedicatedAllocateInfoKHR dedicated_info = {
        .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO_KHR,
        .image = image,
    };
    VkImportMemoryBufferCollectionFUCHSIA import_info = {
        .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_BUFFER_COLLECTION_FUCHSIA,
        .pNext = &dedicated_info,
        .collection = collection,
        .index = i,
    };

    VkMemoryAllocateInfo alloc_info{
        .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .pNext = &import_info,
        .allocationSize = memory_requirements.size,
        .memoryTypeIndex = memory_type_index,
    };
    VkDeviceMemory memory;
    result = pDisp->AllocateMemory(device, &alloc_info, pAllocator, &memory);
    if (result != VK_SUCCESS) {
      fprintf(stderr, "%s: vkAllocateMemory failed: %d\n", kTag, result);
      return result;
    }
    result = pDisp->BindImageMemory(device, image, memory, 0);
    if (result != VK_SUCCESS) {
      fprintf(stderr, "%s: vkBindImageMemory failed: %d\n", kTag, result);
      return result;
    }

    uint32_t image_id = next_image_id();
    const fuchsia::hardware::display::ImageId fidl_image_id = ToFidlImageId(image_id);
    display_coordinator_->ImportImage(
        image_metadata, /*buffer_id=*/
        {
            .buffer_collection_id = kBufferCollectionId,
            .buffer_index = i,
        },
        fidl_image_id,
        [this, &status](fuchsia::hardware::display::Coordinator_ImportImage_Result result) {
          status = result.is_err() ? result.err() : ZX_OK;
          got_message_response_ = true;
        });

    if (!WaitForAsyncMessage()) {
      return false;
    }
    if (status != ZX_OK) {
      fprintf(stderr, "%s: ImportVmoImage failed: %d\n", kTag, status);
      return false;
    }

    ImageInfo info = {.image = image, .memory = memory, .image_id = image_id};

    image_info_out->push_back(info);

    image_ids.insert(image_id);
  }

  pDisp->DestroyBufferCollectionFUCHSIA(device, collection, pAllocator);
  display_coordinator_->ReleaseBufferCollection(kBufferCollectionId);

  display_coordinator_->CreateLayer(
      [this, &status](fuchsia::hardware::display::Coordinator_CreateLayer_Result result) {
        status = result.is_err() ? result.err() : ZX_OK;
        layer_id_ = result.is_response()
                        ? result.response().layer_id
                        : fuchsia::hardware::display::LayerId{
                              .value = fuchsia::hardware::display::types::INVALID_DISP_ID};
        got_message_response_ = true;
      });
  if (!WaitForAsyncMessage()) {
    return false;
  }
  if (status != ZX_OK) {
    fprintf(stderr, "%s: CreateLayer failed: %d\n", kTag, status);
    return false;
  }

  display_coordinator_->SetDisplayLayers(
      display_id_, std::vector<fuchsia::hardware::display::LayerId>{layer_id_});
  display_coordinator_->SetLayerPrimaryConfig(layer_id_, image_metadata);

  return true;
}

bool ImagePipeSurfaceDisplay::GetSize(uint32_t* width_out, uint32_t* height_out) {
  *width_out = width_;
  *height_out = height_;
  return true;
}

void ImagePipeSurfaceDisplay::RemoveImage(uint32_t image_id) {
  auto iter = image_ids.find(image_id);
  if (iter != image_ids.end()) {
    image_ids.erase(iter);
  }
}

void ImagePipeSurfaceDisplay::PresentImage(
    uint32_t image_id, std::vector<std::unique_ptr<PlatformEvent>> acquire_fences,
    std::vector<std::unique_ptr<PlatformEvent>> release_fences, VkQueue queue) {
  assert(acquire_fences.size() <= 1);
  assert(release_fences.size() <= 1);

  auto iter = image_ids.find(image_id);
  if (iter == image_ids.end()) {
    fprintf(stderr, "%s::PresentImage: can't find image_id %u\n", kTag, image_id);
    return;
  }

  fuchsia::hardware::display::EventId wait_event_id = {
      .value = fuchsia::hardware::display::types::INVALID_DISP_ID};
  if (acquire_fences.size()) {
    zx::event event = static_cast<FuchsiaEvent*>(acquire_fences[0].get())->Take();

    zx_info_handle_basic_t info;
    zx_status_t status =
        event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      fprintf(stderr, "%s: failed to get event id: %d\n", kTag, status);
      return;
    }
    wait_event_id = {.value = info.koid};
    display_coordinator_->ImportEvent(std::move(event), wait_event_id);
    if (status != ZX_OK) {
      fprintf(stderr, "%s: fb_import_event failed: %d\n", kTag, status);
      return;
    }
  }

  fuchsia::hardware::display::EventId signal_event_id = {
      .value = fuchsia::hardware::display::types::INVALID_DISP_ID};
  if (release_fences.size()) {
    zx::event event = static_cast<FuchsiaEvent*>(release_fences[0].get())->Take();

    zx_info_handle_basic_t info;
    zx_status_t status =
        event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      fprintf(stderr, "%s: failed to get event id: %d\n", kTag, status);
      return;
    }
    signal_event_id = {.value = info.koid};
    display_coordinator_->ImportEvent(std::move(event), signal_event_id);
    if (status != ZX_OK) {
      fprintf(stderr, "%s: fb_import_event failed: %d\n", kTag, status);
      return;
    }
  }

  // image_id is also used in DisplayController interface.
  const fuchsia::hardware::display::ImageId fidl_image_id = ToFidlImageId(image_id);
  display_coordinator_->SetLayerImage(layer_id_, fidl_image_id, wait_event_id, signal_event_id);
  display_coordinator_->ApplyConfig();

  if (wait_event_id.value != fuchsia::hardware::display::types::INVALID_DISP_ID) {
    display_coordinator_->ReleaseEvent(wait_event_id);
  }

  if (signal_event_id.value != fuchsia::hardware::display::types::INVALID_DISP_ID) {
    display_coordinator_->ReleaseEvent(signal_event_id);
  }
}

SupportedImageProperties& ImagePipeSurfaceDisplay::GetSupportedImageProperties() {
  return supported_image_properties_;
}

}  // namespace image_pipe_swapchain
