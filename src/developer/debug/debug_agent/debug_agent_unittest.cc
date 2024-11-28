// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/developer/debug/debug_agent/debug_agent.h"

#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <gtest/gtest.h>

#include "src/developer/debug/debug_agent/arch.h"
#include "src/developer/debug/debug_agent/mock_debug_agent_harness.h"
#include "src/developer/debug/debug_agent/mock_exception_handle.h"
#include "src/developer/debug/debug_agent/mock_process.h"
#include "src/developer/debug/debug_agent/mock_process_handle.h"
#include "src/developer/debug/debug_agent/mock_thread_handle.h"
#include "src/developer/debug/debug_agent/test_utils.h"
#include "src/developer/debug/ipc/message_writer.h"
#include "src/developer/debug/ipc/protocol.h"
#include "src/developer/debug/shared/logging/debug.h"
#include "src/developer/debug/shared/message_loop.h"
#include "src/developer/debug/shared/test_with_loop.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace debug_agent {
namespace {

bool HasAttachedProcessWithKoid(DebugAgent* debug_agent, zx_koid_t koid) {
  DebuggedProcess* proc = debug_agent->GetDebuggedProcess(koid);
  if (!proc)
    return false;

  // All of our process handles should be mock ones.
  return static_cast<MockProcessHandle&>(proc->process_handle()).IsAttached();
}

// Setup -------------------------------------------------------------------------------------------

class DebugAgentMockProcess : public MockProcess {
 public:
  DebugAgentMockProcess(DebugAgent* debug_agent, zx_koid_t koid, std::string name)
      : MockProcess(debug_agent, koid, std::move(name)) {}

  ~DebugAgentMockProcess() = default;

  void SuspendAndSendModules() override {
    // Send the modules over to the ipc.
    debug_agent()->SendNotification(modules_to_send_);
  }

  void set_modules_to_send(debug_ipc::NotifyModules m) { modules_to_send_ = std::move(m); }

 private:
  debug_ipc::NotifyModules modules_to_send_;
};

}  // namespace

// Tests -------------------------------------------------------------------------------------------

class DebugAgentTests : public debug::TestWithLoop {};

