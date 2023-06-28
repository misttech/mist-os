// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifdef __Fuchsia__
#include <lib/zx/event.h>
#include <lib/zx/vmo.h>
#endif

#include <chrono>
#include <thread>

#include <gtest/gtest.h>

#include "magma_util/dlog.h"
#include "magma_util/short_macros.h"
#include "platform_semaphore.h"

namespace {

class TestPlatformSemaphore : public testing::Test {
 public:
  void TestMultiThread() {
    std::shared_ptr<magma::PlatformSemaphore> sem = magma::PlatformSemaphore::Create();
    std::unique_ptr<std::thread> thread;

    // Verify timeout
    thread.reset(new std::thread([sem] {
      DLOG("Waiting for semaphore");
      EXPECT_EQ(sem->Wait(100), MAGMA_STATUS_TIMED_OUT);
      DLOG("Semaphore wait returned");
    }));
    thread->join();

    // Verify return before timeout
    thread.reset(new std::thread([sem] {
      DLOG("Waiting for semaphore");
      EXPECT_TRUE(sem->Wait(100));
      DLOG("Semaphore wait returned");
    }));
    sem->Signal();
    thread->join();

    // Verify autoreset - should timeout again
    thread.reset(new std::thread([sem] {
      DLOG("Waiting for semaphore");
      EXPECT_EQ(sem->Wait(100), MAGMA_STATUS_TIMED_OUT);
      DLOG("Semaphore wait returned");
    }));
    thread->join();

    // Verify wait with no timeout
    thread.reset(new std::thread([sem] {
      DLOG("Waiting for semaphore");
      EXPECT_TRUE(sem->Wait());
      DLOG("Semaphore wait returned");
    }));
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    sem->Signal();
    thread->join();

    // Verify Reset - should timeout.
    sem->Signal();
    sem->Reset();
    thread.reset(new std::thread([sem] {
      DLOG("Waiting for semaphore");
      EXPECT_EQ(sem->Wait(100), MAGMA_STATUS_TIMED_OUT);
      DLOG("Semaphore wait returned");
    }));
    thread->join();
  }

  void TestImportEvent(bool one_shot = false) {
#if !defined(__Fuchsia__)
    GTEST_SKIP();
#endif
    zx::event event;
    ASSERT_EQ(ZX_OK, zx::event::create(/*options=*/0, &event));

    uint64_t flags = one_shot ? MAGMA_IMPORT_SEMAPHORE_ONE_SHOT : 0;

    auto semaphore = magma::PlatformSemaphore::Import(event.release(), flags);
    ASSERT_TRUE(semaphore);

    EXPECT_FALSE(semaphore->Wait(0));

    semaphore->Signal();
    EXPECT_TRUE(semaphore->Wait(0));

    semaphore->Reset();

    if (one_shot) {
      EXPECT_TRUE(semaphore->Wait(0));
    } else {
      EXPECT_FALSE(semaphore->Wait(0));
    }
  }

  void TestImportVmo(bool one_shot = false) {
#if !defined(__Fuchsia__)
    GTEST_SKIP();
#endif
    constexpr uint32_t kSize = 1;  // expect a page
    zx::vmo vmo;
    ASSERT_EQ(ZX_OK, zx::vmo::create(kSize, /*options=*/0, &vmo));

    uint64_t flags = one_shot ? MAGMA_IMPORT_SEMAPHORE_ONE_SHOT : 0;

    auto semaphore = magma::PlatformSemaphore::Import(vmo.release(), flags);
    ASSERT_TRUE(semaphore);

    EXPECT_FALSE(semaphore->Wait(0));

    semaphore->Signal();
    EXPECT_TRUE(semaphore->Wait(0));

    semaphore->Reset();

    if (one_shot) {
      EXPECT_TRUE(semaphore->Wait(0));
    } else {
      EXPECT_FALSE(semaphore->Wait(0));
    }
  }
};

}  // namespace

TEST_F(TestPlatformSemaphore, MultiThread) { TestMultiThread(); }

TEST_F(TestPlatformSemaphore, ImportEvent) { TestImportEvent(); }

TEST_F(TestPlatformSemaphore, ImportEventOneShot) { TestImportEvent(/*one_shot=*/true); }

TEST_F(TestPlatformSemaphore, ImportVmo) { TestImportVmo(); }

TEST_F(TestPlatformSemaphore, ImportVmoOneShot) { TestImportVmo(/*one_shot=*/true); }

TEST_F(TestPlatformSemaphore, Timestamp) {
  auto semaphore = magma::PlatformSemaphore::Create();

  uint64_t signal_timestamp_ns = 0;

  if (semaphore->GetTimestamp(&signal_timestamp_ns)) {
    EXPECT_EQ(signal_timestamp_ns, 0u);
  }

  uint64_t timestamp_ns =
      std::chrono::time_point_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now())
          .time_since_epoch()
          .count();

  semaphore->Signal();

  if (semaphore->GetTimestamp(&signal_timestamp_ns)) {
    EXPECT_GT(signal_timestamp_ns, timestamp_ns);
  }
}
