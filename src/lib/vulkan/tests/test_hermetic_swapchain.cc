// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dlfcn.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/zx/channel.h>

#include <chrono>

#include <gtest/gtest.h>
#include <vulkan/vulkan.h>

namespace {

constexpr uint32_t kVendorIDIntel = 0x8086;

VkPhysicalDeviceType GetVkPhysicalDeviceType(VkPhysicalDevice device) {
  VkPhysicalDeviceProperties properties;
  vkGetPhysicalDeviceProperties(device, &properties);
  return properties.deviceType;
}

uint32_t GetVkPhysicalDeviceVendorID(VkPhysicalDevice device) {
  VkPhysicalDeviceProperties properties;
  vkGetPhysicalDeviceProperties(device, &properties);
  return properties.vendorID;
}

}  // namespace

static VKAPI_ATTR VkBool32 VKAPI_CALL
debug_utils_callback(VkDebugUtilsMessageSeverityFlagBitsEXT messageSeverity,
                     VkDebugUtilsMessageTypeFlagsEXT messageTypes,
                     const VkDebugUtilsMessengerCallbackDataEXT* pCallbackData, void* pUserData);

class TestSwapchain {
 public:
  static constexpr uint32_t kSwapchainImageCount = 3;

  template <class T>
  void LoadProc(T* proc, const char* name) {
    auto get_device_proc_addr = reinterpret_cast<PFN_vkGetDeviceProcAddr>(
        vkGetInstanceProcAddr(vk_instance_, "vkGetDeviceProcAddr"));
    *proc = reinterpret_cast<T>(get_device_proc_addr(vk_device_, name));
  }