TEST_F(DebugAgentTests, OnGlobalStatus) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  debug_ipc::StatusRequest request = {};

  debug_ipc::StatusReply reply = {};
  remote_api->OnStatus(request, &reply);

  ASSERT_EQ(reply.processes.size(), 0u);

  constexpr uint64_t kProcessKoid1 = 0x1234;
  const std::string kProcessName1 = "process-1";
  constexpr uint64_t kProcess1ThreadKoid1 = 0x1;

  auto process1 = std::make_unique<MockProcess>(nullptr, kProcessKoid1, kProcessName1);
  process1->AddThread(kProcess1ThreadKoid1);
  harness.debug_agent()->InjectProcessForTest(std::move(process1));

  reply = {};
  remote_api->OnStatus(request, &reply);

  ASSERT_EQ(reply.processes.size(), 1u);
  EXPECT_EQ(reply.processes[0].process_koid, kProcessKoid1);
  EXPECT_EQ(reply.processes[0].process_name, kProcessName1);
  ASSERT_EQ(reply.processes[0].threads.size(), 1u);
  EXPECT_EQ(reply.processes[0].threads[0].id.process, kProcessKoid1);
  EXPECT_EQ(reply.processes[0].threads[0].id.thread, kProcess1ThreadKoid1);

  constexpr uint64_t kProcessKoid2 = 0x5678;
  const std::string kProcessName2 = "process-2";
  constexpr uint64_t kProcess2ThreadKoid1 = 0x1;
  constexpr uint64_t kProcess2ThreadKoid2 = 0x2;

  auto process2 = std::make_unique<MockProcess>(nullptr, kProcessKoid2, kProcessName2);
  process2->AddThread(kProcess2ThreadKoid1);
  process2->AddThread(kProcess2ThreadKoid2);
  harness.debug_agent()->InjectProcessForTest(std::move(process2));

  reply = {};
  remote_api->OnStatus(request, &reply);

  ASSERT_EQ(reply.processes.size(), 2u);
  EXPECT_EQ(reply.processes[0].process_koid, kProcessKoid1);
  EXPECT_EQ(reply.processes[0].process_name, kProcessName1);
  ASSERT_EQ(reply.processes[0].threads.size(), 1u);
  EXPECT_EQ(reply.processes[0].threads[0].id.process, kProcessKoid1);
  EXPECT_EQ(reply.processes[0].threads[0].id.thread, kProcess1ThreadKoid1);

  EXPECT_EQ(reply.processes[1].process_koid, kProcessKoid2);
  EXPECT_EQ(reply.processes[1].process_name, kProcessName2);
  ASSERT_EQ(reply.processes[1].threads.size(), 2u);
  EXPECT_EQ(reply.processes[1].threads[0].id.process, kProcessKoid2);
  EXPECT_EQ(reply.processes[1].threads[0].id.thread, kProcess2ThreadKoid1);
  EXPECT_EQ(reply.processes[1].threads[1].id.process, kProcessKoid2);
  EXPECT_EQ(reply.processes[1].threads[1].id.thread, kProcess2ThreadKoid2);

  // Set a limbo provider.

  constexpr zx_koid_t kProcKoid1 = 100;
  constexpr zx_koid_t kThreadKoid1 = 101;
  harness.system_interface()->mock_limbo_provider().AppendException(
      MockProcessHandle(kProcKoid1, "proc1"), MockThreadHandle(kThreadKoid1, "thread1"),
      MockExceptionHandle(kThreadKoid1));

  constexpr zx_koid_t kProcKoid2 = 102;
  constexpr zx_koid_t kThreadKoid2 = 103;
  harness.system_interface()->mock_limbo_provider().AppendException(
      MockProcessHandle(kProcKoid2, "proc2"), MockThreadHandle(kThreadKoid1, "thread2"),
      MockExceptionHandle(kThreadKoid2));

  reply = {};
  remote_api->OnStatus(request, &reply);

  // The attached processes should still be there.
  ASSERT_EQ(reply.processes.size(), 2u);

  // The limbo processes should be there.
  ASSERT_EQ(reply.limbo.size(), 2u);
  EXPECT_EQ(reply.limbo[0].process_koid, kProcKoid1);
  EXPECT_EQ(reply.limbo[0].process_name, "proc1");
  ASSERT_EQ(reply.limbo[0].threads.size(), 1u);

  // TODO(donosoc): Add exception type.
}

TEST_F(DebugAgentTests, OnAttachNotFound) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  debug_ipc::AttachRequest attach_request;
  attach_request.koid = -1;

  // Invalid koid should fail.
  debug_ipc::AttachReply attach_reply;
  remote_api->OnAttach(attach_request, &attach_reply);

  EXPECT_TRUE(attach_reply.status.has_error());

  constexpr zx_koid_t kProcKoid1 = 100;
  constexpr zx_koid_t kThreadKoid1 = 101;
  harness.system_interface()->mock_limbo_provider().AppendException(
      MockProcessHandle(kProcKoid1, "proc1"), MockThreadHandle(kThreadKoid1, "thread1"),
      MockExceptionHandle(kThreadKoid1));

  // Even with limbo it should fail.
  remote_api->OnAttach(attach_request, &attach_reply);
  EXPECT_TRUE(attach_reply.status.has_error());
}

