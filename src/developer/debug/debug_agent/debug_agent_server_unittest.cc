// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent_server.h"

#include <algorithm>
#include <memory>

#include <gtest/gtest.h>

#include "lib/async/default.h"
#include "src/developer/debug/debug_agent/mock_component_manager.h"
#include "src/developer/debug/debug_agent/mock_debug_agent_harness.h"
#include "src/developer/debug/debug_agent/mock_process.h"
#include "src/developer/debug/debug_agent/mock_process_handle.h"
#include "src/developer/debug/debug_agent/mock_thread.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/developer/debug/shared/message_loop.h"
#include "src/developer/debug/shared/test_with_loop.h"

namespace debug_agent {

// This class is a friend of DebugAgentServer so that we may test the private, non-FIDL APIs
// directly.
class DebugAgentServerTest : public debug::TestWithLoop {
 public:
  DebugAgentServerTest()
      : server_(harness_.debug_agent()->GetWeakPtr(),
                debug::MessageLoopFuchsia::Current()->dispatcher()) {}

  DebugAgentServer::AddFilterResult AddFilter(const fuchsia_debugger::Filter& filter) {
    return server_.AddFilter(filter);
  }

  uint32_t AttachToMatchingKoids(const debug_ipc::UpdateFilterReply& reply) {
    return server_.AttachToFilterMatches(reply.matched_processes_for_filter);
  }