  TestSwapchain(std::vector<const char*> instance_layers, bool protected_memory)
      : protected_memory_(protected_memory) {
    std::vector<const char*> instance_ext{
        VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_GET_SURFACE_CAPABILITIES_2_EXTENSION_NAME,
        VK_KHR_SURFACE_PROTECTED_CAPABILITIES_EXTENSION_NAME,
        VK_FUCHSIA_IMAGEPIPE_SURFACE_EXTENSION_NAME, VK_EXT_DEBUG_UTILS_EXTENSION_NAME};
    std::vector<const char*> device_ext{VK_KHR_SWAPCHAIN_EXTENSION_NAME};

    VkDebugUtilsMessengerCreateInfoEXT init_callback = {
        .sType = VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT,
        .pNext = nullptr,
        .flags = 0,
        .messageSeverity = VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT,
        .messageType = VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT |
                       VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT,
        .pfnUserCallback = debug_utils_callback,
        .pUserData = this};
    const VkValidationFeatureEnableEXT sync_validation =
        VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT;
    const VkValidationFeaturesEXT validation_features = {
        .sType = VK_STRUCTURE_TYPE_VALIDATION_FEATURES_EXT,
        .pNext = &init_callback,
        .enabledValidationFeatureCount = 1,
        .pEnabledValidationFeatures = &sync_validation,
    };
    const VkApplicationInfo app_info = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pNext = nullptr,
        .pApplicationName = "test",
        .applicationVersion = 0,
        .pEngineName = "test",
        .engineVersion = 0,
        .apiVersion = VK_MAKE_VERSION(1, 1, 0),
    };
    VkInstanceCreateInfo inst_info = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pNext = &validation_features,
        .pApplicationInfo = &app_info,
        .enabledLayerCount = static_cast<uint32_t>(instance_layers.size()),
        .ppEnabledLayerNames = instance_layers.data(),
        .enabledExtensionCount = static_cast<uint32_t>(instance_ext.size()),
        .ppEnabledExtensionNames = instance_ext.data(),
    };

    VkResult result;
    result = vkCreateInstance(&inst_info, nullptr, &vk_instance_);
    if (result != VK_SUCCESS) {
      fprintf(stderr, "vkCreateInstance failed: %d\n", result);
      return;
    }

    uint32_t gpu_count = 1;
    std::vector<VkPhysicalDevice> physical_devices(gpu_count);
    result = vkEnumeratePhysicalDevices(vk_instance_, &gpu_count, physical_devices.data());
    if (result != VK_SUCCESS && result != VK_INCOMPLETE) {
      fprintf(stderr, "vkEnumeratePhysicalDevices failed: %d\n", result);
      return;
    }
    vk_physical_device_ = physical_devices[0];

    VkPhysicalDeviceProtectedMemoryFeatures protected_memory_features = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_FEATURES,
        .pNext = nullptr,
        .protectedMemory = VK_FALSE,
    };
    if (protected_memory_) {
      VkPhysicalDeviceProperties properties;
      vkGetPhysicalDeviceProperties(vk_physical_device_, &properties);
      if (properties.apiVersion < VK_MAKE_VERSION(1, 1, 0)) {
        protected_memory_is_supported_ = false;
        fprintf(stderr, "Vulkan 1.1 is not supported by device\n");
        return;
      }

      PFN_vkGetPhysicalDeviceFeatures2 get_physical_device_features_2_ =
          reinterpret_cast<PFN_vkGetPhysicalDeviceFeatures2>(
              vkGetInstanceProcAddr(vk_instance_, "vkGetPhysicalDeviceFeatures2"));
      if (!get_physical_device_features_2_) {
        fprintf(stderr, "Failed to find vkGetPhysicalDeviceFeatures2\n");
        return;
      }
      VkPhysicalDeviceFeatures2 physical_device_features = {
          .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
          .pNext = &protected_memory_features};
      get_physical_device_features_2_(vk_physical_device_, &physical_device_features);
      protected_memory_is_supported_ = protected_memory_features.protectedMemory;
      if (!protected_memory_is_supported_) {
        fprintf(stderr, "Protected memory is not supported\n");
        return;
      }
    }

    auto pfnCreateDebugUtilsMessengerEXT = reinterpret_cast<PFN_vkCreateDebugUtilsMessengerEXT>(
        vkGetInstanceProcAddr(vk_instance_, "vkCreateDebugUtilsMessengerEXT"));
    get_surface_support_khr_ = reinterpret_cast<PFN_vkGetPhysicalDeviceSurfaceSupportKHR>(
        vkGetInstanceProcAddr(vk_instance_, "vkGetPhysicalDeviceSurfaceSupportKHR"));
    get_surface_caps2_khr_ = reinterpret_cast<PFN_vkGetPhysicalDeviceSurfaceCapabilities2KHR>(
        vkGetInstanceProcAddr(vk_instance_, "vkGetPhysicalDeviceSurfaceCapabilities2KHR"));

    VkDebugUtilsMessengerCreateInfoEXT callback = {
        .sType = VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT,
        .pNext = nullptr,
        .flags = 0,
        .messageSeverity = VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT,
        .messageType = VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT |
                       VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT,
        .pfnUserCallback = debug_utils_callback,
        .pUserData = this};
    result = pfnCreateDebugUtilsMessengerEXT(vk_instance_, &callback, NULL, &messenger_cb_);
    if (result != VK_SUCCESS) {
      fprintf(stderr, "Failed to install debug callback\n");
      return;
    }
    float queue_priorities[1] = {0.0};
    VkDeviceQueueCreateInfo queue_create_info = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .pNext = nullptr,
        .flags = protected_memory_
                     ? static_cast<VkDeviceQueueCreateFlags>(VK_DEVICE_QUEUE_CREATE_PROTECTED_BIT)
                     : 0,
        .queueFamilyIndex = 0,
        .queueCount = 1,
        .pQueuePriorities = queue_priorities};

    VkDeviceCreateInfo device_create_info = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .pNext = protected_memory_ ? &protected_memory_features : nullptr,
        .queueCreateInfoCount = 1,
        .pQueueCreateInfos = &queue_create_info,
        .enabledLayerCount = 0,
        .ppEnabledLayerNames = nullptr,
        .enabledExtensionCount = static_cast<uint32_t>(device_ext.size()),
        .ppEnabledExtensionNames = device_ext.data(),
        .pEnabledFeatures = nullptr};

    result = vkCreateDevice(vk_physical_device_, &device_create_info, nullptr, &vk_device_);
    if (result != VK_SUCCESS) {
      fprintf(stderr, "vkCreateDevice failed: %d\n", result);
      return;
    }

    VkCommandPoolCreateInfo pool_info = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .pNext = nullptr,
        .flags = protected_memory_ ? VK_COMMAND_POOL_CREATE_PROTECTED_BIT : 0u,
        .queueFamilyIndex = 0,
    };

    EXPECT_EQ(VK_SUCCESS, vkCreateCommandPool(vk_device_, &pool_info, nullptr, &vk_command_pool_));

    PFN_vkGetDeviceProcAddr get_device_proc_addr = reinterpret_cast<PFN_vkGetDeviceProcAddr>(
        vkGetInstanceProcAddr(vk_instance_, "vkGetDeviceProcAddr"));
    if (!get_device_proc_addr) {
      fprintf(stderr, "Failed to find vkGetDeviceProcAddr\n");
      return;
    }

    LoadProc(&create_swapchain_khr_, "vkCreateSwapchainKHR");
    LoadProc(&destroy_swapchain_khr_, "vkDestroySwapchainKHR");
    LoadProc(&get_swapchain_images_khr_, "vkGetSwapchainImagesKHR");
    LoadProc(&acquire_next_image_khr_, "vkAcquireNextImageKHR");
    LoadProc(&queue_present_khr_, "vkQueuePresentKHR");
    LoadProc(&get_device_queue2_, "vkGetDeviceQueue2");

    if (protected_memory_) {
      VkDeviceQueueInfo2 queue_info2 = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_INFO_2,
                                        .pNext = nullptr,
                                        .flags = VK_DEVICE_QUEUE_CREATE_PROTECTED_BIT,
                                        .queueFamilyIndex = 0,
                                        .queueIndex = 0};
      get_device_queue2_(vk_device_, &queue_info2, &vk_queue_);
    } else {
      vkGetDeviceQueue(vk_device_, 0, 0, &vk_queue_);
    }

    init_ = true;
  }

  ~TestSwapchain() {
    if (vk_command_pool_) {
      vkDestroyCommandPool(vk_device_, vk_command_pool_, nullptr);
    }
    if (messenger_cb_) {
      auto pfnDestroyDebugUtilsMessengerEXT = reinterpret_cast<PFN_vkDestroyDebugUtilsMessengerEXT>(
          vkGetInstanceProcAddr(vk_instance_, "vkDestroyDebugUtilsMessengerEXT"));
      pfnDestroyDebugUtilsMessengerEXT(vk_instance_, messenger_cb_, nullptr);
    }

    if (vk_device_ != VK_NULL_HANDLE)
      vkDestroyDevice(vk_device_, nullptr);

    if (vk_instance_ != VK_NULL_HANDLE)
      vkDestroyInstance(vk_instance_, nullptr);
  }

  void ValidateSurfaceForDevice(VkSurfaceKHR surface) {
    VkSurfaceProtectedCapabilitiesKHR surface_protected_caps = {
        .sType = VK_STRUCTURE_TYPE_SURFACE_PROTECTED_CAPABILITIES_KHR,
        .pNext = nullptr,
    };
    VkSurfaceCapabilities2KHR surface_caps2 = {
        .sType = VK_STRUCTURE_TYPE_SURFACE_CAPABILITIES_2_KHR,
        .pNext = &surface_protected_caps,
    };
    VkPhysicalDeviceSurfaceInfo2KHR surface_info = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SURFACE_INFO_2_KHR,
        .pNext = nullptr,
        .surface = surface,
    };
    ASSERT_TRUE(get_surface_caps2_khr_);
    EXPECT_EQ(VK_SUCCESS,
              get_surface_caps2_khr_(vk_physical_device_, &surface_info, &surface_caps2));
    if (protected_memory_) {
      EXPECT_TRUE(surface_protected_caps.supportsProtected);
    }
    VkBool32 surface_supported = false;
    EXPECT_EQ(VK_SUCCESS, get_surface_support_khr_(vk_physical_device_, /*queue_family_index*/ 0,
                                                   surface, &surface_supported));
    EXPECT_TRUE(surface_supported);
  }

  std::vector<VkImage> GetSwapchainImages(VkSwapchainKHR swapchain) {
    uint32_t image_count = 0;
    EXPECT_EQ(VK_SUCCESS, get_swapchain_images_khr_(vk_device_, swapchain, &image_count, nullptr));
    EXPECT_EQ(image_count, kSwapchainImageCount);
    std::vector<VkImage> images(image_count);
    EXPECT_EQ(VK_SUCCESS,
              get_swapchain_images_khr_(vk_device_, swapchain, &image_count, images.data()));
    return images;
  }

  VkResult CreateSwapchainHelper(VkSurfaceKHR surface, VkFormat format, VkImageUsageFlags usage,
                                 VkSwapchainKHR* swapchain_out) {
    ValidateSurfaceForDevice(surface);
    VkSwapchainCreateInfoKHR create_info = {
        .sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
        .pNext = nullptr,
        .flags = protected_memory_ ? VK_SWAPCHAIN_CREATE_PROTECTED_BIT_KHR
                                   : static_cast<VkSwapchainCreateFlagsKHR>(0),
        .surface = surface,
        .minImageCount = kSwapchainImageCount,
        .imageFormat = format,
        .imageColorSpace = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
        .imageExtent = {1280, 800},
        .imageArrayLayers = 1,
        .imageUsage = usage,
        .imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
        .queueFamilyIndexCount = 0,
        .pQueueFamilyIndices = nullptr,
        .preTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
        .compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
        .presentMode = VK_PRESENT_MODE_FIFO_KHR,
        .clipped = VK_TRUE,
        .oldSwapchain = VK_NULL_HANDLE,
    };

    VkResult result = create_swapchain_khr_(vk_device_, &create_info, nullptr, swapchain_out);
    if (result != VK_SUCCESS)
      return result;

    // Get Swapchain images to make the validation layers happy.
    GetSwapchainImages(*swapchain_out);
    return VK_SUCCESS;
  }

  void Surface(bool use_dynamic_symbol) {
    ASSERT_TRUE(init_);

    PFN_vkCreateImagePipeSurfaceFUCHSIA f_vkCreateImagePipeSurfaceFUCHSIA =
        use_dynamic_symbol
            ? reinterpret_cast<PFN_vkCreateImagePipeSurfaceFUCHSIA>(
                  vkGetInstanceProcAddr(vk_instance_, "vkCreateImagePipeSurfaceFUCHSIA"))
            : vkCreateImagePipeSurfaceFUCHSIA;
    ASSERT_TRUE(f_vkCreateImagePipeSurfaceFUCHSIA);

    zx::channel endpoint0, endpoint1;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &endpoint0, &endpoint1));

    VkImagePipeSurfaceCreateInfoFUCHSIA create_info = {
        .sType = VK_STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA,
        .pNext = nullptr,
        .imagePipeHandle = endpoint0.release(),
    };
    VkSurfaceKHR surface;
    EXPECT_EQ(VK_SUCCESS,
              f_vkCreateImagePipeSurfaceFUCHSIA(vk_instance_, &create_info, nullptr, &surface));
    vkDestroySurfaceKHR(vk_instance_, surface, nullptr);
  }

  void CreateSwapchain(int num_swapchains, VkFormat format, VkImageUsageFlags usage) {
    ASSERT_TRUE(init_);

    VkImagePipeSurfaceCreateInfoFUCHSIA create_info = {
        .sType = VK_STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA,
        .pNext = nullptr,
        .imagePipeHandle = ZX_HANDLE_INVALID};
    VkSurfaceKHR surface;
    EXPECT_EQ(VK_SUCCESS,
              vkCreateImagePipeSurfaceFUCHSIA(vk_instance_, &create_info, nullptr, &surface));

    for (int i = 0; i < num_swapchains; ++i) {
      VkSwapchainKHR swapchain;
      VkResult result = CreateSwapchainHelper(surface, format, usage, &swapchain);
      EXPECT_EQ(VK_SUCCESS, result);
      if (VK_SUCCESS == result) {
        destroy_swapchain_khr_(vk_device_, swapchain, nullptr);
      }
    }

    vkDestroySurfaceKHR(vk_instance_, surface, nullptr);
  }

  void TransitionLayout(VkImage image, VkImageLayout to) {
    VkCommandBuffer command_buffer;
    VkCommandBufferAllocateInfo alloc_info = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .pNext = nullptr,
        .commandPool = vk_command_pool_,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    EXPECT_EQ(VK_SUCCESS, vkAllocateCommandBuffers(vk_device_, &alloc_info, &command_buffer));
    VkCommandBufferBeginInfo begin_info = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .pNext = nullptr,
        .flags = 0,
        .pInheritanceInfo = nullptr,
    };
    EXPECT_EQ(VK_SUCCESS, vkBeginCommandBuffer(command_buffer, &begin_info));
    VkImageMemoryBarrier image_barrer = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
        .pNext = nullptr,
        .srcAccessMask = 0,
        .dstAccessMask = 0,
        .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
        .newLayout = to,
        .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .image = image,
        .subresourceRange = {.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT,
                             .baseMipLevel = 0,
                             .levelCount = 1,
                             .baseArrayLayer = 0,
                             .layerCount = 1}};
    vkCmdPipelineBarrier(command_buffer, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                         VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 0, nullptr, 0, nullptr, 1,
                         &image_barrer);
    VkProtectedSubmitInfo protected_submit = {.sType = VK_STRUCTURE_TYPE_PROTECTED_SUBMIT_INFO,
                                              .pNext = nullptr,
                                              .protectedSubmit = protected_memory_};
    VkSubmitInfo submit_info = {
        .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .pNext = &protected_submit,
        .waitSemaphoreCount = 0,
        .pWaitSemaphores = nullptr,
        .commandBufferCount = 1,
        .pCommandBuffers = &command_buffer,
        .signalSemaphoreCount = 0,
        .pSignalSemaphores = nullptr,
    };
    EXPECT_EQ(VK_SUCCESS, vkEndCommandBuffer(command_buffer));
    EXPECT_EQ(VK_SUCCESS, vkQueueSubmit(vk_queue_, 1, &submit_info, VK_NULL_HANDLE));
    EXPECT_EQ(VK_SUCCESS, vkQueueWaitIdle(vk_queue_));
    vkFreeCommandBuffers(vk_device_, vk_command_pool_, 1, &command_buffer);
  }

  VkInstance vk_instance_ = VK_NULL_HANDLE;
  VkPhysicalDevice vk_physical_device_ = VK_NULL_HANDLE;
  VkDevice vk_device_ = VK_NULL_HANDLE;
  VkCommandPool vk_command_pool_ = VK_NULL_HANDLE;
  VkQueue vk_queue_ = VK_NULL_HANDLE;
  VkDebugUtilsMessengerEXT messenger_cb_ = nullptr;
  PFN_vkGetPhysicalDeviceSurfaceCapabilities2KHR get_surface_caps2_khr_;
  PFN_vkGetPhysicalDeviceSurfaceSupportKHR get_surface_support_khr_;
  PFN_vkCreateSwapchainKHR create_swapchain_khr_;
  PFN_vkDestroySwapchainKHR destroy_swapchain_khr_;
  PFN_vkGetSwapchainImagesKHR get_swapchain_images_khr_;
  PFN_vkAcquireNextImageKHR acquire_next_image_khr_;
  PFN_vkQueuePresentKHR queue_present_khr_;
  PFN_vkGetDeviceQueue2 get_device_queue2_;

  const bool protected_memory_ = false;
  bool init_ = false;
  bool protected_memory_is_supported_ = false;
  bool allows_validation_errors_ = false;
};