TEST_F(DebugAgentTests, OnAttach) {
  constexpr zx_koid_t kProcess1Koid = 11u;  // Koid for job1-p2 from the GetMockJobTree() hierarchy.

  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  debug_ipc::AttachRequest attach_request;
  attach_request.koid = kProcess1Koid;

  debug_ipc::AttachReply attach_reply;
  remote_api->OnAttach(attach_request, &attach_reply);

  // We should've received a watch command (which does the low level exception watching).
  EXPECT_TRUE(HasAttachedProcessWithKoid(harness.debug_agent(), kProcess1Koid));

  // We should've gotten an attach reply.
  EXPECT_TRUE(attach_reply.status.ok());
  EXPECT_EQ(attach_reply.koid, kProcess1Koid);
  EXPECT_EQ(attach_reply.name, "job1-p2");

  // Asking for some invalid process should fail.
  attach_request.koid = 0x231315;  // Some invalid value.
  remote_api->OnAttach(attach_request, &attach_reply);

  // We should've gotten an error reply.
  EXPECT_TRUE(attach_reply.status.has_error());

  // Attaching to a third process should work.
  attach_request.koid = 21u;
  remote_api->OnAttach(attach_request, &attach_reply);

  EXPECT_TRUE(attach_reply.status.ok());
  EXPECT_EQ(attach_reply.koid, 21u);
  EXPECT_EQ(attach_reply.name, "job121-p2");

  // Attaching again to a process should fail.
  remote_api->OnAttach(attach_request, &attach_reply);

  EXPECT_TRUE(attach_reply.status.has_error());
}

TEST_F(DebugAgentTests, AttachToLimbo) {
  // debug::SetDebugMode(true);
  // debug::SetLogCategories({debug::LogCategory::kAll});

  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  constexpr zx_koid_t kProcKoid = 100;
  constexpr zx_koid_t kThreadKoid = 101;
  MockProcessHandle mock_process(kProcKoid, "proc");
  MockThreadHandle mock_thread(kThreadKoid, "thread");
  mock_process.set_threads({mock_thread});

  harness.system_interface()->mock_limbo_provider().AppendException(
      mock_process, mock_thread, MockExceptionHandle(kThreadKoid));

  debug_ipc::AttachRequest attach_request = {};
  attach_request.koid = kProcKoid;

  debug_ipc::AttachReply attach_reply;
  remote_api->OnAttach(attach_request, &attach_reply);
  // The threads are only populated on the next tick.
  debug::MessageLoop::Current()->RunUntilNoTasks();

  // Process should be watching.
  EXPECT_TRUE(HasAttachedProcessWithKoid(harness.debug_agent(), kProcKoid));

  // We should've gotten an attach reply.
  EXPECT_TRUE(attach_reply.status.ok());
  EXPECT_EQ(attach_reply.koid, kProcKoid);
  EXPECT_EQ(attach_reply.name, "proc");

  {
    DebuggedProcess* process = harness.debug_agent()->GetDebuggedProcess(kProcKoid);
    ASSERT_TRUE(process);
    auto threads = process->GetThreads();
    ASSERT_EQ(threads.size(), 1u);

    // Search for the exception thread.
    DebuggedThread* exception_thread = nullptr;
    for (DebuggedThread* thread : threads) {
      if (thread->koid() == kThreadKoid) {
        exception_thread = thread;
        break;
      }
    }

    ASSERT_TRUE(exception_thread);
    ASSERT_TRUE(exception_thread->in_exception());
    EXPECT_EQ(exception_thread->exception_handle()->GetThreadHandle()->GetKoid(), kThreadKoid);
  }
}

TEST_F(DebugAgentTests, OnEnterLimbo) {
  MockDebugAgentHarness harness;

  constexpr zx_koid_t kProcKoid1 = 100;
  constexpr zx_koid_t kThreadKoid1 = 101;
  harness.system_interface()->mock_limbo_provider().AppendException(
      MockProcessHandle(kProcKoid1, "proc1"), MockThreadHandle(kThreadKoid1, "thread1"),
      MockExceptionHandle(kThreadKoid1));

  // Call the limbo.
  harness.system_interface()->mock_limbo_provider().CallOnEnterLimbo();

  // Should've sent a notification.
  {
    ASSERT_EQ(harness.stream_backend()->process_starts().size(), 1u);

    auto& process_start = harness.stream_backend()->process_starts()[0];
    EXPECT_EQ(process_start.type, debug_ipc::NotifyProcessStarting::Type::kLimbo);
    EXPECT_EQ(process_start.koid, kProcKoid1);
    EXPECT_EQ(process_start.name, "proc1");
  }
}

