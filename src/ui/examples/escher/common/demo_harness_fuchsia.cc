// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/examples/escher/common/demo_harness_fuchsia.h"

#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/zx/time.h>

#include <memory>

#include <hid/usages.h>

#include "lib/vfs/cpp/pseudo_dir.h"
#include "src/ui/examples/escher/common/demo.h"
#include "src/ui/lib/escher/util/trace_macros.h"

// Directory provided by the "isolated-cache-storage" feature of the component sandbox.
static const char* kCacheDirectoryPath = "/cache";

// When running on Fuchsia, New() instantiates a DemoHarnessFuchsia.
std::unique_ptr<DemoHarness> DemoHarness::New(DemoHarness::WindowParams window_params,
                                              DemoHarness::InstanceParams instance_params) {
  auto harness = new DemoHarnessFuchsia(nullptr, window_params);
  harness->Init(std::move(instance_params));
  return std::unique_ptr<DemoHarness>(harness);
}

DemoHarnessFuchsia::DemoHarnessFuchsia(async::Loop* loop, WindowParams window_params)
    : DemoHarness(window_params),
      owned_loop_(loop ? nullptr : new async::Loop(&kAsyncLoopConfigAttachToCurrentThread)),
      loop_(loop ? loop : owned_loop_.get()),
      component_context_(sys::ComponentContext::CreateAndServeOutgoingDirectory()) {
  // Provide a PseudoDir where the demo can register debugging services.
  auto debug_dir = std::make_shared<vfs::PseudoDir>();
  component_context()->outgoing()->debug_dir()->AddSharedEntry("demo", debug_dir);
  filesystem_ = escher::HackFilesystem::New(debug_dir);

  // Synchronously create trace provider so that all subsequent traces are recorded.  This is
  // necessary e.g. if the system is already tracing when this app is launched, in order to not miss
  // trace events that occur during startup.
  bool already_started = false;
  trace::TraceProviderWithFdio::CreateSynchronously(loop_->dispatcher(), "Escher DemoHarness",
                                                    &trace_provider_, &already_started);
}

std::string DemoHarnessFuchsia::GetCacheDirectoryPath() { return kCacheDirectoryPath; }

// TODO(fxbug.dev/124389): Support input via /dev/class/input-report.
void DemoHarnessFuchsia::InitWindowSystem() {}

vk::SurfaceKHR DemoHarnessFuchsia::CreateWindowAndSurface(const WindowParams& params) {
  VkImagePipeSurfaceCreateInfoFUCHSIA create_info = {
      .sType = VK_STRUCTURE_TYPE_IMAGEPIPE_SURFACE_CREATE_INFO_FUCHSIA,
      .pNext = nullptr,
  };
  VkSurfaceKHR surface;
  VkResult err = vkCreateImagePipeSurfaceFUCHSIA(instance(), &create_info, nullptr, &surface);
  FX_CHECK(!err);
  return surface;
}

void DemoHarnessFuchsia::AppendPlatformSpecificInstanceExtensionNames(InstanceParams* params) {
  params->extension_names.insert(VK_KHR_SURFACE_EXTENSION_NAME);
  params->extension_names.insert(VK_FUCHSIA_IMAGEPIPE_SURFACE_EXTENSION_NAME);
  params->extension_names.insert(VK_KHR_EXTERNAL_MEMORY_CAPABILITIES_EXTENSION_NAME);
  params->extension_names.insert(VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME);
  params->layer_names.insert("VK_LAYER_FUCHSIA_imagepipe_swapchain_fb");
}

void DemoHarnessFuchsia::AppendPlatformSpecificDeviceExtensionNames(std::set<std::string>* names) {
  names->insert(VK_FUCHSIA_EXTERNAL_MEMORY_EXTENSION_NAME);
}

void DemoHarnessFuchsia::ShutdownWindowSystem() {}

void DemoHarnessFuchsia::RunForPlatform(Demo* demo) {
  // We put in a delay so that tracing is ready to capture the first frame (otherwise we miss the
  // first frame and catch the second).
  async::PostDelayedTask(
      loop_->dispatcher(), [this, demo] { this->RenderFrameOrQuit(demo); }, zx::msec(1));
  loop_->Run();
}

void DemoHarnessFuchsia::RenderFrameOrQuit(Demo* demo) {
  if (ShouldQuit()) {
    loop_->Quit();
    device().waitIdle();
  } else {
    MaybeDrawFrame();
    async::PostDelayedTask(
        loop_->dispatcher(), [this, demo] { this->RenderFrameOrQuit(demo); }, zx::msec(1));
  }
}
