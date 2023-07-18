// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/bin/vulkan_loader/lavapipe_device.h"

#include <lib/fdio/directory.h>
#include <lib/fit/thread_checker.h>
#include <lib/syslog/cpp/macros.h>

#include "src/graphics/bin/vulkan_loader/app.h"

// static
std::unique_ptr<LavapipeDevice> LavapipeDevice::Create(LoaderApp* app, const std::string& name,
                                                       inspect::Node* parent) {
  std::unique_ptr<LavapipeDevice> device(new LavapipeDevice(app));
  if (!device->Initialize(name, parent))
    return nullptr;
  return device;
}

bool LavapipeDevice::Initialize(const std::string& name, inspect::Node* parent) {
  FIT_DCHECK_IS_THREAD_VALID(main_thread_);
  node() = parent->CreateChild("lavapipe-" + name);
  icd_list_.Initialize(&node());
  auto pending_action_token = app()->GetPendingActionToken();

  auto data = node().CreateChild(name);
  std::string component_url = "fuchsia-pkg://fuchsia.com/libvulkan_lavapipe#meta/vulkan.cm";
  data.RecordString("component_url", component_url);

  icd_list_.Add(app()->CreateIcdComponent(component_url));
  icds().push_back(std::move(data));

  return true;
}