TEST_F(DebugAgentTests, DetachFromLimbo) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  constexpr zx_koid_t kProcKoid = 14;  // MockJobTree job11-p1

  // Attempting to detach to a process that doesn't exist should fail.
  {
    debug_ipc::DetachRequest request = {};
    request.koid = kProcKoid;

    debug_ipc::DetachReply reply = {};
    remote_api->OnDetach(request, &reply);

    ASSERT_TRUE(reply.status.has_error());
    ASSERT_EQ(harness.system_interface()->mock_limbo_provider().release_calls().size(), 0u);
  }

  // Adding it should now find it and remove it.
  constexpr zx_koid_t kProcKoid1 = 100;
  constexpr zx_koid_t kThreadKoid1 = 101;
  harness.system_interface()->mock_limbo_provider().AppendException(
      MockProcessHandle(kProcKoid1, "proc1"), MockThreadHandle(kThreadKoid1, "thread1"),
      MockExceptionHandle(kThreadKoid1));
  {
    debug_ipc::DetachRequest request = {};
    request.koid = kProcKoid1;

    debug_ipc::DetachReply reply = {};
    remote_api->OnDetach(request, &reply);

    ASSERT_TRUE(reply.status.ok());
    ASSERT_EQ(harness.system_interface()->mock_limbo_provider().release_calls().size(), 1u);
    EXPECT_EQ(harness.system_interface()->mock_limbo_provider().release_calls()[0], kProcKoid1);
  }

  // This should've remove it from limbo, trying it again should fail.
  {
    debug_ipc::DetachRequest request = {};
    request.koid = kProcKoid1;

    debug_ipc::DetachReply reply = {};
    remote_api->OnDetach(request, &reply);

    ASSERT_TRUE(reply.status.has_error());
    ASSERT_EQ(harness.system_interface()->mock_limbo_provider().release_calls().size(), 1u);
    EXPECT_EQ(harness.system_interface()->mock_limbo_provider().release_calls()[0], kProcKoid1);
  }
}