  auto GetMatchingProcesses(std::optional<fuchsia_debugger::Filter> filter) {
    return server_.GetMatchingProcesses(std::move(filter));
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
  EXPECT_EQ(status_reply.filters[0].config.recursive, false);
  EXPECT_EQ(status_reply.processes.size(), 1u);
  EXPECT_EQ(status_reply.processes[0].process_koid,
            reply.matched_processes_for_filter[0].matched_pids[0]);

  // Run the loop so the process has a chance to update its thread list.
  loop().RunUntilNoTasks();

  auto proc = agent->GetDebuggedProcess(reply.matched_processes_for_filter[0].matched_pids[0]);
  auto thread_records = proc->GetThreadRecords();

  ASSERT_FALSE(thread_records.empty());

  // No threads should be suspended because we should have attached weakly.
  for (auto& record : thread_records) {
    EXPECT_EQ(record.state, debug_ipc::ThreadRecord::State::kRunning);
  }

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

  // Inject a component starting event so the second filter attaches to the root component that
  // doesn't have an ELF program running with it. This will install the subsequent moniker prefix
  // filter that will be used to match a child component with an ELF process.
  harness()->system_interface()->mock_component_manager().InjectComponentEvent(
      FakeEventType::kDebugStarted, kFullRootMoniker,
      "fuchsia-pkg://devhost/root_package#meta/root_component.cm");

  status_reply = GetAgentStatus();

  // Should have an extra filter now, which is a moniker prefix filter on the given moniker above.
  ASSERT_EQ(status_reply.filters.size(), 3u);
  EXPECT_EQ(status_reply.filters[2].pattern, kFullRootMoniker);
  EXPECT_EQ(status_reply.filters[2].type, debug_ipc::Filter::Type::kComponentMonikerPrefix);
  EXPECT_EQ(status_reply.filters[2].config.recursive, false);

  // Koid of job4 from MockSystemInterface.
  constexpr zx_koid_t kJob4Koid = 32;
  // Inject a process starting event for the ELF process running under some child component of the
  // root component above.
  constexpr zx_koid_t kProcess2Koid = 33;
  auto handle = std::make_unique<MockProcessHandle>(kProcess2Koid);
  // Set the job koid so that we can look up the corresponding component information.
  handle->set_job_koid(kJob4Koid);
  agent->OnProcessChanged(DebugAgent::ProcessChangedHow::kStarting, std::move(handle));

  status_reply = GetAgentStatus();

  // Now we should have also attached to the new process that matched the implicit moniker prefix
  // filter that was installed above.
  EXPECT_EQ(status_reply.processes.size(), 2u);

  EXPECT_NE(agent->GetDebuggedProcess(kProcess2Koid), nullptr);

  // Now we install a job-only filter that will attach DebugAgent directly to a matching job's
  // exception channel.
  fuchsia_debugger::Filter third;
  third.type(fuchsia_debugger::FilterType::kMonikerPrefix);
  // Component moniker associated with job5 in mock_system_interface.
  third.pattern("/some");
  third.options().job_only(true);

  result = AddFilter(third);
  EXPECT_TRUE(result.ok());

  reply = result.take_value();

  auto third_filter_match = std::ranges::find_if(reply.matched_processes_for_filter,
                                                 [](const debug_ipc::FilterMatch& match) {
                                                   if ((match.id & 0xF) == 3)
                                                     return true;

                                                   return false;
                                                 });

  ASSERT_NE(third_filter_match, reply.matched_processes_for_filter.end());
  EXPECT_EQ(third_filter_match->matched_pids.size(), 2u);

  constexpr zx_koid_t kJob5Koid = 35;
  constexpr zx_koid_t kJob51Koid = 38;

  // The order of the matches will always be in ascending order.
  EXPECT_EQ(third_filter_match->matched_pids[0], kJob5Koid);
  EXPECT_EQ(third_filter_match->matched_pids[1], kJob51Koid);

  // Now we test explicit attach requests. This will be the case if the filter is installed after
  // the component that matches is already launched and we have been notified of it. See the
  // job_only tests in debug_agent_unittests to see the case where the filter is matched upon a
  // notification that a component is starting.
  debug_ipc::AttachRequest attach_request;
  attach_request.koid = kJob5Koid;
  attach_request.config.target = debug_ipc::AttachConfig::Target::kJob;
  attach_request.config.weak = false;

  debug_ipc::AttachReply attach_reply;
  agent->OnAttach(attach_request, &attach_reply);
  ASSERT_TRUE(attach_reply.status.ok()) << attach_reply.status.message();

  attach_request.koid = kJob51Koid;

  agent->OnAttach(attach_request, &attach_reply);
  ASSERT_TRUE(attach_reply.status.has_error());
  ASSERT_EQ(attach_reply.status.type(), debug::Status::kAlreadyExists);

  EXPECT_TRUE(agent->GetDebuggedJob(kJob5Koid));
  // Should not be attached to the child job.
  EXPECT_FALSE(agent->GetDebuggedJob(kJob51Koid));
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

  // recursive and job_only options are mutually exclusive.
  f.type(fuchsia_debugger::FilterType::kMoniker);
  f.options().recursive(true);
  f.options().job_only(true);
  result = AddFilter(f);
  EXPECT_TRUE(result.has_error());
  EXPECT_EQ(result.err(), fuchsia_debugger::FilterError::kInvalidOptions);
}

TEST_F(DebugAgentServerTest, GetMatchingProcesses) {
  auto agent = GetDebugAgent();

  // Not passing a filter will return all attached processes. There aren't any of those yet, so the
  // return value is empty.
  auto result = GetMatchingProcesses(std::nullopt);
  EXPECT_TRUE(result.ok());
  EXPECT_TRUE(result.value().empty());

  // If provided, the filter must be valid.
  result = GetMatchingProcesses({{{.pattern = ""}}});
  EXPECT_TRUE(result.has_error());
  EXPECT_EQ(result.err(), fuchsia_debugger::FilterError::kNoPattern);

  result = GetMatchingProcesses({{{.pattern = "some/pattern"}}});
  EXPECT_TRUE(result.has_error());
  EXPECT_EQ(result.err(), fuchsia_debugger::FilterError::kUnknownType);

  constexpr char kFullRootMoniker[] = "/moniker/generated/root:test";

  // This filter is intended to match |kFullRootMoniker| and all child components.
  fuchsia_debugger::Filter f1;
  f1.pattern("root:test");
  f1.type(fuchsia_debugger::FilterType::kMonikerSuffix);
  f1.options({{.recursive = true}});

  AddFilter(f1);

  // Create the subfilter.
  harness()->system_interface()->mock_component_manager().InjectComponentEvent(
      FakeEventType::kDebugStarted, kFullRootMoniker,
      "fuchsia-pkg://devhost/root_package#meta/root_component.cm");

  // Koid of job4 from MockSystemInterface.
  constexpr zx_koid_t kJob4Koid = 32;
  // Inject a process starting event for the ELF process running under some child component of the
  // root component above.
  constexpr zx_koid_t kProcessKoid = 33;
  auto handle = std::make_unique<MockProcessHandle>(kProcessKoid);
  // Set the job koid so that we can look up the corresponding component information.
  handle->set_job_koid(kJob4Koid);
  agent->OnProcessChanged(DebugAgent::ProcessChangedHow::kStarting, std::move(handle));

  // We are now attached to something. Omitting the filter should give us back a process.
  result = GetMatchingProcesses(std::nullopt);
  EXPECT_TRUE(result.ok());
  EXPECT_EQ(result.value().size(), 1u);
  EXPECT_EQ(result.value()[0]->koid(), kProcessKoid);
}

TEST_F(DebugAgentServerTest, AttachToJobOnComponentStarting) {
  constexpr zx_koid_t kJobKoid = 101;
  constexpr std::string kComponentMoniker = "some/fake/moniker";
  constexpr std::string kComponentUrl = "url";

  fuchsia_debugger::Filter filter;
  filter.pattern("moniker");
  filter.type(fuchsia_debugger::FilterType::kMonikerSuffix);
  filter.options().job_only(true);

  AddFilter(filter);

  harness()->system_interface()->mock_component_manager().InjectComponentEvent(
      FakeEventType::kDebugStarted, kComponentMoniker, kComponentUrl, kJobKoid);

  EXPECT_NE(GetDebugAgent()->GetDebuggedJob(kJobKoid), nullptr);
}

class FakeFidlClient : public fidl::AsyncEventHandler<fuchsia_debugger::DebugAgent> {
 public:
  explicit FakeFidlClient(fidl::ClientEnd<fuchsia_debugger::DebugAgent> client_end,
                          async_dispatcher_t* dispatcher)
      : client_(std::move(client_end), dispatcher, this) {}