static VKAPI_ATTR VkBool32 VKAPI_CALL
debug_utils_callback(VkDebugUtilsMessageSeverityFlagBitsEXT messageSeverity,
                     VkDebugUtilsMessageTypeFlagsEXT messageTypes,
                     const VkDebugUtilsMessengerCallbackDataEXT* pCallbackData, void* pUserData) {
  fprintf(stderr, "Got debug utils callback: %s\n", pCallbackData->pMessage);
  auto swapchain = static_cast<TestSwapchain*>(pUserData);
  EXPECT_TRUE(swapchain->allows_validation_errors_);
  return VK_FALSE;
}
using UseProtectedMemory = bool;
using ValidationBeforeLayer = bool;

using ParamType = std::tuple<UseProtectedMemory, ValidationBeforeLayer>;

static std::string NameFromParam(const testing::TestParamInfo<ParamType>& info) {
  bool protected_mem = std::get<0>(info.param);
  bool validation_before = std::get<1>(info.param);
  return std::string() + (protected_mem ? "Protected" : "Unprotected") +
         (validation_before ? "ValidationBefore" : "ValidationAfter");
}

class SwapchainTest : public ::testing::TestWithParam<ParamType> {
 protected:
  void SetUp() override {
    std::vector<const char*> instance_layers;
    if (validation_before_layer()) {
      instance_layers.push_back("VK_LAYER_KHRONOS_validation");
    }
    instance_layers.push_back("VK_LAYER_FUCHSIA_imagepipe_swapchain_fb");

    auto test = std::make_unique<TestSwapchain>(instance_layers, use_protected_memory());
    if (use_protected_memory() && !test->protected_memory_is_supported_) {
      GTEST_SKIP();
    }
    ASSERT_TRUE(test->init_);

    test_ = std::move(test);
  }

