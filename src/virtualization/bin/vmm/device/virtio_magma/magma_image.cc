// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_image.h"

#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fidl/fuchsia.ui.composition/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/image-format/image_format.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/syslog/global.h>
#include <lib/zx/channel.h>

#include <cassert>

#include <src/lib/fsl/handles/object_info.h>

#include "drm_fourcc.h"

#include <vulkan/vulkan.hpp>

#define LOG_VERBOSE(msg, ...) \
  if (false)                  \
  FX_LOGF(INFO, "magma_image", msg, ##__VA_ARGS__)

namespace {

uint32_t to_uint32(uint64_t value) {
  assert(value <= std::numeric_limits<uint32_t>::max());
  return static_cast<uint32_t>(value);
}

uint64_t SysmemModifierToDrmModifier(fuchsia_images2::PixelFormatModifier modifier) {
  static_assert(DRM_FORMAT_MOD_LINEAR ==
                fidl::ToUnderlying(fuchsia_images2::wire::PixelFormatModifier::kLinear));
  static_assert(I915_FORMAT_MOD_X_TILED ==
                fidl::ToUnderlying(fuchsia_images2::wire::PixelFormatModifier::kIntelI915XTiled));
  static_assert(I915_FORMAT_MOD_Y_TILED ==
                fidl::ToUnderlying(fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiled));
  static_assert(I915_FORMAT_MOD_Yf_TILED ==
                fidl::ToUnderlying(fuchsia_images2::wire::PixelFormatModifier::kIntelI915YfTiled));
  switch (modifier) {
    case fuchsia_images2::wire::PixelFormatModifier::kLinear:
    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915XTiled:
    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiled:
    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915YfTiled:
      return fidl::ToUnderlying(modifier);
    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiledCcs:
      return I915_FORMAT_MOD_Y_TILED_CCS;
    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915YfTiledCcs:
      return I915_FORMAT_MOD_Yf_TILED_CCS;
    case fuchsia_images2::wire::PixelFormatModifier::kArmLinearTe:
      // No DRM format modifier available.
      return DRM_FORMAT_MOD_INVALID;
    default:
      FX_CHECK(false) << "Unhandled format modifier";
      return DRM_FORMAT_MOD_INVALID;
  }
}

// Use async fidl to notice when buffer collection closes server-side.
class AsyncHandler : public fidl::WireAsyncEventHandler<fuchsia_sysmem2::BufferCollection> {
 public:
  AsyncHandler() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

  void on_fidl_error(::fidl::UnbindInfo info) override { unbind_info_ = info; }
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_sysmem2::BufferCollection> metadata) override {}

  async::Loop& loop() { return loop_; }

  auto& unbind_info() { return unbind_info_; }

 private:
  async::Loop loop_;
  std::optional<fidl::UnbindInfo> unbind_info_;
};

class VulkanImageCreator {
 public:
  vk::Result InitVulkan(uint32_t physical_device_index);
  zx_status_t InitSysmem();
  zx_status_t InitScenic();

  vk::PhysicalDeviceLimits GetPhysicalDeviceLimits();

  // Creates the buffer collection and sets constraints.
  vk::Result CreateCollection(vk::ImageConstraintsInfoFUCHSIA* image_constraints_info,
                              fuchsia_images2::wire::PixelFormat format,
                              std::vector<fuchsia_images2::PixelFormatModifier>& modifiers);

  zx_status_t GetImageInfo(uint32_t width, uint32_t height, zx::vmo* vmo_out,
                           zx::eventpair* token_out, magma_image_info_t* image_info_out);

  // Scenic is used if client asks for presentable images.
  bool use_scenic() { return scenic_allocator_.is_valid(); }

  bool IsIntelDevice() {
    auto props = physical_device_.getProperties();
    return props.vendorID == 0x8086;
  }

  void GetFormatFeatures(vk::Format format, bool linear_tiling,
                         vk::FormatFeatureFlags* features_out) {
    auto result = physical_device_.getFormatProperties(format);
    if (linear_tiling) {
      *features_out = result.linearTilingFeatures;
    } else {
      *features_out = result.optimalTilingFeatures;
    }
  }

 private:
  vk::DispatchLoaderDynamic loader_;
  vk::UniqueInstance instance_;
  vk::PhysicalDevice physical_device_;
  vk::UniqueDevice device_;
  fidl::WireSyncClient<fuchsia_ui_composition::Allocator> scenic_allocator_;
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_allocator_;
  fidl::WireSyncClient<fuchsia_sysmem2::BufferCollectionToken> local_token_;
  fidl::WireSyncClient<fuchsia_sysmem2::BufferCollectionToken> vulkan_token_;
  fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> scenic_token_endpoint_;
  std::unique_ptr<AsyncHandler> async_handler_;
  fidl::WireSharedClient<fuchsia_sysmem2::BufferCollection> collection_;
  fuchsia_ui_composition::wire::BufferCollectionImportToken scenic_import_token_;
};

vk::Result VulkanImageCreator::InitVulkan(uint32_t physical_device_index) {
  {
    auto app_info =
        vk::ApplicationInfo().setPApplicationName("magma_image").setApiVersion(VK_API_VERSION_1_1);

    auto instance_info = vk::InstanceCreateInfo().setPApplicationInfo(&app_info);

    auto result = vk::createInstanceUnique(instance_info);
    if (result.result != vk::Result::eSuccess) {
      LOG_VERBOSE("Failed to create instance: %s", vk::to_string(result.result).data());
      return result.result;
    }
    instance_ = std::move(result.value);
  }

  assert(instance_);

  {
    auto [result, physical_devices] = instance_->enumeratePhysicalDevices();

    if (result != vk::Result::eSuccess) {
      LOG_VERBOSE("Failed to enumerate physical devices: %s", vk::to_string(result).data());
      return result;
    }
    if (physical_device_index >= physical_devices.size()) {
      LOG_VERBOSE("Invalid physical device index: %d (%zd)", physical_device_index,
                  physical_devices.size());
      return vk::Result::eErrorInitializationFailed;
    }

    physical_device_ = physical_devices[physical_device_index];
  }

  assert(physical_device_);

  {
    const vk::QueueFlags& queue_flags = vk::QueueFlagBits::eGraphics;
    const auto queue_families = physical_device_.getQueueFamilyProperties();

    size_t queue_family_index = queue_families.size();

    for (size_t i = 0; i < queue_families.size(); ++i) {
      if (queue_families[i].queueFlags & queue_flags) {
        queue_family_index = i;
        break;
      }
    }

    if (queue_family_index == queue_families.size()) {
      LOG_VERBOSE("Failed to find queue family with flags %#x", static_cast<uint32_t>(queue_flags));
      return vk::Result::eErrorInitializationFailed;
    }

    std::array<const char*, 1> device_extensions{VK_FUCHSIA_BUFFER_COLLECTION_EXTENSION_NAME};

    const float queue_priority = 0.0f;
    auto queue_create_info = vk::DeviceQueueCreateInfo()
                                 .setQueueFamilyIndex(to_uint32(queue_family_index))
                                 .setQueueCount(1)
                                 .setPQueuePriorities(&queue_priority);
    auto device_create_info = vk::DeviceCreateInfo()
                                  .setQueueCreateInfoCount(1)
                                  .setPQueueCreateInfos(&queue_create_info)
                                  .setEnabledExtensionCount(device_extensions.size())
                                  .setPpEnabledExtensionNames(device_extensions.data());

    auto result = physical_device_.createDeviceUnique(device_create_info, nullptr);
    if (result.result != vk::Result::eSuccess) {
      LOG_VERBOSE("Failed to create device: %s", vk::to_string(result.result).data());
      return result.result;
    }

    device_ = std::move(result.value);
  }

  assert(device_);

  loader_.init(instance_.get(), device_.get());

  return vk::Result::eSuccess;
}

zx_status_t VulkanImageCreator::InitSysmem() {
  {
    auto client_end = component::Connect<fuchsia_sysmem2::Allocator>();
    if (!client_end.is_ok()) {
      LOG_VERBOSE("Failed to connect to sysmem allocator: %d", client_end.status_value());
      return client_end.status_value();
    }

    sysmem_allocator_ = fidl::WireSyncClient<fuchsia_sysmem2::Allocator>(std::move(*client_end));
  }

  // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
  fidl::Arena arena;
  (void)sysmem_allocator_->SetDebugClientInfo(
      fuchsia_sysmem2::wire::AllocatorSetDebugClientInfoRequest::Builder(arena)
          .name(fsl::GetCurrentProcessName())
          .id(fsl::GetCurrentProcessKoid())
          .Build());
  arena.Reset();

  {
    auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
    if (!endpoints.is_ok()) {
      LOG_VERBOSE("Failed to create endpoints: %d", endpoints.status_value());
      return endpoints.status_value();
    }

    auto result = sysmem_allocator_->AllocateSharedCollection(
        fuchsia_sysmem2::wire::AllocatorAllocateSharedCollectionRequest::Builder(arena)
            .token_request(std::move(endpoints->server))
            .Build());
    if (!result.ok()) {
      LOG_VERBOSE("Failed to allocate shared collection: %d", result.status());
      return result.status();
    }
    arena.Reset();

    local_token_ =
        fidl::WireSyncClient<fuchsia_sysmem2::BufferCollectionToken>(std::move(endpoints->client));
  }

  if (use_scenic()) {
    auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
    if (!endpoints.is_ok()) {
      LOG_VERBOSE("Failed to create endpoints: %d", endpoints.status_value());
      return endpoints.status_value();
    }

    auto result = local_token_->Duplicate(
        fuchsia_sysmem2::wire::BufferCollectionTokenDuplicateRequest::Builder(arena)
            .rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS)
            .token_request(std::move(endpoints->server))
            .Build());
    if (!result.ok()) {
      LOG_VERBOSE("Failed to duplicate token: %d", result.status());
      return result.status();
    }
    arena.Reset();

    scenic_token_endpoint_ = std::move(endpoints->client);
  }

  {
    auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
    if (!endpoints.is_ok()) {
      LOG_VERBOSE("Failed to create endpoints: %d", endpoints.status_value());
      return endpoints.status_value();
    }

    auto result = local_token_->Duplicate(
        fuchsia_sysmem2::wire::BufferCollectionTokenDuplicateRequest::Builder(arena)
            .rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS)
            .token_request(std::move(endpoints->server))
            .Build());
    if (!result.ok()) {
      LOG_VERBOSE("Failed to duplicate token: %d", result.status());
      return result.status();
    }

    vulkan_token_ =
        fidl::WireSyncClient<fuchsia_sysmem2::BufferCollectionToken>(std::move(endpoints->client));
  }

  {
    // Sync the local token that was used for Duplicating
    auto result = local_token_->Sync();
    if (!result.ok()) {
      LOG_VERBOSE("Failed to sync token: %d", result.status());
      return result.status();
    }
  }

  return ZX_OK;
}

