// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/component_manager.h"

#include "src/developer/debug/debug_agent/process_handle.h"
#include "src/developer/debug/debug_agent/system_interface.h"

namespace debug_agent {

std::vector<debug_ipc::ComponentInfo> ComponentManager::FindComponentInfo(
    const ProcessHandle& process) const {
  zx_koid_t job_koid = process.GetJobKoid();
  while (job_koid) {
    auto components = FindComponentInfo(job_koid);
    if (!components.empty())
      return components;
    job_koid = system_interface_->GetParentJobKoid(job_koid);
  }
  return {};
}

}  // namespace debug_agent