  void TearDown() override {
    for (auto& semaphore : single_use_semaphores_) {
      vkDestroySemaphore(test_->vk_device_, semaphore, nullptr);
    }
  }

  bool use_protected_memory() { return std::get<0>(GetParam()); }

  bool validation_before_layer() { return std::get<1>(GetParam()); }

  VkSemaphore MakeSingleUseSemaphore() {
    VkSemaphore semaphore;
    VkSemaphoreCreateInfo semaphore_create_info{.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
    EXPECT_EQ(VK_SUCCESS,
              vkCreateSemaphore(test_->vk_device_, &semaphore_create_info, nullptr, &semaphore));
    single_use_semaphores_.push_back(semaphore);
    return semaphore;
  }

  std::unique_ptr<TestSwapchain> test_;
  std::vector<VkSemaphore> single_use_semaphores_;
};

TEST_P(SwapchainTest, Surface) { test_->Surface(false); }

TEST_P(SwapchainTest, SurfaceDynamicSymbol) { test_->Surface(true); }

TEST_P(SwapchainTest, Create) {
  test_->CreateSwapchain(1, VK_FORMAT_B8G8R8A8_UNORM, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT);
}

TEST_P(SwapchainTest, CreateTwice) {
  test_->CreateSwapchain(2, VK_FORMAT_B8G8R8A8_UNORM, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT);
}

TEST_P(SwapchainTest, CreateForStorage) {
  // TODO(60853): STORAGE usage is currently not supported by FEMU Vulkan ICD.
  if (GetVkPhysicalDeviceType(test_->vk_physical_device_) == VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU) {
    GTEST_SKIP();
  }

  if (GetVkPhysicalDeviceVendorID(test_->vk_physical_device_) == kVendorIDIntel) {
    // TODO(https://fxbug.dev/42163990): STORAGE usage isn't supported by Intel.
    GTEST_SKIP();
  }

  test_->CreateSwapchain(1, VK_FORMAT_B8G8R8A8_UNORM, VK_IMAGE_USAGE_STORAGE_BIT);
}

TEST_P(SwapchainTest, CreateForRgbaStorage) {
  // TODO(60853): STORAGE usage is currently not supported by FEMU Vulkan ICD.
  if (GetVkPhysicalDeviceType(test_->vk_physical_device_) == VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU) {
    GTEST_SKIP();
  }
  test_->CreateSwapchain(1, VK_FORMAT_R8G8B8A8_UNORM, VK_IMAGE_USAGE_STORAGE_BIT);
}

TEST_P(SwapchainTest, CreateForSrgb) {
  test_->CreateSwapchain(1, VK_FORMAT_B8G8R8A8_SRGB, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT);
}

TEST_P(SwapchainTest, AcquireFence) {
  VkImagePipeSurfaceCreateInfoFUCHSIA create_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA,
      .pNext = nullptr,
      .imagePipeHandle = ZX_HANDLE_INVALID,
  };