  void AttachTo(const fuchsia_debugger::Filter& filter,
                fit::callback<void(fidl::Result<fuchsia_debugger::DebugAgent::AttachTo>&)> cb) {
    client_->AttachTo(filter).Then(
        [cb = std::move(cb)](fidl::Result<fuchsia_debugger::DebugAgent::AttachTo>& reply) mutable {
          ASSERT_TRUE(cb);
          cb(reply);
        });
  }

  void OnFatalException(
      fidl::Event<fuchsia_debugger::DebugAgent::OnFatalException>& event) override {
    exceptions_.emplace_back(event);
    debug::MessageLoop::Current()->QuitNow();
  }

  const auto& GetExceptions() const { return exceptions_; }

  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_debugger::DebugAgent> metadata) override {
    FX_LOGS(WARNING) << "Unknown event: " << metadata.event_ordinal;
  }

 private:
  std::vector<fidl::Event<fuchsia_debugger::DebugAgent::OnFatalException>> exceptions_;
  fidl::Client<fuchsia_debugger::DebugAgent> client_;
};

class DebugAgentServerTestWithClient : public debug::TestWithLoop {
 public:
  void SetUp() override {
    auto [client_end, server_end] = *fidl::CreateEndpoints<fuchsia_debugger::DebugAgent>();
    client_ =
        std::make_unique<FakeFidlClient>(std::move(client_end), async_get_default_dispatcher());

    // The server is owned by the message loop.
    DebugAgentServer::BindServer(async_get_default_dispatcher(), std::move(server_end),
                                 harness()->debug_agent()->GetWeakPtr());
  }

  void TearDown() override { client_.reset(); }

  FakeFidlClient& client() { return *client_; }
  MockDebugAgentHarness* harness() { return &harness_; }
  DebugAgent* agent() { return harness_.debug_agent(); }