zx_status_t VulkanImageCreator::InitScenic() {
  auto client_end = component::Connect<fuchsia_ui_composition::Allocator>();
  if (!client_end.is_ok()) {
    LOG_VERBOSE("Failed to connect to scenic allocator: %d", client_end.status_value());
    return client_end.status_value();
  }

  scenic_allocator_ =
      fidl::WireSyncClient<fuchsia_ui_composition::Allocator>(std::move(*client_end));

  return ZX_OK;
}

vk::PhysicalDeviceLimits VulkanImageCreator::GetPhysicalDeviceLimits() {
  assert(physical_device_);

  vk::PhysicalDeviceProperties props = physical_device_.getProperties();

  return props.limits;
}

vk::Result VulkanImageCreator::CreateCollection(
    vk::ImageConstraintsInfoFUCHSIA* image_constraints_info,
    fuchsia_images2::wire::PixelFormat format,
    std::vector<fuchsia_images2::PixelFormatModifier>& modifiers) {
  assert(device_);

  if (use_scenic()) {
    fuchsia_ui_composition::wire::BufferCollectionExportToken export_token;

    zx_status_t status = zx::eventpair::create(0, &export_token.value, &scenic_import_token_.value);
    if (status != ZX_OK) {
      LOG_VERBOSE("zx::eventpair::create failed: %d", status);
      return vk::Result::eErrorInitializationFailed;
    }

    fidl::Arena allocator;
    fuchsia_ui_composition::wire::RegisterBufferCollectionArgs args(allocator);
    args.set_export_token(std::move(export_token));
    // A sysmem token channel serves both sysmem(1) and sysmem2, so we can convert here until
    // this protocol has a sysmem2 token field.
    args.set_buffer_collection_token(fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>(
        scenic_token_endpoint_.TakeChannel()));

    auto result = scenic_allocator_->RegisterBufferCollection(args);
    if (!result.ok()) {
      LOG_VERBOSE("RegisterBufferCollection returned %d", result.status());
      return vk::Result::eErrorInitializationFailed;
    }

    if (result->is_error()) {
      LOG_VERBOSE("RegisterBufferCollection is_err()");
      return vk::Result::eErrorInitializationFailed;
    }
  }

  // Set vulkan constraints.
  vk::UniqueHandle<vk::BufferCollectionFUCHSIA, vk::DispatchLoaderDynamic> vk_collection;

  {
    auto collection_create_info = vk::BufferCollectionCreateInfoFUCHSIA().setCollectionToken(
        vulkan_token_.TakeClientEnd().TakeChannel().release());

    auto result = device_->createBufferCollectionFUCHSIAUnique(collection_create_info,
                                                               nullptr /*pAllocator*/, loader_);
    if (result.result != vk::Result::eSuccess) {
      LOG_VERBOSE("Failed to create buffer collection: %d", static_cast<int>(result.result));
      return result.result;
    }

    vk_collection = std::move(result.value);
  }

  assert(vk_collection);

  {
    auto result = device_->setBufferCollectionImageConstraintsFUCHSIA(
        vk_collection.get(), image_constraints_info, loader_);
    if (result != vk::Result::eSuccess) {
      LOG_VERBOSE("Failed to set constraints: %s", vk::to_string(result).data());
      return result;
    }
  }

  // Set local constraints.
  async_handler_ = std::make_unique<AsyncHandler>();

  {
    auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollection>();
    if (!endpoints.is_ok()) {
      LOG_VERBOSE("Failed to create endpoints: %d", endpoints.status_value());
      return vk::Result::eErrorInitializationFailed;
    }

    fidl::Arena arena;
    auto result = sysmem_allocator_->BindSharedCollection(
        fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena)
            .token(local_token_.TakeClientEnd())
            .buffer_collection_request(std::move(endpoints->server))
            .Build());
    if (!result.ok()) {
      LOG_VERBOSE("Failed to bind shared collection: %d", result.status());
      return vk::Result::eErrorInitializationFailed;
    }

    collection_.Bind(std::move(endpoints->client), async_handler_->loop().dispatcher(),
                     async_handler_.get(),
                     fidl::ObserveTeardown([&loop = async_handler_->loop()] { loop.Quit(); }));
  }

  {
    fidl::Arena arena;
    auto constraints = fuchsia_sysmem2::wire::BufferCollectionConstraints::Builder(arena);
    constraints.usage(fuchsia_sysmem2::wire::BufferUsage::Builder(arena)
                          .cpu(fuchsia_sysmem2::wire::kCpuUsageReadOften |
                               fuchsia_sysmem2::wire::kCpuUsageWriteOften)
                          .Build());
    constraints.min_buffer_count(1);
    constraints.buffer_memory_constraints(
        fuchsia_sysmem2::wire::BufferMemoryConstraints::Builder(arena)
            .ram_domain_supported(true)
            .cpu_domain_supported(true)
            .Build());

    std::vector<fuchsia_sysmem2::wire::ImageFormatConstraints> image_constraints_vec;
    const auto& image_create_info = image_constraints_info->pFormatConstraints[0].imageCreateInfo;
    for (uint32_t index = 0; index < modifiers.size(); index++) {
      auto image_constraints = fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena);
      image_constraints.min_size(fuchsia_math::wire::SizeU{
          .width = image_create_info.extent.width, .height = image_create_info.extent.height});
      image_constraints.max_size(fuchsia_math::wire::SizeU{
          .width = image_create_info.extent.width, .height = image_create_info.extent.height});
      ZX_DEBUG_ASSERT(!image_constraints.has_min_bytes_per_row());  // Rely on Vulkan to specify
      image_constraints.color_spaces(std::array{fuchsia_images2::wire::ColorSpace::kSrgb});
      image_constraints.pixel_format(format);
      image_constraints.pixel_format_modifier(modifiers[index]);
      image_constraints_vec.emplace_back(image_constraints.Build());
    }
    constraints.image_format_constraints(std::move(image_constraints_vec));

    auto result = collection_->SetConstraints(
        fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena)
            .constraints(constraints.Build())
            .Build());
    if (!result.ok()) {
      LOG_VERBOSE("Failed to set constraints: %d", result.status());
      return vk::Result::eErrorInitializationFailed;
    }
  }

  return vk::Result::eSuccess;
}