TEST_F(DebugAgentTests, Kill) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  constexpr zx_koid_t kProcKoid = 14;  // MockJobTree job11-p1

  // Attempt to kill a process that's not there should fail.
  {
    debug_ipc::KillRequest kill_request = {};
    kill_request.process_koid = kProcKoid;

    debug_ipc::KillReply kill_reply = {};
    remote_api->OnKill(kill_request, &kill_reply);
    ASSERT_TRUE(kill_reply.status.has_error());
  }

  // Attach to a process so that the debugger knows about it.
  {
    debug_ipc::AttachRequest attach_request = {};
    attach_request.koid = kProcKoid;

    debug_ipc::AttachReply attach_reply;
    remote_api->OnAttach(attach_request, &attach_reply);

    // There should be a process.
    ASSERT_EQ(harness.debug_agent()->procs_.size(), 1u);
    // Should not come from limbo.
    EXPECT_FALSE(harness.debug_agent()->procs_.begin()->second->from_limbo());
  }

  // Killing now should work.
  {
    debug_ipc::KillRequest kill_request = {};
    kill_request.process_koid = kProcKoid;

    debug_ipc::KillReply kill_reply = {};
    remote_api->OnKill(kill_request, &kill_reply);

    // There should be no more processes.
    ASSERT_EQ(harness.debug_agent()->procs_.size(), 0u);

    // Killing again should fail.
    remote_api->OnKill(kill_request, &kill_reply);
    ASSERT_TRUE(kill_reply.status.has_error());
  }

  // Add the process to the limbo.
  constexpr zx_koid_t kLimboProcKoid = 100;
  constexpr zx_koid_t kLimboThreadKoid = 101;
  MockProcessHandle mock_process(kLimboProcKoid, "proc");
  // This is a limbo process so we can not kill it.
  mock_process.set_kill_status(debug::Status("Access denied"));
  MockThreadHandle mock_thread(kLimboThreadKoid, "thread");
  MockExceptionHandle mock_exception(kLimboThreadKoid);
  mock_process.set_threads({mock_thread});
  harness.system_interface()->mock_limbo_provider().AppendException(mock_process, mock_thread,
                                                                    mock_exception);

  // There should be no more processes.
  ASSERT_EQ(harness.debug_agent()->procs_.size(), 0u);

  // Killing now should release it.
  {
    debug_ipc::KillRequest kill_request = {};
    kill_request.process_koid = kLimboProcKoid;

    debug_ipc::KillReply kill_reply = {};
    remote_api->OnKill(kill_request, &kill_reply);
    ASSERT_TRUE(kill_reply.status.ok());

    ASSERT_EQ(harness.system_interface()->mock_limbo_provider().release_calls().size(), 1u);
    EXPECT_EQ(harness.system_interface()->mock_limbo_provider().release_calls()[0], kLimboProcKoid);

    // Killing again should not find it.
    remote_api->OnKill(kill_request, &kill_reply);
    ASSERT_TRUE(kill_reply.status.has_error());
  }

  harness.system_interface()->mock_limbo_provider().AppendException(mock_process, mock_thread,
                                                                    mock_exception);

  debug_ipc::AttachRequest attach_request = {};
  attach_request.koid = kLimboProcKoid;

  debug_ipc::AttachReply attach_reply;
  remote_api->OnAttach(attach_request, &attach_reply);

  // There should be a process.
  ASSERT_EQ(harness.debug_agent()->procs_.size(), 1u);

  {
    auto it = harness.debug_agent()->procs_.find(kLimboProcKoid);
    ASSERT_NE(it, harness.debug_agent()->procs_.end());
    EXPECT_TRUE(harness.debug_agent()->procs_.begin()->second->from_limbo());

    // Killing it should free the process.
    debug_ipc::KillRequest kill_request = {};
    kill_request.process_koid = kLimboProcKoid;

    debug_ipc::KillReply kill_reply = {};
    remote_api->OnKill(kill_request, &kill_reply);
    ASSERT_TRUE(kill_reply.status.ok());

    ASSERT_EQ(harness.debug_agent()->procs_.size(), 0u);

    // There should be a limbo process to be killed.
    ASSERT_EQ(harness.debug_agent()->killed_limbo_procs_.size(), 1u);
    EXPECT_EQ(harness.debug_agent()->killed_limbo_procs_.count(kLimboProcKoid), 1u);

    // There should've have been more release calls (yet).
    ASSERT_EQ(harness.system_interface()->mock_limbo_provider().release_calls().size(), 1u);

    // When the process "re-enters" the limbo, it should be removed.
    harness.system_interface()->mock_limbo_provider().AppendException(mock_process, mock_thread,
                                                                      mock_exception);
    harness.system_interface()->mock_limbo_provider().CallOnEnterLimbo();

    // There should not be an additional proc in the agent.
    ASSERT_EQ(harness.debug_agent()->procs_.size(), 0u);

    // There should've been a release call.
    ASSERT_EQ(harness.system_interface()->mock_limbo_provider().release_calls().size(), 2u);
    EXPECT_EQ(harness.system_interface()->mock_limbo_provider().release_calls()[1], kLimboProcKoid);
  }
}

