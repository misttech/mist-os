// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/event.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls/object.h>

#include <zxtest/zxtest.h>

#include "helper.h"

namespace object_info_test {
namespace {

TEST(TaskGetInfoTest, InfoStatsUnstartedSucceeds) {
  static constexpr char kProcessName[] = "object-info-unstarted";

  zx::vmar vmar;
  zx::process process;

  ASSERT_OK(zx::process::create(*zx::job::default_job(), kProcessName, sizeof(kProcessName), 0u,
                                &process, &vmar));

  zx_info_task_stats_t info;
  ASSERT_OK(process.get_info(ZX_INFO_TASK_STATS, &info, sizeof(info), nullptr, nullptr));
}

// TODO(https://fxbug.dev/338300808): While in the transition phase for RFC-0254 we are in 'legacy
// attribution', and this is indicated by the fractional value being UINT64_MAX. This test can be
// removed once the legacy attribution is removed and the rest of the tests are updated for the
// new attribution model.
TEST(TaskGetInfoTest, FractionalScaledSharedBytesSentinel) {
  zx_info_task_stats_t info;
  ASSERT_OK(
      zx::process::self()->get_info(ZX_INFO_TASK_STATS, &info, sizeof(info), nullptr, nullptr));
  EXPECT_EQ(info.mem_fractional_scaled_shared_bytes, UINT64_MAX);
}

template <typename InfoT>
static void TestProcessGetInfoStatsSmokeTest(const uint32_t topic) {
  InfoT info;
  ASSERT_OK(zx::process::self()->get_info(topic, &info, sizeof(info), nullptr, nullptr));

  EXPECT_GT(info.mem_private_bytes, 0u);
  EXPECT_GE(info.mem_shared_bytes, 0u);
  EXPECT_GE(info.mem_mapped_bytes, info.mem_private_bytes + info.mem_shared_bytes);
  EXPECT_GE(info.mem_scaled_shared_bytes, 0u);
  EXPECT_GE(info.mem_shared_bytes, info.mem_scaled_shared_bytes);
}

TEST(TaskGetInfoTest, InfoStatsSmokeTest) {
  TestProcessGetInfoStatsSmokeTest<zx_info_task_stats_t>(ZX_INFO_TASK_STATS);
}

TEST(TaskGetInfoTest, InfoStatsSmokeTestV1) {
  TestProcessGetInfoStatsSmokeTest<zx_info_task_stats_v1_t>(ZX_INFO_TASK_STATS_V1);
}

constexpr auto handle_provider = []() -> const zx::process& {
  const static zx::unowned_process process = zx::process::self();
  return *process;
};

TEST(TaskGetInfoTest, InfoTaskStatsInvalidHandleFails) {
  ASSERT_NO_FATAL_FAILURE(
      (CheckInvalidHandleFails<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, 1, handle_provider)));
}

TEST(TaskGetInfoTest, InfoTaskStatsNullAvailSucceeds) {
  ASSERT_TRUE(handle_provider().is_valid());
  ASSERT_NO_FATAL_FAILURE(
      (CheckNullAvailSucceeds<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, 1, handle_provider)));
}

TEST(TaskGetInfoTest, InfoTaskStatsNullActualSucceeds) {
  ASSERT_NO_FATAL_FAILURE(
      (CheckNullActualSucceeds<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, 1, handle_provider)));
}

TEST(TaskGetInfoTest, InfoTaskStatsNullActualAndAvailSucceeds) {
  ASSERT_NO_FATAL_FAILURE((CheckNullActualAndAvailSucceeds<zx_info_task_stats_t>(
      ZX_INFO_TASK_STATS, 1, handle_provider)));
}

TEST(TaskGetInfoTest, InfoTaskStatsInvalidBufferPointerFails) {
  ASSERT_NO_FATAL_FAILURE(
      (CheckInvalidBufferPointerFails<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, handle_provider)));
}

TEST(TaskGetInfoTest, InfoTaskStatsBadActualIsInvalidArg) {
  ASSERT_NO_FATAL_FAILURE(
      (BadActualIsInvalidArgs<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, 1, handle_provider)));
}

TEST(TaskGetInfoTest, InfoTaskStatsBadAvailIsInvalidArg) {
  ASSERT_NO_FATAL_FAILURE(
      (BadAvailIsInvalidArgs<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, 1, handle_provider)));
}

TEST(TaskGetInfoTest, InfoTaskStatsZeroSizedBufferIsTooSmall) {
  ASSERT_NO_FATAL_FAILURE(
      (CheckZeroSizeBufferFails<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, handle_provider)));
}

constexpr auto job_provider = []() -> const zx::job& {
  const static zx::unowned_job job = zx::job::default_job();
  return *job;
};

TEST(TaskGetInfoTest, InfoTaskStatsJobHandleIsBadHandle) {
  ASSERT_NO_FATAL_FAILURE(
      CheckWrongHandleTypeFails<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, 1, job_provider));
}

constexpr auto thread_provider = []() -> const zx::thread& {
  const static zx::unowned_thread thread = zx::thread::self();
  return *thread;
};

TEST(TaskGetInfoTest, InfoTaskStatsThreadHandleIsBadHandle) {
  ASSERT_NO_FATAL_FAILURE(
      CheckWrongHandleTypeFails<zx_info_task_stats_t>(ZX_INFO_TASK_STATS, 1, thread_provider));
}

TEST(TaskGetInfoTest, InfoTaskRuntimeWrongType) {
  zx::event event;
  zx::event::create(0, &event);

  auto event_provider = [&]() -> const zx::event& { return event; };

  ASSERT_NO_FATAL_FAILURE(
      CheckWrongHandleTypeFails<zx_info_task_runtime_t>(ZX_INFO_TASK_RUNTIME, 1, event_provider));
}

TEST(TaskGetInfoTest, InfoTaskRuntimeInvalidHandle) {
  ASSERT_NO_FATAL_FAILURE(
      CheckInvalidHandleFails<zx_info_task_runtime_t>(ZX_INFO_TASK_RUNTIME, 1, thread_provider));
}

}  // namespace
}  // namespace object_info_test