zx_status_t VulkanImageCreator::GetImageInfo(uint32_t width, uint32_t height, zx::vmo* vmo_out,
                                             zx::eventpair* token_out,
                                             magma_image_info_t* image_info_out) {
  auto wait_result = collection_.sync()->WaitForAllBuffersAllocated();

  // Process any channel closures to detect any allocation errors
  async_handler_->loop().RunUntilIdle();

  auto unbind_info = async_handler_->unbind_info();
  if (unbind_info && unbind_info->status() != ZX_OK) {
    LOG_VERBOSE("Unbind: %s", unbind_info->FormatDescription().c_str());
    return unbind_info->status();
  }

  const fidl::Status call_result = collection_->Release();
  if (!call_result.ok()) {
    LOG_VERBOSE("Close: %s", call_result.FormatDescription().c_str());
  }

  // Run the loop to ensure local unbind completes.
  collection_.AsyncTeardown();
  // This will be interrupted by `loop.Quit()`.
  async_handler_->loop().Run();

  if (!wait_result.ok()) {
    LOG_VERBOSE("WaitForAllBuffersAllocated failed: %s", wait_result.FormatDescription().c_str());
    return wait_result.status();
  }
  if (wait_result->is_error()) {
    LOG_VERBOSE("Buffer allocation failed: %u", fidl::ToUnderlying(wait_result->error_value()));
    zx_status_t v1_status = sysmem::V1CopyFromV2Error(wait_result->error_value());
    return v1_status;
  }

  auto& collection_info = wait_result->value()->buffer_collection_info();

  if (collection_info.buffers().count() != 1) {
    LOG_VERBOSE("Incorrect buffer collection count: %zu", collection_info.buffers().count());
    return ZX_ERR_INTERNAL;
  }

  if (!collection_info.buffers()[0].vmo().is_valid()) {
    LOG_VERBOSE("Invalid vmo");
    return ZX_ERR_INTERNAL;
  }

  if (collection_info.buffers()[0].vmo_usable_start() != 0) {
    LOG_VERBOSE("Unsupported vmo usable start: %lu",
                collection_info.buffers()[0].vmo_usable_start());
    return ZX_ERR_INTERNAL;
  }

  fidl::Arena arena;
  fpromise::result<fuchsia_images2::wire::ImageFormat> image_format = ImageConstraintsToFormat(
      arena, collection_info.settings().image_format_constraints(), width, height);
  if (!image_format) {
    LOG_VERBOSE("Failed to get image format");
    return ZX_ERR_INTERNAL;
  }

  for (uint32_t plane = 0; plane < MAGMA_MAX_IMAGE_PLANES; ++plane) {
    uint64_t offset;
    if (!ImageFormatPlaneByteOffset(image_format.value(), plane, &offset)) {
      image_info_out->plane_offsets[plane] = 0;
    } else {
      image_info_out->plane_offsets[plane] = to_uint32(offset);
    }

    uint32_t row_bytes;
    if (!ImageFormatPlaneRowBytes(image_format.value(), plane, &row_bytes)) {
      image_info_out->plane_strides[plane] = 0;
    } else {
      image_info_out->plane_strides[plane] = row_bytes;
    }
  }

  if (image_format.value().has_pixel_format_modifier()) {
    image_info_out->drm_format_modifier =
        SysmemModifierToDrmModifier(image_format.value().pixel_format_modifier());
  } else {
    image_info_out->drm_format_modifier = DRM_FORMAT_MOD_LINEAR;
  }

  switch (collection_info.settings().buffer_settings().coherency_domain()) {
    case fuchsia_sysmem2::wire::CoherencyDomain::kCpu:
      image_info_out->coherency_domain = MAGMA_COHERENCY_DOMAIN_CPU;
      break;
    case fuchsia_sysmem2::wire::CoherencyDomain::kRam:
      image_info_out->coherency_domain = MAGMA_COHERENCY_DOMAIN_RAM;
      break;
    case fuchsia_sysmem2::wire::CoherencyDomain::kInaccessible:
      image_info_out->coherency_domain = MAGMA_COHERENCY_DOMAIN_INACCESSIBLE;
      break;
    default:
      LOG_VERBOSE(
          "Unhandled coherency domain: %u",
          fidl::ToUnderlying(collection_info.settings().buffer_settings().coherency_domain()));
      return ZX_ERR_INTERNAL;
  }

  *vmo_out = std::move(collection_info.buffers()[0].vmo());
  *token_out = std::move(scenic_import_token_.value);

  return ZX_OK;
}

