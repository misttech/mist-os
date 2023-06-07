// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/experimental/profiler/process_watcher.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/macros.h>

#include <condition_variable>
#include <mutex>
#include <thread>

#include <gtest/gtest.h>

// Monitor a process and ensure it gets notified when it starts a thread.
//
// Note that the starting thread is paused, but not the starter thread. That ensures we don't
// accidentally deadlock ourself when handling the debug exception.
TEST(ProcessWatcherTests, SelfThreads) {
  std::atomic<bool> saw_child = false;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  zx::unowned_process self = zx::process::self();

  ProcessWatcher watcher{std::move(self),
                         [&saw_child](zx_koid_t, zx_koid_t, zx::thread t) { saw_child = true; }};
  std::thread thread_spawner{[&saw_child]() {
    for (;;) {
      if (saw_child) {
        return;
      }
      std::thread t([]() { return 0; });
      t.join();
    }
  }};

  thread_spawner.detach();
  zx::result<> watch_res = watcher.Watch(loop.dispatcher());
  ASSERT_TRUE(watch_res.is_ok());

  while (!saw_child) {
    loop.RunUntilIdle();
  }
  EXPECT_TRUE(saw_child);
}