  VkSurfaceKHR surface;
  EXPECT_EQ(VK_SUCCESS,
            vkCreateImagePipeSurfaceFUCHSIA(test_->vk_instance_, &create_info, nullptr, &surface));

  VkSwapchainKHR swapchain;
  EXPECT_EQ(VK_SUCCESS,
            test_->CreateSwapchainHelper(surface, VK_FORMAT_R8G8B8A8_UNORM,
                                         VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, &swapchain));

  VkFence fence;
  VkFenceCreateInfo info{
      .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, .pNext = nullptr, .flags = 0};
  vkCreateFence(test_->vk_device_, &info, nullptr, &fence);

  // The swapchain itself outputs an error when it receives a fence.
  test_->allows_validation_errors_ = true;
  uint32_t image_index;
  EXPECT_EQ(VK_ERROR_DEVICE_LOST,
            test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0, VK_NULL_HANDLE, fence,
                                           &image_index));
  test_->allows_validation_errors_ = false;
  vkDestroyFence(test_->vk_device_, fence, nullptr);

  test_->destroy_swapchain_khr_(test_->vk_device_, swapchain, nullptr);
  vkDestroySurfaceKHR(test_->vk_instance_, surface, nullptr);
}

INSTANTIATE_TEST_SUITE_P(SwapchainTestSuite, SwapchainTest,
                         ::testing::Combine(::testing::Bool(), ::testing::Bool()), NameFromParam);

