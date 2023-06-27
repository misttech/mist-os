// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/scheduler/role.h>
#include <lib/zx/event.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include <condition_variable>
#include <mutex>
#include <thread>

#include <zxtest/zxtest.h>

namespace {

TEST(RoleApi, SetRole) {
  // Test setting a role on the current thread.
  {
    EXPECT_OK(fuchsia_scheduler::SetRoleForThisThread("fuchsia.test-role:ok"));
    EXPECT_EQ(ZX_ERR_NOT_FOUND,
              fuchsia_scheduler::SetRoleForThisThread("fuchsia.test-role:not-found"));
  }

  // Test setting a role on another thread.
  {
    std::mutex lock;
    bool done = false;
    std::condition_variable condition;

    std::thread thread{[&] {
      std::unique_lock<std::mutex> guard{lock};
      condition.wait(guard, [&] { return done; });
    }};

    const zx::unowned_thread thread_handle{native_thread_get_zx_handle(thread.native_handle())};

    EXPECT_OK(fuchsia_scheduler::SetRoleForThread(thread_handle->borrow(), "fuchsia.test-role:ok"));
    EXPECT_EQ(ZX_ERR_NOT_FOUND, fuchsia_scheduler::SetRoleForThread(thread_handle->borrow(),
                                                                    "fuchsia.test-role:not-found"));

    {
      std::unique_lock<std::mutex> guard{lock};
      done = true;
    }

    condition.notify_all();
    thread.join();
  }

  // Test setting a role on an unsupported object type.
  {
    zx::event event;
    ASSERT_OK(zx::event::create(0, &event));

    EXPECT_EQ(ZX_ERR_WRONG_TYPE, fuchsia_scheduler::SetRoleForHandle(
                                     zx::unowned_handle{event.get()}, "fuchsia.test-role:ok"));
  }
}

}  // anonymous namespace
