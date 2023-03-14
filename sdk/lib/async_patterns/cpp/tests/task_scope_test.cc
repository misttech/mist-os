// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/time.h>
#include <lib/async/task.h>
#include <lib/async_patterns/cpp/task_scope.h>
#include <lib/fit/defer.h>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

TEST(TaskScope, Construction) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
  async_patterns::TaskScope tasks{loop.dispatcher()};
}

TEST(TaskScope, Post) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
  async_patterns::TaskScope tasks{loop.dispatcher()};

  bool ok = false;
  tasks.Post([&] {
    EXPECT_FALSE(ok);
    ok = true;
  });

  EXPECT_FALSE(ok);
  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_TRUE(ok);
}

TEST(TaskScope, PostCanceledOnDestruction) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};

  bool ok = false;
  {
    async_patterns::TaskScope tasks{loop.dispatcher()};
    tasks.Post([&] { ok = true; });
  }

  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostCanceledOnDestructionTaskDestructorPostTask) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};

  bool ok = false;
  bool destroyed = false;
  {
    async_patterns::TaskScope tasks{loop.dispatcher()};
    auto deferred = fit::defer([&] {
      destroyed = true;
      tasks.Post([&] { ok = true; });
    });
    tasks.Post([&, deferred = std::move(deferred)]() mutable {
      ok = true;
      deferred.cancel();
    });
    EXPECT_FALSE(ok);
    EXPECT_FALSE(destroyed);
  }

  EXPECT_TRUE(destroyed);
  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostCanceledOnShutdown) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
  async_patterns::TaskScope tasks{loop.dispatcher()};

  bool ok = false;
  tasks.Post([&] { ok = true; });

  loop.Shutdown();
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostCanceledOnShutdownTaskDestructorPostTask) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};

  bool ok = false;
  bool destroyed = false;
  async_patterns::TaskScope tasks{loop.dispatcher()};
  auto deferred = fit::defer([&] {
    destroyed = true;
    tasks.Post([&] { ok = true; });
  });
  tasks.Post([&, deferred = std::move(deferred)]() mutable {
    ok = true;
    deferred.cancel();
  });
  EXPECT_FALSE(ok);
  EXPECT_FALSE(destroyed);

  loop.Shutdown();
  EXPECT_TRUE(destroyed);
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostDelayed) {
  async::TestLoop loop;
  async_patterns::TaskScope tasks{loop.dispatcher()};

  bool ok = false;
  tasks.PostDelayed(
      [&] {
        EXPECT_FALSE(ok);
        ok = true;
      },
      zx::sec(2));

  loop.RunFor(zx::sec(1));
  EXPECT_FALSE(ok);

  loop.RunFor(zx::sec(1));
  EXPECT_TRUE(ok);
}

TEST(TaskScope, PostDelayedCanceledOnDestruction) {
  async::TestLoop loop;

  bool ok = false;
  {
    async_patterns::TaskScope tasks{loop.dispatcher()};
    tasks.PostDelayed([&] { ok = true; }, zx::sec(1));
  }

  loop.RunFor(zx::sec(3));
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostDelayedCanceledOnDestructionTaskDestructorPostTask) {
  async::TestLoop loop;

  bool ok = false;
  bool destroyed = false;
  {
    async_patterns::TaskScope tasks{loop.dispatcher()};
    auto deferred = fit::defer([&] {
      destroyed = true;
      tasks.PostDelayed([&] { ok = true; }, zx::sec(1));
    });
    tasks.PostDelayed(
        [&, deferred = std::move(deferred)]() mutable {
          ok = true;
          deferred.cancel();
        },
        zx::sec(1));
    EXPECT_FALSE(ok);
    EXPECT_FALSE(destroyed);
  }

  EXPECT_TRUE(destroyed);
  loop.RunFor(zx::sec(10));
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostDelayedCanceledOnShutdown) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
  async_patterns::TaskScope tasks{loop.dispatcher()};

  bool ok = false;
  tasks.PostDelayed([&]() mutable { ok = true; }, zx::sec(1));
  EXPECT_FALSE(ok);

  loop.Shutdown();
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostDelayedCanceledOnShutdownTaskDestructorPostTask) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};

  bool ok = false;
  bool destroyed = false;
  async_patterns::TaskScope tasks{loop.dispatcher()};
  auto deferred = fit::defer([&] {
    destroyed = true;
    tasks.PostDelayed([&] { ok = true; }, zx::sec(1));
  });
  tasks.PostDelayed(
      [&, deferred = std::move(deferred)]() mutable {
        ok = true;
        deferred.cancel();
      },
      zx::sec(1));
  EXPECT_FALSE(ok);
  EXPECT_FALSE(destroyed);

  loop.Shutdown();
  EXPECT_TRUE(destroyed);
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostForTime) {
  async::TestLoop loop;
  async_patterns::TaskScope tasks{loop.dispatcher()};

  bool ok = false;
  tasks.PostForTime(
      [&] {
        EXPECT_FALSE(ok);
        ok = true;
      },
      async::Now(loop.dispatcher()) + zx::sec(2));

  loop.RunFor(zx::sec(1));
  EXPECT_FALSE(ok);

  loop.RunFor(zx::sec(1));
  EXPECT_TRUE(ok);
}