class SwapchainFidlTest : public SwapchainTest {};

TEST_P(SwapchainFidlTest, PresentAndAcquireNoSemaphore) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  VkImagePipeSurfaceCreateInfoFUCHSIA create_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA,
      .pNext = nullptr,
      .imagePipeHandle = ZX_HANDLE_INVALID,
  };
  VkSurfaceKHR surface;
  EXPECT_EQ(VK_SUCCESS,
            vkCreateImagePipeSurfaceFUCHSIA(test_->vk_instance_, &create_info, nullptr, &surface));

  VkSwapchainKHR swapchain;
  ASSERT_EQ(VK_SUCCESS,
            test_->CreateSwapchainHelper(surface, VK_FORMAT_B8G8R8A8_UNORM,
                                         VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, &swapchain));

  VkQueue queue;
  if (use_protected_memory()) {
    VkDeviceQueueInfo2 queue_info2 = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_INFO_2,
                                      .pNext = nullptr,
                                      .flags = VK_DEVICE_QUEUE_CREATE_PROTECTED_BIT,
                                      .queueFamilyIndex = 0,
                                      .queueIndex = 0};
    test_->get_device_queue2_(test_->vk_device_, &queue_info2, &queue);
  } else {
    vkGetDeviceQueue(test_->vk_device_, 0, 0, &queue);
  }

  // Supplying neither fences nor semaphores is against the vulkan spec.
  test_->allows_validation_errors_ = true;
  uint32_t image_index;
  // Acquire all initial images.
  for (uint32_t i = 0; i < 3; i++) {
    EXPECT_EQ(VK_SUCCESS,
              test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0, VK_NULL_HANDLE,
                                             VK_NULL_HANDLE, &image_index));
    EXPECT_EQ(i, image_index);
  }

  EXPECT_EQ(VK_NOT_READY,
            test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0, VK_NULL_HANDLE,
                                           VK_NULL_HANDLE, &image_index));

  test_->allows_validation_errors_ = false;

  uint32_t present_index;  // Initialized below.
  VkResult present_result;
  VkPresentInfoKHR present_info = {
      .sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
      .pNext = nullptr,
      .waitSemaphoreCount = 0,
      .pWaitSemaphores = nullptr,
      .swapchainCount = 1,
      .pSwapchains = &swapchain,
      .pImageIndices = &present_index,
      .pResults = &present_result,
  };

  constexpr uint32_t kFrameCount = 100;
  for (uint32_t i = 0; i < kFrameCount; i++) {
    present_index = i % 3;
    auto swapchain_images = test_->GetSwapchainImages(swapchain);
    test_->TransitionLayout(swapchain_images[present_index], VK_IMAGE_LAYOUT_PRESENT_SRC_KHR);
    ASSERT_EQ(VK_SUCCESS, test_->queue_present_khr_(queue, &present_info));

    // Flatland only releases images after a subsequent Present(), so the first image won't be
    // released right away.
    if (i == 0) {
      continue;
    }

    // Supplying neither fences nor semaphores is against the vulkan spec.
    test_->allows_validation_errors_ = true;
    constexpr uint64_t kTimeoutNs = std::chrono::nanoseconds(std::chrono::seconds(10)).count();
    EXPECT_EQ(VK_SUCCESS,
              test_->acquire_next_image_khr_(test_->vk_device_, swapchain, kTimeoutNs,
                                             VK_NULL_HANDLE, VK_NULL_HANDLE, &image_index));
    // The previous present image should be available.
    ASSERT_EQ(present_index, (image_index + 1) % 3);

    ASSERT_EQ(VK_NOT_READY,
              test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0, VK_NULL_HANDLE,
                                             VK_NULL_HANDLE, &image_index));
    test_->allows_validation_errors_ = false;
  }

  test_->destroy_swapchain_khr_(test_->vk_device_, swapchain, nullptr);
  vkDestroySurfaceKHR(test_->vk_instance_, surface, nullptr);
}