magma_status_t MagmaStatus(vk::Result result) {
  switch (result) {
    case vk::Result::eSuccess:
      return MAGMA_STATUS_OK;
    case vk::Result::eTimeout:
      return MAGMA_STATUS_TIMED_OUT;
    case vk::Result::eErrorDeviceLost:
      return MAGMA_STATUS_CONNECTION_LOST;
    case vk::Result::eErrorOutOfHostMemory:
    case vk::Result::eErrorOutOfDeviceMemory:
    case vk::Result::eErrorMemoryMapFailed:
      return MAGMA_STATUS_MEMORY_ERROR;
    case vk::Result::eErrorFormatNotSupported:
      return MAGMA_STATUS_INVALID_ARGS;
    default:
      return MAGMA_STATUS_INTERNAL_ERROR;
  }
}

vk::Format DrmFormatToVulkanFormat(uint64_t drm_format) {
  switch (drm_format) {
    case DRM_FORMAT_ARGB8888:
    case DRM_FORMAT_XRGB8888:
      return vk::Format::eB8G8R8A8Unorm;
    case DRM_FORMAT_ABGR8888:
    case DRM_FORMAT_XBGR8888:
      return vk::Format::eR8G8B8A8Unorm;
  }
  LOG_VERBOSE("Unhandle DRM format: %#lx", drm_format);
  return vk::Format::eUndefined;
}

