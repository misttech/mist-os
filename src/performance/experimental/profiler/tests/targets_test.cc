// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/experimental/profiler/targets.h"

#include <lib/syslog/cpp/macros.h>

#include <condition_variable>
#include <mutex>
#include <thread>

#include <gtest/gtest.h>

TEST(TargetsTest, OwnJob) {
  zx::job parent;
  ASSERT_EQ(ZX_OK, zx::job::default_job()->duplicate(ZX_RIGHT_SAME_RIGHTS, &parent));

  zx::result<profiler::JobTarget> target = profiler::MakeJobTarget(std::move(parent));
  ASSERT_TRUE(target.is_ok());

  // We don't expect to have any sub jobs
  EXPECT_TRUE(target->child_jobs.empty());

  // And the only process should this process
  zx::unowned_process self = zx::process::self();
  zx_info_handle_basic_t info;
  ASSERT_EQ(ZX_OK, self->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));

  EXPECT_EQ(target->processes.size(), size_t{1});
  EXPECT_EQ(target->processes.front().pid, info.koid);

  // There might be multiple threads running, but we should be one of them
  zx::unowned_thread thread_self = zx::thread::self();
  zx_info_handle_basic_t thread_info;
  ASSERT_EQ(ZX_OK, thread_self->get_info(ZX_INFO_HANDLE_BASIC, &thread_info, sizeof(thread_info),
                                         nullptr, nullptr));

  ASSERT_GT(target->processes.front().threads.size(), size_t{0});

  bool found_self = false;
  for (const profiler::ThreadTarget& target : target->processes.front().threads) {
    if (target.tid == thread_info.koid) {
      found_self = true;
    }
  }

  EXPECT_TRUE(found_self);
}