TEST_P(SwapchainFidlTest, ForceQuit) {
  // TODO(https://fxbug.dev/383660387): This test case is flaky and may cause
  // a host crash on emulators with SwiftShader. Thus, we disable it on
  // emulators.
  if (GetVkPhysicalDeviceType(test_->vk_physical_device_) == VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU) {
    GTEST_SKIP();
  }

  VkImagePipeSurfaceCreateInfoFUCHSIA create_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA,
      .pNext = nullptr,
      .imagePipeHandle = ZX_HANDLE_INVALID,
  };
  VkSurfaceKHR surface;
  EXPECT_EQ(VK_SUCCESS,
            vkCreateImagePipeSurfaceFUCHSIA(test_->vk_instance_, &create_info, nullptr, &surface));

  VkSwapchainKHR swapchain;
  ASSERT_EQ(VK_SUCCESS,
            test_->CreateSwapchainHelper(surface, VK_FORMAT_B8G8R8A8_UNORM,
                                         VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, &swapchain));

  VkQueue queue;
  if (use_protected_memory()) {
    VkDeviceQueueInfo2 queue_info2 = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_INFO_2,
                                      .pNext = nullptr,
                                      .flags = VK_DEVICE_QUEUE_CREATE_PROTECTED_BIT,
                                      .queueFamilyIndex = 0,
                                      .queueIndex = 0};
    test_->get_device_queue2_(test_->vk_device_, &queue_info2, &queue);
  } else {
    vkGetDeviceQueue(test_->vk_device_, 0, 0, &queue);
  }

  uint32_t image_index;
  EXPECT_EQ(VK_SUCCESS,
            test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0,
                                           MakeSingleUseSemaphore(), VK_NULL_HANDLE, &image_index));

  auto swapchain_images = test_->GetSwapchainImages(swapchain);
  test_->TransitionLayout(swapchain_images[image_index], VK_IMAGE_LAYOUT_PRESENT_SRC_KHR);

  uint32_t present_index = image_index;
  VkResult present_result;
  VkPresentInfoKHR present_info = {
      .sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
      .pNext = nullptr,
      .waitSemaphoreCount = 0,
      .pWaitSemaphores = nullptr,
      .swapchainCount = 1,
      .pSwapchains = &swapchain,
      .pImageIndices = &present_index,
      .pResults = &present_result,
  };

  ASSERT_EQ(VK_SUCCESS, test_->queue_present_khr_(queue, &present_info));

  test_->destroy_swapchain_khr_(test_->vk_device_, swapchain, nullptr);
  vkDestroySurfaceKHR(test_->vk_instance_, surface, nullptr);
}