TEST(TaskScope, PostForTimeCanceledOnDestruction) {
  async::TestLoop loop;

  bool ok = false;
  {
    async_patterns::TaskScope tasks{loop.dispatcher()};
    tasks.PostForTime([&] { ok = true; }, async::Now(loop.dispatcher()) + zx::sec(1));
  }

  loop.RunFor(zx::sec(3));
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostForTimeCanceledOnDestructionTaskDestructorPostTask) {
  async::TestLoop loop;

  bool ok = false;
  bool destroyed = false;
  {
    async_patterns::TaskScope tasks{loop.dispatcher()};
    auto deferred = fit::defer([&] {
      destroyed = true;
      tasks.PostForTime([&] { ok = true; }, async::Now(loop.dispatcher()) + zx::sec(1));
    });
    tasks.PostForTime(
        [&, deferred = std::move(deferred)]() mutable {
          ok = true;
          deferred.cancel();
        },
        async::Now(loop.dispatcher()) + zx::sec(1));
    EXPECT_FALSE(ok);
    EXPECT_FALSE(destroyed);
  }

  EXPECT_TRUE(destroyed);
  loop.RunFor(zx::sec(10));
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostForTimeCanceledOnShutdown) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
  async_patterns::TaskScope tasks{loop.dispatcher()};

  bool ok = false;
  tasks.PostForTime([&]() mutable { ok = true; }, async::Now(loop.dispatcher()) + zx::sec(1));
  EXPECT_FALSE(ok);

  loop.Shutdown();
  EXPECT_FALSE(ok);
}

TEST(TaskScope, PostForTimeCanceledOnShutdownTaskDestructorPostTask) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};

  bool ok = false;
  bool destroyed = false;
  async_patterns::TaskScope tasks{loop.dispatcher()};
  auto deferred = fit::defer([&] {
    destroyed = true;
    tasks.PostForTime([&] { ok = true; }, async::Now(loop.dispatcher()) + zx::sec(1));
  });
  tasks.PostForTime(
      [&, deferred = std::move(deferred)]() mutable {
        ok = true;
        deferred.cancel();
      },
      async::Now(loop.dispatcher()) + zx::sec(1));
  EXPECT_FALSE(ok);
  EXPECT_FALSE(destroyed);

  loop.Shutdown();
  EXPECT_TRUE(destroyed);
  EXPECT_FALSE(ok);
}

TEST(TaskScope, Mixture) {
  async::TestLoop loop;
  async_patterns::TaskScope tasks{loop.dispatcher()};

  int count = 0;
  tasks.Post([&] { count++; });
  tasks.PostDelayed([&] { count++; }, zx::sec(1));
  tasks.PostForTime([&] { count++; }, async::Now(loop.dispatcher()) + zx::sec(1));
  tasks.Post([&] { count++; });

  EXPECT_EQ(count, 0);
  loop.RunFor(zx::sec(1));
  EXPECT_EQ(count, 4);
}

TEST(TaskScope, CheckSynchronization) {
  async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
  async_patterns::TaskScope tasks{loop.dispatcher()};

  std::thread([&] {
    ASSERT_DEATH(tasks.Post([] {}), "\\|async_patterns::TaskQueue\\| is thread-unsafe");
  }).join();
  std::thread([&] {
    ASSERT_DEATH(tasks.PostDelayed([] {}, zx::sec(1)),
                 "\\|async_patterns::TaskQueue\\| is thread-unsafe");
  }).join();
  std::thread([&] {
    ASSERT_DEATH(tasks.PostForTime([] {}, zx::time::infinite()),
                 "\\|async_patterns::TaskQueue\\| is thread-unsafe");
  }).join();
}
