// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "surface.h"

#include "utils.h"

namespace vkp {

Surface::Surface(std::shared_ptr<vk::Instance> instance, GLFWwindow *window)
    : initialized_(false), instance_(instance), window_(window) {}

Surface::~Surface() {
  if (initialized_) {
    vkDestroySurfaceKHR(*instance_, surface_, nullptr);
  }
}

bool Surface::Init() {
  if (initialized_) {
    RTN_MSG(false, "vkp::Surface::Init() failed - already initialized.\n");
  }
  if (!instance_.get()) {
    RTN_MSG(false, "vkp::Surface::Init() failed - must provide instance and window.\n");
  }
  auto rv = glfwCreateWindowSurface(*instance_, window_, nullptr, &surface_);

  if (rv != VK_SUCCESS) {
    RTN_MSG(false, "GLFW surface creation failed.\n");
  }

  initialized_ = true;
  return true;
}

}  // namespace vkp