fuchsia_images2::wire::PixelFormat DrmFormatToSysmemFormat(uint64_t drm_format) {
  switch (drm_format) {
    case DRM_FORMAT_ARGB8888:
    case DRM_FORMAT_XRGB8888:
      return fuchsia_images2::wire::PixelFormat::kB8G8R8A8;
    case DRM_FORMAT_ABGR8888:
    case DRM_FORMAT_XBGR8888:
      return fuchsia_images2::wire::PixelFormat::kR8G8B8A8;
  }
  LOG_VERBOSE("Unhandle DRM format: %#lx", drm_format);
  return fuchsia_images2::wire::PixelFormat::kInvalid;
}

fuchsia_images2::wire::PixelFormatModifier DrmModifierToSysmemModifier(uint64_t modifier) {
  switch (modifier) {
    case DRM_FORMAT_MOD_LINEAR:
      return fuchsia_images2::wire::PixelFormatModifier::kLinear;
    case I915_FORMAT_MOD_X_TILED:
      return fuchsia_images2::wire::PixelFormatModifier::kIntelI915XTiled;
    case I915_FORMAT_MOD_Y_TILED:
      return fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiled;
    case I915_FORMAT_MOD_Yf_TILED:
      return fuchsia_images2::wire::PixelFormatModifier::kIntelI915YfTiled;
    case I915_FORMAT_MOD_Y_TILED_CCS:
      return fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiledCcs;
    case I915_FORMAT_MOD_Yf_TILED_CCS:
      return fuchsia_images2::wire::PixelFormatModifier::kIntelI915YfTiledCcs;
  }
  LOG_VERBOSE("Unhandle DRM modifier: %#lx", modifier);
  return fuchsia_images2::wire::PixelFormatModifier::kInvalid;
}

}  // namespace

