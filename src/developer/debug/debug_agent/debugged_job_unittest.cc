// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/debugged_job.h"

#include <gtest/gtest.h>

#include "src/developer/debug/debug_agent/mock_debug_agent_harness.h"
#include "src/developer/debug/debug_agent/mock_process.h"
#include "src/developer/debug/debug_agent/mock_thread.h"

namespace debug_agent {
TEST(DebuggedJob, ReportUnhandledException) {
  debug_agent::MockDebugAgentHarness harness;
  DebuggedJob job(harness.debug_agent());

  constexpr zx_koid_t kJobKoid = 1234;
  auto mock_job_handle = std::make_unique<MockJobHandle>(kJobKoid);

  constexpr zx_koid_t kProcess1Koid = 1235;
  constexpr zx_koid_t kProcess1Thread1Koid = 1236;
  constexpr zx_koid_t kProcess2Koid = 1237;
  constexpr zx_koid_t kProcess2Thread1Koid = 1238;
  constexpr zx_koid_t kProcess2Thread2Koid = 1239;

  MockProcess* p1 = harness.AddProcess(kProcess1Koid);
  auto p1_handle = p1->mock_process_handle();

  auto p1_t1 = p1->AddThread(kProcess1Thread1Koid);
  p1_handle.set_threads({p1_t1->mock_thread_handle()});

  MockProcess* p2 = harness.AddProcess(kProcess2Koid);
  auto p2_t1 = p2->AddThread(kProcess2Thread1Koid);
  auto p2_t2 = p2->AddThread(kProcess2Thread2Koid);
  auto p2_handle = p2->mock_process_handle();
  p2_handle.set_threads({p2_t1->mock_thread_handle(), p2_t2->mock_thread_handle()});

  mock_job_handle->set_child_processes({p1_handle, p2_handle});

  MockJobHandle* job_handle = mock_job_handle.get();

  DebuggedJobCreateInfo create_info(std::move(mock_job_handle));
  // This tells the MockJobHandle that we are allowed to receive exception events, mimicking a real
  // system.
  create_info.type = JobExceptionChannelType::kException;
  job.Init(std::move(create_info));

  auto mock_exception = std::make_unique<MockExceptionHandle>(kProcess1Koid, kProcess1Thread1Koid);

  job_handle->OnException(std::move(mock_exception), MockJobExceptionInfo::kException);

  ASSERT_FALSE(harness.stream_backend()->exceptions().empty());
  ASSERT_EQ(harness.stream_backend()->exceptions().size(), 1u);

  // This exception should be marked that it came from the job and there's nothing to do other than
  // display the exception and move on.
  EXPECT_TRUE(harness.stream_backend()->exceptions()[0].job_only);

  // Other fields in the exception notification should always be empty.
  EXPECT_TRUE(harness.stream_backend()->exceptions()[0].hit_breakpoints.empty());
  EXPECT_TRUE(harness.stream_backend()->exceptions()[0].other_affected_threads.empty());

  // The exception should be released after the notification is sent.
  EXPECT_EQ(mock_exception, nullptr);
}
}  // namespace debug_agent
