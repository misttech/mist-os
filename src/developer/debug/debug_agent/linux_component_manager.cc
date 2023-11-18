// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/linux_component_manager.h"

namespace debug_agent {

std::vector<debug_ipc::ComponentInfo> LinuxComponentManager::FindComponentInfo(
    zx_koid_t job_koid) const {
  return {};
}

void LinuxComponentManager::SetDebugAgent(DebugAgent* debug_agent) {}

debug::Status LinuxComponentManager::LaunchComponent(std::string url) {
  return debug::Status("No components on Linux");
}

debug::Status LinuxComponentManager::LaunchTest(std::string url, std::optional<std::string> realm,
                                                std::vector<std::string> case_filters) {
  return debug::Status("No components on Linux");
}

bool LinuxComponentManager::OnProcessStart(const ProcessHandle& process, StdioHandles* out_stdio,
                                           std::string* process_name_override) {
  return false;
}

}  // namespace debug_agent