namespace magma_image {

magma_status_t CreateDrmImage(uint32_t physical_device_index,
                              const magma_image_create_info_t* create_info,
                              magma_image_info_t* image_info_out, zx::vmo* vmo_out,
                              zx::eventpair* token_out) {
  assert(create_info);
  assert(image_info_out);
  assert(vmo_out);

  if (static_cast<uint32_t>(create_info->flags) &
      ~(MAGMA_IMAGE_CREATE_FLAGS_PRESENTABLE | MAGMA_IMAGE_CREATE_FLAGS_VULKAN_USAGE)) {
    LOG_VERBOSE("Invalid flags: %#lx", create_info->flags);
    return MAGMA_STATUS_INVALID_ARGS;
  }

  vk::Format vk_format = DrmFormatToVulkanFormat(create_info->drm_format);
  if (vk_format == vk::Format::eUndefined) {
    LOG_VERBOSE("Invalid format: %#lx", create_info->drm_format);
    return MAGMA_STATUS_INVALID_ARGS;
  }

  auto sysmem_format = DrmFormatToSysmemFormat(create_info->drm_format);
  if (sysmem_format == fuchsia_images2::wire::PixelFormat::kInvalid) {
    LOG_VERBOSE("Invalid format: %#lx", create_info->drm_format);
    return MAGMA_STATUS_INVALID_ARGS;
  }

  std::vector<fuchsia_images2::PixelFormatModifier> sysmem_modifiers;

  // Convert modifiers provided by client.
  {
    bool terminator_found = false;
    for (uint64_t drm_format_modifier : create_info->drm_format_modifiers) {
      if (drm_format_modifier == DRM_FORMAT_MOD_INVALID) {
        terminator_found = true;
        break;
      }

      fuchsia_images2::PixelFormatModifier modifier =
          DrmModifierToSysmemModifier(drm_format_modifier);
      if (modifier == fuchsia_images2::wire::PixelFormatModifier::kInvalid) {
        LOG_VERBOSE("Invalid modifier: %#lx", drm_format_modifier);
        return MAGMA_STATUS_INVALID_ARGS;
      }

      sysmem_modifiers.push_back(modifier);
    }

    if (!terminator_found) {
      LOG_VERBOSE("Missing modifier terminator");
      return MAGMA_STATUS_INVALID_ARGS;
    }
  }

  VulkanImageCreator image_creator;

  {
    vk::Result result = image_creator.InitVulkan(physical_device_index);
    if (result != vk::Result::eSuccess) {
      LOG_VERBOSE("Failed to initialize Vulkan");
      return MagmaStatus(result);
    }
  }

  {
    vk::PhysicalDeviceLimits limits = image_creator.GetPhysicalDeviceLimits();
    if (create_info->width > limits.maxImageDimension2D ||
        create_info->height > limits.maxImageDimension2D) {
      LOG_VERBOSE("Invalid width %u or height %u (%u)", create_info->width, create_info->height,
                  limits.maxImageDimension2D);
      return MAGMA_STATUS_INVALID_ARGS;
    }
  }

  if (create_info->flags & MAGMA_IMAGE_CREATE_FLAGS_PRESENTABLE) {
    zx_status_t status = image_creator.InitScenic();
    if (status != ZX_OK) {
      LOG_VERBOSE("Failed to initialize scenic: %d", status);
      return MAGMA_STATUS_INTERNAL_ERROR;
    }
  }

  zx_status_t status = image_creator.InitSysmem();
  if (status != ZX_OK) {
    LOG_VERBOSE("Failed to initialize sysmem: %d", status);
    return MAGMA_STATUS_INTERNAL_ERROR;
  }

  vk::ImageUsageFlags vk_usage = {};
  vk::FormatFeatureFlags vk_format_features = {};

  if (create_info->flags & MAGMA_IMAGE_CREATE_FLAGS_VULKAN_USAGE) {
    // Use the Vulkan usage as provided by the client.
    vk_usage = static_cast<vk::ImageUsageFlags>(create_info->flags >> 32);

    if (vk_usage & vk::ImageUsageFlagBits::eTransferSrc) {
      vk_format_features |= vk::FormatFeatureFlagBits::eTransferSrc;
    }
    if (vk_usage & vk::ImageUsageFlagBits::eTransferDst) {
      vk_format_features |= vk::FormatFeatureFlagBits::eTransferDst;
    }
    if (vk_usage & vk::ImageUsageFlagBits::eSampled) {
      vk_format_features |= vk::FormatFeatureFlagBits::eSampledImage;
    }
    if (vk_usage & vk::ImageUsageFlagBits::eStorage) {
      vk_format_features |= vk::FormatFeatureFlagBits::eStorageImage;
    }
    if (vk_usage & vk::ImageUsageFlagBits::eColorAttachment) {
      vk_format_features |= vk::FormatFeatureFlagBits::eColorAttachment;
    }
    if (vk_usage & vk::ImageUsageFlagBits::eDepthStencilAttachment) {
      vk_format_features |= vk::FormatFeatureFlagBits::eDepthStencilAttachment;
    }
  } else {
    // If linear isn't requested, assume we'll get a tiled format modifier.
    bool linear_tiling = sysmem_modifiers.size() == 1 &&
                         sysmem_modifiers[0] == fuchsia_images2::wire::PixelFormatModifier::kLinear;

    image_creator.GetFormatFeatures(vk_format, linear_tiling, &vk_format_features);

    // For non-ICD clients like GBM, the client API has no fine grained usage.
    // To maximize compatibility, we pass as many usages as make sense given the format features.
    if (vk_format_features & vk::FormatFeatureFlagBits::eTransferSrc) {
      vk_usage |= vk::ImageUsageFlagBits::eTransferSrc;
    }
    if (vk_format_features & vk::FormatFeatureFlagBits::eTransferDst) {
      vk_usage |= vk::ImageUsageFlagBits::eTransferDst;
    }
    if (vk_format_features & vk::FormatFeatureFlagBits::eSampledImage) {
      vk_usage |= vk::ImageUsageFlagBits::eSampled;
    }
    // Intel Vulkan driver doesn't support CCS with storage images; assume the client doesn't need
    // storage to allow for the performance benefit of CCS.
    if (!image_creator.IsIntelDevice() &&
        vk_format_features & vk::FormatFeatureFlagBits::eStorageImage) {
      vk_usage |= vk::ImageUsageFlagBits::eStorage;
    }
    if (vk_format_features & vk::FormatFeatureFlagBits::eColorAttachment) {
      vk_usage |= vk::ImageUsageFlagBits::eColorAttachment;
      vk_usage |= vk::ImageUsageFlagBits::eInputAttachment;
    }
    if (vk_format_features & vk::FormatFeatureFlagBits::eDepthStencilAttachment) {
      vk_usage |= vk::ImageUsageFlagBits::eDepthStencilAttachment;
      vk_usage |= vk::ImageUsageFlagBits::eInputAttachment;
    }
    // No format features apply here.
    vk_usage |= vk::ImageUsageFlagBits::eTransientAttachment;
  }

  auto image_create_info = vk::ImageCreateInfo()
                               .setFormat(vk_format)
                               .setImageType(vk::ImageType::e2D)
                               .setMipLevels(1)
                               .setArrayLayers(1)
                               .setExtent({create_info->width, create_info->height, 1})
                               .setTiling(vk::ImageTiling::eOptimal)
                               .setSharingMode(vk::SharingMode::eExclusive)
                               .setUsage(vk_usage);

  const vk::SysmemColorSpaceFUCHSIA kRgbColorSpace = {
      static_cast<uint32_t>(fuchsia_images2::wire::ColorSpace::kSrgb)};
  const vk::SysmemColorSpaceFUCHSIA kYuvColorSpace = {
      static_cast<uint32_t>(fuchsia_images2::wire::ColorSpace::kRec709)};

  bool isYuvFormat = vk_format == vk::Format::eG8B8G8R8422Unorm ||
                     vk_format == vk::Format::eG8B8R82Plane420Unorm ||
                     vk_format == vk::Format::eG8B8R83Plane420Unorm;

  auto format_info = vk::ImageFormatConstraintsInfoFUCHSIA()
                         .setImageCreateInfo(image_create_info)
                         .setRequiredFormatFeatures(vk_format_features)
                         .setSysmemPixelFormat({})
                         .setColorSpaceCount(1u)
                         .setPColorSpaces(isYuvFormat ? &kYuvColorSpace : &kRgbColorSpace);

  auto image_constraints =
      vk::ImageConstraintsInfoFUCHSIA()
          .setFlags({})
          .setFormatConstraints(format_info)
          .setBufferCollectionConstraints(
              vk::BufferCollectionConstraintsInfoFUCHSIA().setMinBufferCount(1u).setMaxBufferCount(
                  1u));

  vk::Result result =
      image_creator.CreateCollection(&image_constraints, sysmem_format, sysmem_modifiers);
  if (result != vk::Result::eSuccess) {
    LOG_VERBOSE("Failed to create collection: %s", vk::to_string(result).data());
    return MagmaStatus(result);
  }

  status = image_creator.GetImageInfo(create_info->width, create_info->height, vmo_out, token_out,
                                      image_info_out);
  if (status != ZX_OK) {
    LOG_VERBOSE("GetImageInfo failed: %d", status);
    if (status == ZX_ERR_NOT_SUPPORTED)
      return MAGMA_STATUS_INVALID_ARGS;

    return MAGMA_STATUS_INTERNAL_ERROR;
  }

  return MAGMA_STATUS_OK;
}

}  // namespace magma_image