TEST_P(SwapchainFidlTest, AcquireZeroTimeout) {
  // TODO(https://fxbug.dev/383660387): This test case is flaky and may cause
  // a host crash on emulators with SwiftShader. Thus, we disable it on
  // emulators.
  if (GetVkPhysicalDeviceType(test_->vk_physical_device_) == VK_PHYSICAL_DEVICE_TYPE_VIRTUAL_GPU) {
    GTEST_SKIP();
  }

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  VkImagePipeSurfaceCreateInfoFUCHSIA create_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA,
      .pNext = nullptr,
      .imagePipeHandle = ZX_HANDLE_INVALID,
  };

  VkSurfaceKHR surface;
  EXPECT_EQ(VK_SUCCESS,
            vkCreateImagePipeSurfaceFUCHSIA(test_->vk_instance_, &create_info, nullptr, &surface));

  VkSwapchainKHR swapchain;
  ASSERT_EQ(VK_SUCCESS,
            test_->CreateSwapchainHelper(surface, VK_FORMAT_B8G8R8A8_UNORM,
                                         VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, &swapchain));

  VkQueue queue;
  if (use_protected_memory()) {
    VkDeviceQueueInfo2 queue_info2 = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_INFO_2,
                                      .pNext = nullptr,
                                      .flags = VK_DEVICE_QUEUE_CREATE_PROTECTED_BIT,
                                      .queueFamilyIndex = 0,
                                      .queueIndex = 0};
    test_->get_device_queue2_(test_->vk_device_, &queue_info2, &queue);
  } else {
    vkGetDeviceQueue(test_->vk_device_, 0, 0, &queue);
  }
  uint32_t image_index;
  // Acquire all initial images.
  for (uint32_t i = 0; i < 3; i++) {
    EXPECT_EQ(VK_SUCCESS, test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0,
                                                         MakeSingleUseSemaphore(), VK_NULL_HANDLE,
                                                         &image_index));
    EXPECT_EQ(i, image_index);
  }

  // Timeout of zero with no pending presents
  EXPECT_EQ(VK_NOT_READY,
            test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0,
                                           MakeSingleUseSemaphore(), VK_NULL_HANDLE, &image_index));

  uint32_t present_index = 0;
  auto swapchain_images = test_->GetSwapchainImages(swapchain);
  test_->TransitionLayout(swapchain_images[present_index], VK_IMAGE_LAYOUT_PRESENT_SRC_KHR);
  VkResult present_result;
  VkPresentInfoKHR present_info = {
      .sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
      .pNext = nullptr,
      .waitSemaphoreCount = 0,
      .pWaitSemaphores = nullptr,
      .swapchainCount = 1,
      .pSwapchains = &swapchain,
      .pImageIndices = &present_index,
      .pResults = &present_result,
  };

  ASSERT_EQ(VK_SUCCESS, test_->queue_present_khr_(queue, &present_info));

  {
    // It's a validation error to not specify a fence or semaphore.
    test_->allows_validation_errors_ = true;
    // Timeout of zero with pending presents
    VkResult result = test_->acquire_next_image_khr_(test_->vk_device_, swapchain, 0,
                                                     VK_NULL_HANDLE, VK_NULL_HANDLE, &image_index);
    EXPECT_EQ(result, VK_NOT_READY);
    test_->allows_validation_errors_ = false;
  }

  test_->destroy_swapchain_khr_(test_->vk_device_, swapchain, nullptr);
  vkDestroySurfaceKHR(test_->vk_instance_, surface, nullptr);
}

INSTANTIATE_TEST_SUITE_P(SwapchainFidlTestSuite, SwapchainFidlTest,
                         ::testing::Combine(::testing::Bool(), ::testing::Bool()), NameFromParam);
