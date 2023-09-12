// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_UTILS_ESCHER_PROVIDER_H_
#define SRC_UI_SCENIC_LIB_UTILS_ESCHER_PROVIDER_H_

#include <lib/sys/cpp/component_context.h>

#include "src/ui/lib/escher/escher.h"

#include <vulkan/vulkan.hpp>

namespace utils {

escher::EscherUniquePtr CreateEscher(sys::ComponentContext* app_context);

VkBool32 HandleDebugReport(VkDebugReportFlagsEXT flags_in,
                           VkDebugReportObjectTypeEXT object_type_in, uint64_t object,
                           size_t location, int32_t message_code, const char* pLayerPrefix,
                           const char* pMessage, void* pUserData);

}  // namespace utils

#endif  // SRC_UI_SCENIC_LIB_UTILS_ESCHER_PROVIDER_H_