TEST_F(DebugAgentTests, OnUpdateGlobalSettings) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  // The default strategy should be first chance for a type that has yet to be
  // updated.
  EXPECT_EQ(debug_ipc::ExceptionStrategy::kFirstChance,
            harness.debug_agent()->GetExceptionStrategy(debug_ipc::ExceptionType::kGeneral));
  EXPECT_EQ(debug_ipc::ExceptionStrategy::kFirstChance,
            harness.debug_agent()->GetExceptionStrategy(debug_ipc::ExceptionType::kPageFault));

  {
    const debug_ipc::UpdateGlobalSettingsRequest request = {
        .exception_strategies =
            {
                {
                    .type = debug_ipc::ExceptionType::kGeneral,
                    .value = debug_ipc::ExceptionStrategy::kSecondChance,
                },
                {
                    .type = debug_ipc::ExceptionType::kPageFault,
                    .value = debug_ipc::ExceptionStrategy::kSecondChance,
                },
            },
    };
    debug_ipc::UpdateGlobalSettingsReply reply;
    remote_api->OnUpdateGlobalSettings(request, &reply);
    EXPECT_TRUE(reply.status.ok());
  }

  EXPECT_EQ(debug_ipc::ExceptionStrategy::kSecondChance,
            harness.debug_agent()->GetExceptionStrategy(debug_ipc::ExceptionType::kGeneral));
  EXPECT_EQ(debug_ipc::ExceptionStrategy::kSecondChance,
            harness.debug_agent()->GetExceptionStrategy(debug_ipc::ExceptionType::kPageFault));

  {
    const debug_ipc::UpdateGlobalSettingsRequest request = {
        .exception_strategies =
            {
                {
                    .type = debug_ipc::ExceptionType::kGeneral,
                    .value = debug_ipc::ExceptionStrategy::kFirstChance,
                },
            },
    };
    debug_ipc::UpdateGlobalSettingsReply reply;
    remote_api->OnUpdateGlobalSettings(request, &reply);
    EXPECT_TRUE(reply.status.ok());
  }

  EXPECT_EQ(debug_ipc::ExceptionStrategy::kFirstChance,
            harness.debug_agent()->GetExceptionStrategy(debug_ipc::ExceptionType::kGeneral));
  EXPECT_EQ(debug_ipc::ExceptionStrategy::kSecondChance,
            harness.debug_agent()->GetExceptionStrategy(debug_ipc::ExceptionType::kPageFault));
}

TEST_F(DebugAgentTests, WeakFilterMatchDoesNotSendModules) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  constexpr char kProcessName[] = "process-1";
  constexpr uint64_t kProcessKoid = 0x12345;

  debug_ipc::UpdateFilterRequest request;
  auto& filter = request.filters.emplace_back();
  filter.type = debug_ipc::Filter::Type::kProcessName;
  filter.pattern = kProcessName;
  filter.id = 1;
  filter.config.weak = true;

  debug_ipc::UpdateFilterReply reply;
  remote_api->OnUpdateFilter(request, &reply);

  EXPECT_TRUE(reply.matched_processes_for_filter.empty());

  harness.debug_agent()->OnProcessChanged(
      DebugAgent::ProcessChangedHow::kStarting,
      std::make_unique<MockProcessHandle>(kProcessKoid, kProcessName));

  // We should have sent a process starting notification, but no modules.
  EXPECT_FALSE(harness.stream_backend()->process_starts().empty());
  EXPECT_TRUE(harness.stream_backend()->modules().empty());
}

TEST_F(DebugAgentTests, RecursiveFilterAppliesImplicitFilter) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  constexpr char kRootComponentUrl[] = "fuchsia-pkg://devhost/root_package#meta/root_component.cm";
  constexpr char kRootComponentMoniker[] = "some/moniker/root:test";
  constexpr char kSubpackageUrl[] = "#meta/child.cm";
  constexpr char kSubpackageMoniker[] = "some/moniker/root:test/driver";

  debug_ipc::UpdateFilterRequest request;
  auto& filter = request.filters.emplace_back();
  filter.type = debug_ipc::Filter::Type::kComponentUrl;
  filter.pattern = kRootComponentUrl;
  filter.config.recursive = true;

  debug_ipc::UpdateFilterReply reply;
  remote_api->OnUpdateFilter(request, &reply);

  harness.debug_agent()->OnComponentStarted(kRootComponentMoniker, kRootComponentUrl);

  // There should now be TWO filters, one of which the frontend doesn't know about.

  // Now we start the child component, which contains a program.
  harness.debug_agent()->OnComponentStarted(kSubpackageMoniker, kSubpackageUrl);

  EXPECT_EQ(harness.stream_backend()->component_starts().size(), 2u);
  EXPECT_EQ(harness.stream_backend()->component_starts()[1].component.url, kSubpackageUrl);
  EXPECT_EQ(harness.stream_backend()->component_starts()[1].component.moniker, kSubpackageMoniker);
}

