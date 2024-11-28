// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_MOCK_COMPONENT_MANAGER_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_MOCK_COMPONENT_MANAGER_H_

#include <map>

#include "src/developer/debug/debug_agent/component_manager.h"
#include "src/developer/debug/debug_agent/debug_agent.h"

namespace debug_agent {

enum class FakeEventType : uint32_t {
  kDebugStarted = 0,
  kStopped,
  kLast,
};

class MockComponentManager : public ComponentManager {
 public:
  explicit MockComponentManager(SystemInterface* system_interface)
      : ComponentManager(system_interface) {}
  ~MockComponentManager() override = default;

  auto& component_info() { return component_info_; }

  // ComponentManager implementation.
  void SetDebugAgent(DebugAgent* agent) override { debug_agent_ = agent; }

  std::vector<debug_ipc::ComponentInfo> FindComponentInfo(zx_koid_t job_koid) const override {
    auto [start, end] = component_info_.equal_range(job_koid);
    if (start == component_info_.end()) {
      // Not found.
      return {};
    }

    std::vector<debug_ipc::ComponentInfo> components;
    components.reserve(std::distance(start, end));
    for (auto& i = start; i != end; ++i) {
      components.push_back(i->second);
    }
    return components;
  }

  void InjectComponentEvent(FakeEventType type, const std::string& moniker,
                            const std::string& url) {
    switch (type) {
      case FakeEventType::kDebugStarted: {
        debug_agent_->OnComponentStarted(moniker, url);
        break;
      }
      case FakeEventType::kStopped: {
        debug_agent_->OnComponentExited(moniker, url);
        break;
      }
      default: {
        FX_NOTREACHED();
        break;
      }
    }
  }

  debug::Status LaunchComponent(std::string url) override { return debug::Status("Not supported"); }

  debug::Status LaunchTest(std::string url, std::optional<std::string> realm,
                           std::vector<std::string> case_filters) override {
    return debug::Status("Not supported");
  }

  bool OnProcessStart(const ProcessHandle& process, StdioHandles* out_stdio,
                      std::string* process_name_override) override {
    return false;
  }

 private:
  DebugAgent* debug_agent_;
  std::multimap<zx_koid_t, debug_ipc::ComponentInfo> component_info_;
};

}  // namespace debug_agent

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_MOCK_COMPONENT_MANAGER_H_
