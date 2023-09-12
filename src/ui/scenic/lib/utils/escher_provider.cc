// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/escher_provider.h"

#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/vfs/cpp/pseudo_file.h>

#include <vulkan/vulkan.h>

#include "src/ui/lib/escher/fs/hack_filesystem.h"
#include "src/ui/lib/escher/hmd/pose_buffer_latching_shader.h"
#include "src/ui/lib/escher/paper/paper_renderer_static_config.h"
#include "src/ui/lib/escher/vk/vulkan_device_queues.h"
#include "src/ui/lib/escher/vk/vulkan_instance.h"

namespace utils {

escher::EscherUniquePtr CreateEscher(sys::ComponentContext* app_context) {
  // Initialize Vulkan.
  constexpr bool kRequiresSurface = false;
  escher::VulkanInstance::Params instance_params(
      {{},
       {
           VK_EXT_DEBUG_REPORT_EXTENSION_NAME,
           VK_KHR_EXTERNAL_MEMORY_CAPABILITIES_EXTENSION_NAME,
           VK_KHR_EXTERNAL_SEMAPHORE_CAPABILITIES_EXTENSION_NAME,
           VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME,
       },
       kRequiresSurface});

  // Only enable Vulkan validation layers when in debug mode.
#if defined(SCENIC_ENABLE_VULKAN_VALIDATION)
  instance_params.layer_names.insert("VK_LAYER_KHRONOS_validation");
#endif
  instance_params.initial_debug_report_callbacks.push_back(HandleDebugReport);
  auto vulkan_instance = escher::VulkanInstance::New(std::move(instance_params));
  if (!vulkan_instance)
    return nullptr;

  // Tell Escher not to filter out queues that don't support presentation.
  // The display manager only supports a single connection, so none of the
  // available queues will support presentation.  This is OK, because we use
  // the display manager API to present frames directly, instead of using
  // Vulkan swapchains.
  escher::VulkanDeviceQueues::Params device_queues_params(
      {{
           VK_EXT_QUEUE_FAMILY_FOREIGN_EXTENSION_NAME,
           VK_FUCHSIA_BUFFER_COLLECTION_EXTENSION_NAME,
           VK_FUCHSIA_EXTERNAL_MEMORY_EXTENSION_NAME,
           VK_FUCHSIA_EXTERNAL_SEMAPHORE_EXTENSION_NAME,
           VK_KHR_BIND_MEMORY_2_EXTENSION_NAME,
           VK_KHR_EXTERNAL_MEMORY_EXTENSION_NAME,
           VK_KHR_EXTERNAL_SEMAPHORE_EXTENSION_NAME,
           VK_KHR_GET_MEMORY_REQUIREMENTS_2_EXTENSION_NAME,
           VK_KHR_MAINTENANCE1_EXTENSION_NAME,
       },
       {
           VK_KHR_GLOBAL_PRIORITY_EXTENSION_NAME,
           VK_KHR_SAMPLER_YCBCR_CONVERSION_EXTENSION_NAME,
       },
       vk::SurfaceKHR(),
       escher::VulkanDeviceQueues::Params::kDisableQueueFilteringForPresent |
           escher::VulkanDeviceQueues::Params::kAllowProtectedMemory});

  auto vulkan_device_queues =
      escher::VulkanDeviceQueues::New(vulkan_instance, device_queues_params);
  if (!vulkan_device_queues) {
    return nullptr;
  }

  // Provide a PseudoDir where the gfx system can register debugging services.
  auto debug_dir = std::make_shared<vfs::PseudoDir>();
  app_context->outgoing()->debug_dir()->AddSharedEntry("gfx", debug_dir);

  auto shader_fs = escher::HackFilesystem::New(debug_dir);
  {
#if ESCHER_USE_RUNTIME_GLSL
    auto paths = escher::kPaperRendererShaderPaths;
    paths.insert(paths.end(), escher::hmd::kPoseBufferLatchingPaths.begin(),
                 escher::hmd::kPoseBufferLatchingPaths.end());
    bool success = shader_fs->InitializeWithRealFiles(paths);
#else
    auto paths = escher::kPaperRendererShaderSpirvPaths;
    paths.insert(paths.end(), escher::hmd::kPoseBufferLatchingSpirvPaths.begin(),
                 escher::hmd::kPoseBufferLatchingSpirvPaths.end());
    bool success = shader_fs->InitializeWithRealFiles(paths);
#endif
    FX_DCHECK(success) << "Failed to init shader files.";
  }

  // Initialize Escher.
#if ESCHER_USE_RUNTIME_GLSL
  escher::GlslangInitializeProcess();
#endif
  return escher::EscherUniquePtr(
      new escher::Escher(vulkan_device_queues, std::move(shader_fs), /*gpu_allocator*/ nullptr),
      // Custom deleter.
      // The vulkan instance is a stack variable, but it is a
      // fxl::RefPtr, so we can store by value.
      [=](escher::Escher* escher) {
#if ESCHER_USE_RUNTIME_GLSL
        escher::GlslangFinalizeProcess();
#endif
        delete escher;
      });
}

VkBool32 HandleDebugReport(VkDebugReportFlagsEXT flags_in,
                           VkDebugReportObjectTypeEXT object_type_in, uint64_t object,
                           size_t location, int32_t message_code, const char* pLayerPrefix,
                           const char* pMessage, void* pUserData) {
  vk::DebugReportFlagsEXT flags(static_cast<vk::DebugReportFlagBitsEXT>(flags_in));
  vk::DebugReportObjectTypeEXT object_type(
      static_cast<vk::DebugReportObjectTypeEXT>(object_type_in));

// Macro to facilitate matching messages.  Example usage:
//  if (VK_MATCH_REPORT(DescriptorSet, 0, "VUID-VkWriteDescriptorSet-descriptorType-01403")) {
//    FX_LOGS(INFO) << "ignoring descriptor set problem: " << pMessage << "\n\n";
//    return false;
//  }
#define VK_MATCH_REPORT(OTYPE, CODE, X)                                                 \
  ((object_type == vk::DebugReportObjectTypeEXT::e##OTYPE) && (message_code == CODE) && \
   (0 == strncmp(pMessage + 3, X, strlen(X) - 1)))

#define VK_DEBUG_REPORT_MESSAGE                                                                \
  pMessage << " (layer: " << pLayerPrefix << "  code: " << message_code                        \
           << "  object-type: " << vk::to_string(object_type) << "  object: " << object << ")" \
           << std::endl;

  bool fatal = false;
  if (flags == vk::DebugReportFlagBitsEXT::eInformation) {
    FX_LOGS(INFO) << "## Vulkan Information: " << VK_DEBUG_REPORT_MESSAGE;
  } else if (flags == vk::DebugReportFlagBitsEXT::eWarning) {
    FX_LOGS(WARNING) << "## Vulkan Warning: " << VK_DEBUG_REPORT_MESSAGE;
  } else if (flags == vk::DebugReportFlagBitsEXT::ePerformanceWarning) {
    FX_LOGS(WARNING) << "## Vulkan Performance Warning: " << VK_DEBUG_REPORT_MESSAGE;
  } else if (flags == vk::DebugReportFlagBitsEXT::eError) {
    // Treat all errors as fatal.
    fatal = true;
    FX_LOGS(ERROR) << "## Vulkan Error: " << VK_DEBUG_REPORT_MESSAGE;
  } else if (flags == vk::DebugReportFlagBitsEXT::eDebug) {
    FX_LOGS(INFO) << "## Vulkan Debug: " << VK_DEBUG_REPORT_MESSAGE;
  } else {
    // This should never happen, unless a new value has been added to
    // vk::DebugReportFlagBitsEXT.  In that case, add a new if-clause above.
    fatal = true;
    FX_LOGS(ERROR) << "## Vulkan Unknown Message Type (flags: " << vk::to_string(flags) << "): ";
  }

  // Crash immediately on fatal errors.
  FX_CHECK(!fatal);

  return false;

#undef VK_DEBUG_REPORT_MESSAGE
}

}  // namespace utils
