// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/standalone-test/standalone.h>
#include <lib/zx/clock.h>
#include <lib/zx/event.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/resource.h>
#include <zircon/syscalls/system.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <condition_variable>
#include <iostream>
#include <mutex>
#include <random>
#include <thread>
#include <vector>

#include <zxtest/zxtest.h>

#include "../needs-next.h"

NEEDS_NEXT_SYSCALL(zx_system_suspend_enter);

namespace {

zx::result<zx::resource> GetSystemCpuResource() {
  zx::resource system_cpu_resource;
  const zx_status_t status =
      zx::resource::create(*standalone::GetSystemResource(), ZX_RSRC_KIND_SYSTEM,
                           ZX_RSRC_SYSTEM_CPU_BASE, 1, nullptr, 0, &system_cpu_resource);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(system_cpu_resource));
}

class Random {
 public:
  Random() = default;

  zx::duration GetUniform(zx::duration min, zx::duration max) {
    std::uniform_int_distribution<zx_duration_t> distribution{min.get(), max.get()};
    return zx::duration{distribution(generator_)};
  }

 private:
  std::mt19937_64 generator_{std::random_device{}()};
};

TEST(SystemSuspend, ResourceValidation) {
  NEEDS_NEXT_SKIP(zx_system_suspend_enter);

  const zx::result resource_result = GetSystemCpuResource();
  ASSERT_OK(resource_result.status_value());

  zx::event event;
  ASSERT_OK(zx::event::create(0, &event));

  // The suspend call validates the resource before proceeding, errors associated with other
  // conditions means validation succeeded.
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            zx_system_suspend_enter(resource_result->get(), ZX_TIME_INFINITE_PAST));
  EXPECT_EQ(ZX_ERR_WRONG_TYPE, zx_system_suspend_enter(event.get(), ZX_TIME_INFINITE_PAST));

  // TODO(eieio): This syscall uses standard resource validation. Do we need to cover all cases in
  // this test?
}

TEST(SystemSuspend, TimeoutIsPast) {
  NEEDS_NEXT_SKIP(zx_system_suspend_enter);

  const zx::result resource_result = GetSystemCpuResource();
  ASSERT_OK(resource_result.status_value());

  const zx::time_boot almost_now = zx::clock::get_boot() - zx::nsec(1);
  EXPECT_EQ(ZX_ERR_TIMED_OUT, zx_system_suspend_enter(resource_result->get(), almost_now.get()));
  EXPECT_EQ(ZX_ERR_TIMED_OUT,
            zx_system_suspend_enter(resource_result->get(), ZX_TIME_INFINITE_PAST));
  EXPECT_EQ(ZX_ERR_TIMED_OUT, zx_system_suspend_enter(resource_result->get(), 0));
}

TEST(SystemSuspend, SuspendAndResumeByTimer) {
  NEEDS_NEXT_SKIP(zx_system_suspend_enter);

  const zx::result resource_result = GetSystemCpuResource();
  ASSERT_OK(resource_result.status_value());

  // Avoid flakes due to VM pauses by using increasing resume durations until timeouts do not occur.
  zx::time_boot resume_at;
  zx_status_t suspend_status;
  zx::duration suspend_duration = zx::sec(1);
  do {
    resume_at = zx::clock::get_boot() + suspend_duration;
    suspend_status = zx_system_suspend_enter(resource_result->get(), resume_at.get());
    suspend_duration *= 2;
  } while (suspend_status == ZX_ERR_TIMED_OUT);

  EXPECT_OK(suspend_status);
  EXPECT_GT(zx::clock::get_boot(), resume_at);
}

TEST(SystemSuspend, ConcurrentSuspend) {
  NEEDS_NEXT_SKIP(zx_system_suspend_enter);

  const zx::result resource_result = GetSystemCpuResource();
  ASSERT_OK(resource_result.status_value());

  const size_t num_threads = 4;
  const size_t suspend_tries_per_thread = 10;

  std::mutex lock;
  std::condition_variable initialized_condition;
  std::condition_variable start_condition;
  size_t initialized_count = 0;
  bool start_flag = 0;

  const auto suspend_tester = [&](const size_t id) {
    std::cout << "Suspend tester " << id << " starting up...\n";
    {
      std::unique_lock<std::mutex> guard{lock};
      initialized_count++;
      initialized_condition.notify_one();
      start_condition.wait(guard, [&] { return start_flag; });
    }

    Random random;

    for (size_t i = 0; i < suspend_tries_per_thread; i++) {
      // Vary the suspend duration so that some attempts are likely to succeed and some are likely
      // to abort due to reaching the resume time before suspend completes.
      const zx::duration suspend_duration = random.GetUniform(zx::msec(10), zx::sec(1));

      std::cout << "Suspend tester " << id << " attempt " << i << "...\n";
      const zx::time_boot resume_at = zx::clock::get_boot() + suspend_duration;
      const zx_status_t suspend_status =
          zx_system_suspend_enter(resource_result->get(), resume_at.get());
      EXPECT_TRUE(suspend_status == ZX_OK || suspend_status == ZX_ERR_TIMED_OUT);
      std::cout << "Suspend tester " << id << " attempt " << i
                << ": status=" << zx_status_get_string(suspend_status) << "\n";
    }
  };

  std::vector<std::thread> threads;
  threads.reserve(num_threads);
  for (size_t i = 0; i < num_threads; i++) {
    threads.emplace_back(suspend_tester, i);
  }

  {
    std::unique_lock<std::mutex> guard{lock};
    std::cout << "Waiting for suspend test threads to start...\n";
    initialized_condition.wait(guard, [&] { return initialized_count == num_threads; });
    start_flag = true;
  }
  start_condition.notify_all();

  for (std::thread& thread : threads) {
    thread.join();
  }
}

}  // anonymous namespace