TEST_F(DebugAgentTests, AttachToExistingJob) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  constexpr zx_koid_t kJobKoid = 8;
  constexpr zx_koid_t kProcessKoid = 9;

  debug_ipc::AttachRequest request;
  request.koid = kJobKoid;
  request.config.target = debug_ipc::AttachConfig::Target::kJob;
  request.config.weak = true;

  debug_ipc::AttachReply reply;
  remote_api->OnAttach(request, &reply);

  // Because we didn't inject all of the notifications.
  auto debugged_job = harness.debug_agent()->GetDebuggedJob(kJobKoid);
  ASSERT_TRUE(debugged_job);

  // Simply attaching to a job won't necessarily create the DebuggedProcess objects (i.e. the client
  // issued a direct attach command to an already running component instead of installing a filter
  // and waiting for a matching component to start).
  EXPECT_FALSE(harness.debug_agent()->GetDebuggedProcess(kProcessKoid));
}

TEST_F(DebugAgentTests, JobOnlyFilter) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  // Matches the default job tree in MockSystemInterface.
  constexpr zx_koid_t kJobKoid = 25;
  constexpr zx_koid_t kProcessKoid = 26;
  constexpr char kComponentRootMoniker[] = "fixed/moniker";

  debug_ipc::UpdateFilterRequest request;
  auto& filter = request.filters.emplace_back();
  filter.type = debug_ipc::Filter::Type::kComponentMonikerSuffix;
  filter.pattern = kComponentRootMoniker;
  filter.config.job_only = true;
  // By specifying a strong attach, we'll claim the job's exception channel, not the debugger
  // exception channel.
  filter.config.weak = false;

  debug_ipc::UpdateFilterReply reply;
  // We don't need to worry about the contents of the reply, it will have all of the child processes
  // under the component. In a real system, the client will choose what to attach to.
  remote_api->OnUpdateFilter(request, &reply);

  // Send the notification that the process under this job started.
  harness.debug_agent()->OnProcessChanged(
      DebugAgent::ProcessChangedHow::kStarting,
      harness.debug_agent()->system_interface().GetProcess(kProcessKoid));

  // We should now have both a DebuggedJob and a DebuggedProcess for this job and process. The
  // process should *not* have the exception channel bound, because the filter was configured as
  // job_only.
  EXPECT_EQ(harness.stream_backend()->process_starts().size(), 1u);
  EXPECT_TRUE(harness.debug_agent()->GetDebuggedJob(kJobKoid));
  EXPECT_TRUE(harness.debug_agent()->GetDebuggedProcess(kProcessKoid));
  EXPECT_FALSE(harness.debug_agent()->GetDebuggedProcess(kProcessKoid)->IsAttached());
}

TEST_F(DebugAgentTests, JobOnlyFilterDoesNotAttachToChildJobs) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  // Matches the default job tree in MockSystemInterface. This is "job1".
  constexpr zx_koid_t kJobKoid = 8;
  // Direct child process of "job1".
  constexpr zx_koid_t kProcessKoid = 9;
  // This is a child job of the above, "job11".
  constexpr zx_koid_t kSecondJobKoid = 13;
  // Child of job11.
  constexpr zx_koid_t kSecondProcessKoid = 14;

  // Component moniker associated with all above jobs.
  constexpr char kComponentRootMoniker[] = "/moniker";

  debug_ipc::UpdateFilterRequest request;
  auto& filter = request.filters.emplace_back();
  filter.type = debug_ipc::Filter::Type::kComponentMonikerSuffix;
  filter.pattern = kComponentRootMoniker;
  filter.config.job_only = true;
  // By specifying a strong attach, we'll claim the job's exception channel, not the debugger
  // exception channel.
  filter.config.weak = false;

  debug_ipc::UpdateFilterReply reply;
  // We don't need to worry about the contents of the reply, it will have all of the child processes
  // under the component. In a real system, the client will choose what to attach to.
  remote_api->OnUpdateFilter(request, &reply);

  // Send the notification that the child process of "job1" started. This one will always start
  // first. This should cause us to attach to the root job of the component matching this process,
  // i.e. "job1".
  harness.debug_agent()->OnProcessChanged(
      DebugAgent::ProcessChangedHow::kStarting,
      harness.debug_agent()->system_interface().GetProcess(kProcessKoid));

  // Send the notification that the child process of "job11" started. This should _not_ attach to
  harness.debug_agent()->OnProcessChanged(
      DebugAgent::ProcessChangedHow::kStarting,
      harness.debug_agent()->system_interface().GetProcess(kSecondProcessKoid));

  // We should now have both processes specified above. Neither process should have the exception
  // channel bound, because the filter was configured as job_only. Furthermore, the parent job
  // _should_ be attached (i.e. we'll have a DebuggedJob object for it) and the child job should
  // _not_ be attached.
  EXPECT_EQ(harness.stream_backend()->process_starts().size(), 2u);
  EXPECT_TRUE(harness.debug_agent()->GetDebuggedJob(kJobKoid));
  // We are _not_ attached to this second job, even though it would also match the component filter.
  EXPECT_FALSE(harness.debug_agent()->GetDebuggedJob(kSecondJobKoid));
}