 private:
  std::unique_ptr<FakeFidlClient> client_ = nullptr;
  MockDebugAgentHarness harness_;
};

TEST_F(DebugAgentServerTestWithClient, OnFatalException) {
  constexpr zx_koid_t kProcessKoid = 0x1234;
  constexpr zx_koid_t kThreadKoid = 0x2345;
  auto mock_process = harness()->AddProcess(kProcessKoid);
  auto mock_thread = mock_process->AddThread(kThreadKoid);

  // Now that the server is bound to the message loop with a client, we can send the notification.
  // Note that there may be an error from inspector complaining about a process koid that doesn't
  // exist, but that's not important for this test.
  mock_thread->SendException(0x12345678, debug_ipc::ExceptionType::kGeneral);

  loop().Run();

  ASSERT_EQ(client().GetExceptions().size(), 1u);
  EXPECT_TRUE(client().GetExceptions()[0].thread());
  EXPECT_EQ(*client().GetExceptions()[0].thread(), kThreadKoid);
}

// Debug exception types should not send notifications to clients, e.g. single step, software
// breakpoints, etc.
TEST_F(DebugAgentServerTestWithClient, DebugExceptionDoesNotSendEvent) {
  constexpr zx_koid_t kProcessKoid = 0x1234;
  constexpr zx_koid_t kThreadKoid = 0x2345;
  auto mock_process = harness()->AddProcess(kProcessKoid);
  auto mock_thread = mock_process->AddThread(kThreadKoid);

  // clang-format off
  constexpr std::array<debug_ipc::ExceptionType, 5> debug_exceptions = {
    // These are taken from the same set that populates the IsDebug function in ipc/records.cc. We
    // don't need to worry about the process and thread lifetime exceptions that will return true in
    // that function because separate debug_ipc notifications will be sent for those and if we
    // decide to make them !IsDebug then it shouldn't affect this FIDL event.
    debug_ipc::ExceptionType::kHardwareBreakpoint,
    debug_ipc::ExceptionType::kWatchpoint,
    debug_ipc::ExceptionType::kSingleStep,
    debug_ipc::ExceptionType::kSoftwareBreakpoint,
    debug_ipc::ExceptionType::kSynthetic,
  };
  // clang-format on

  constexpr uint64_t kExceptionAddress = 0x12345678;
  for (auto exception_type : debug_exceptions) {
    // Watchpoints need some special set up.
    if (exception_type == debug_ipc::ExceptionType::kWatchpoint) {
      DebugRegisters debug_registers;
      auto wp_info = debug_registers.SetWatchpoint(debug_ipc::BreakpointType::kReadWrite,
                                                   {kExceptionAddress, kExceptionAddress + 1}, 4);
      ASSERT_TRUE(wp_info);
      debug_registers.SetForHitWatchpoint(wp_info->slot);

      mock_thread->mock_thread_handle().SetDebugRegisters(debug_registers);
    }

    mock_thread->SendException(kExceptionAddress, exception_type);

    // This should return immediately.
    loop().RunUntilNoTasks();

    ASSERT_TRUE(client().GetExceptions().empty());
  }
}

TEST_F(DebugAgentServerTestWithClient, AttachToJobOnFilterInstalled) {
  // The non-exhaustive job structure with matching component information will look like this from
  // MockSystemInterface:
  // ..
  // root
  // ├─j: 8 "/moniker"
  // ├─j: 25 "/a/long/generated_to_here/fixed/moniker"
  // └─j: 35 "/some/moniker"
  //    └─j: 38 "/some/other/moniker"
  // ..
  // Since job 38 is a child of 35, DebugAgent shouldn't attach to it, but it will appear as a match
  // to the filter. Therefore, there will be four matches for the filter, and three expected
  // attaches.
  constexpr std::array kExpectedJobKoids = {8, 25, 35};

  // See the default pre-populated system in mock_system_interface.h to see which monikers will be
  // matched.
  fuchsia_debugger::Filter filter;
  filter.pattern("moniker");
  filter.type(fuchsia_debugger::FilterType::kMonikerSuffix);
  filter.options().job_only(true);

  client().AttachTo(filter,
                    [=](fidl::Result<fuchsia_debugger::DebugAgent::AttachTo>& result) mutable {
                      ASSERT_TRUE(result.is_ok());
                      EXPECT_EQ(result->num_matches(), 4u);
                      for (auto koid : kExpectedJobKoids) {
                        EXPECT_NE(agent()->GetDebuggedJob(koid), nullptr);
                      }

                      loop().QuitNow();
                    });

  loop().Run();
}

}  // namespace debug_agent
