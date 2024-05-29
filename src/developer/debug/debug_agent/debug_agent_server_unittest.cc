// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent_server.h"

#include <gtest/gtest.h>

#include "src/developer/debug/debug_agent/mock_debug_agent_harness.h"
#include "src/developer/debug/debug_agent/mock_process_handle.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/developer/debug/shared/test_with_loop.h"

namespace debug_agent {

// This class is a friend of DebugAgentServer so that we may test the private, non-FIDL APIs
// directly. Those APIs are designed to expose as
class DebugAgentServerTest : public debug::TestWithLoop {
 public:
  DebugAgentServerTest()
      : server_(harness_.debug_agent()->GetWeakPtr(),
                debug::MessageLoopFuchsia::Current()->dispatcher()) {}

  DebugAgentServer::AddFilterResult AddFilter(const fuchsia_debugger::Filter& filter) {
    return server_.AddFilter(filter);
  }

  uint32_t AttachToMatchingKoids(const debug_ipc::UpdateFilterReply& reply) {
    std::vector<zx_koid_t> koids;
    for (const auto& match : reply.matched_processes_for_filter) {
      koids.insert(koids.end(), match.matched_pids.begin(), match.matched_pids.end());
    }
    return server_.AttachToKoids(koids);
  }

  debug_ipc::StatusReply GetAgentStatus() {
    debug_ipc::StatusReply reply;
    harness_.debug_agent()->OnStatus({}, &reply);
    return reply;
  }

  MockDebugAgentHarness* harness() { return &harness_; }
  DebugAgent* GetDebugAgent() { return harness_.debug_agent(); }
  DebugAgentServer* server() { return &server_; }

 private:
  MockDebugAgentHarness harness_;
  DebugAgentServer server_;
};

TEST_F(DebugAgentServerTest, AddNewFilter) {
  DebugAgent* agent = GetDebugAgent();

  auto status_reply = GetAgentStatus();

  // There shouldn't be any installed filters yet.
  ASSERT_EQ(status_reply.filters.size(), 0u);

  fuchsia_debugger::Filter first;
  // This will match job koid 25 from mock_system_interface.
  first.pattern("fixed/moniker");
  first.type(fuchsia_debugger::FilterType::kMonikerSuffix);

  auto result = AddFilter(first);
  EXPECT_TRUE(result.ok());

  auto reply = result.take_value();

  // There should be one reported match.
  EXPECT_EQ(reply.matched_processes_for_filter.size(), 1u);
  EXPECT_EQ(reply.matched_processes_for_filter[0].matched_pids.size(), 1u);

  // Now attach to the matching koid.
  EXPECT_EQ(AttachToMatchingKoids(reply), 1u);

  status_reply = GetAgentStatus();

  EXPECT_EQ(status_reply.filters.size(), 1u);
  EXPECT_EQ(status_reply.filters[0].pattern, first.pattern());
  EXPECT_EQ(status_reply.filters[0].type, debug_ipc::Filter::Type::kComponentMonikerSuffix);
  // The recursive flag was left unspecified, which should leave the default value of false in the
  // debug_ipc filter.
  EXPECT_EQ(status_reply.filters[0].recursive, false);
  EXPECT_EQ(status_reply.processes.size(), 1u);
  EXPECT_EQ(status_reply.processes[0].process_koid,
            reply.matched_processes_for_filter[0].matched_pids[0]);

  // Corresponds to the koid of the process under the "fixed/moniker" component.
  constexpr zx_koid_t kProcessKoid = 26;
  EXPECT_NE(agent->GetDebuggedProcess(kProcessKoid), nullptr);

  // Simulate a test environment rooted in the collection "root" with name "test". A recursive
  // moniker suffix filter on "root:test" will implicitly install a second moniker prefix filter for
  // the entire moniker up to and including "root:test" so that any child components spawned within
  // its realm will be attached to. We don't need to know the moniker of any child components in
  // order to attach to any processes they contain.
  constexpr char kFullRootMoniker[] = "/moniker/generated/root:test";

  fuchsia_debugger::Filter second;
  second.pattern("root:test");
  second.type(fuchsia_debugger::FilterType::kMonikerSuffix);
  second.options().recursive(true);

  result = AddFilter(second);
  EXPECT_TRUE(result.ok());

  reply = result.take_value();

  // Updating the filter will give us back the first match, but we need to receive a component
  // discovered event to match with the routing component that doesn't have an associated ELF
  // program. The only match should be the process that matched the first filter.
  EXPECT_EQ(reply.matched_processes_for_filter.size(), 1u);
  EXPECT_EQ(reply.matched_processes_for_filter[0].matched_pids.size(), 1u);
  // It should have already been attached when we previously matched.
  EXPECT_NE(agent->GetDebuggedProcess(reply.matched_processes_for_filter[0].matched_pids[0]),
            nullptr);
  EXPECT_EQ(reply.matched_processes_for_filter[0].matched_pids.size(), 1u);
  EXPECT_EQ(reply.matched_processes_for_filter[0].matched_pids[0], kProcessKoid);

  // Inject a component discovery event so the second filter attaches to the root component that
  // doesn't have an ELF program running with it. This will install the subsequent moniker prefix
  // filter that will be used to match a child component with an ELF process.
  harness()->system_interface()->mock_component_manager().InjectComponentEvent(
      FakeEventType::kDiscovered, kFullRootMoniker,
      "fuchsia-pkg://devhost/root_package#meta/root_component.cm");

  status_reply = GetAgentStatus();

  // Should have an extra filter now, which is a moniker prefix filter on the given moniker above.
  ASSERT_EQ(status_reply.filters.size(), 3u);
  EXPECT_EQ(status_reply.filters[2].pattern, kFullRootMoniker);
  EXPECT_EQ(status_reply.filters[2].type, debug_ipc::Filter::Type::kComponentMonikerPrefix);
  EXPECT_EQ(status_reply.filters[2].recursive, false);

  // Koid of job4 from MockSystemInterface.
  constexpr zx_koid_t kJob4Koid = 32;
  // Inject a process starting event for the ELF process running under some child component of the
  // root component above.
  constexpr zx_koid_t kProcess2Koid = 33;
  auto handle = std::make_unique<MockProcessHandle>(kProcess2Koid);
  // Set the job koid so that we can look up the corresponding component information.
  handle->set_job_koid(kJob4Koid);
  agent->OnProcessStarting(std::move(handle));

  status_reply = GetAgentStatus();

  // Now we should have also attached to the new process that matched the implicit moniker prefix
  // filter that was installed above.
  EXPECT_EQ(status_reply.processes.size(), 2u);

  EXPECT_NE(agent->GetDebuggedProcess(kProcess2Koid), nullptr);
}

TEST_F(DebugAgentServerTest, AddFilterErrors) {
  fuchsia_debugger::Filter f;

  auto result = AddFilter(f);
  EXPECT_TRUE(result.has_error());
  EXPECT_EQ(result.err(), fuchsia_debugger::FilterError::kNoPattern);

  // Set pattern but not type.
  f.pattern("test");

  result = AddFilter(f);
  EXPECT_TRUE(result.has_error());
  EXPECT_EQ(result.err(), fuchsia_debugger::FilterError::kUnknownType);

  // Some filter type from the future.
  f.type(static_cast<fuchsia_debugger::FilterType>(1234));

  result = AddFilter(f);
  EXPECT_TRUE(result.has_error());
  EXPECT_EQ(result.err(), fuchsia_debugger::FilterError::kUnknownType);
}

}  // namespace debug_agent