// This test is very similar to the above test. The difference is in the component structure. This
// test's filter matches multiple components, which respectively in a real system would have unique
// job_ids, and therefore return multiple results in the filter matching logic. This is different
// from the above situation where one component (and its corresponding job) contains multiple jobs
// but no child components.
TEST_F(DebugAgentTests, DoNotAttachToChildJobs) {
  MockDebugAgentHarness harness;
  RemoteAPI* remote_api = harness.debug_agent();

  // From the default MockSystemInterface.
  constexpr zx_koid_t kParentJobKoid = 35;
  constexpr zx_koid_t kChildJobKoid = 38;

  // This realm has two ELF components, one is the realm's root, the other is a child. They each
  // have distinctive job_ids.
  constexpr char kMonikerPrefix[] = "/some";

  debug_ipc::UpdateFilterRequest request;
  auto& filter = request.filters.emplace_back();
  filter.type = debug_ipc::Filter::Type::kComponentMonikerPrefix;
  filter.pattern = kMonikerPrefix;
  filter.config.job_only = true;
  // By specifying a strong attach, we'll claim the job's exception channel, not the debugger
  // exception channel.
  filter.config.weak = false;

  debug_ipc::UpdateFilterReply reply;
  remote_api->OnUpdateFilter(request, &reply);

  // Two filters should have matched.
  ASSERT_EQ(reply.matched_processes_for_filter.size(), 1u);
  auto found_job_filter = reply.matched_processes_for_filter[0];
  auto matched_job_koids = found_job_filter.matched_pids;

  // Two components with unique job_ids match the filter. Both match the moniker prefix filter, but
  // we should only attach to the parent.
  EXPECT_EQ(matched_job_koids.size(), 2u);
  auto parent_job = std::find(matched_job_koids.begin(), matched_job_koids.end(), kParentJobKoid);
  ASSERT_NE(parent_job, matched_job_koids.end());
  auto child_job = std::find(matched_job_koids.begin(), matched_job_koids.end(), kChildJobKoid);
  ASSERT_NE(child_job, matched_job_koids.end());

  // Process starting events will trigger automatic attaching.
  auto job5_p1 = std::make_unique<MockProcessHandle>(36);
  job5_p1->set_job_koid(kParentJobKoid);
  auto job51_p1 = std::make_unique<MockProcessHandle>(39);
  job51_p1->set_job_koid(kChildJobKoid);
  harness.debug_agent()->OnProcessChanged(DebugAgent::ProcessChangedHow::kStarting,
                                          std::move(job5_p1));
  harness.debug_agent()->OnProcessChanged(DebugAgent::ProcessChangedHow::kStarting,
                                          std::move(job51_p1));

  EXPECT_TRUE(harness.debug_agent()->GetDebuggedJob(*parent_job));
  EXPECT_FALSE(harness.debug_agent()->GetDebuggedJob(*child_job));
}

}  // namespace debug_agent
