// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <thread>

#include <gtest/gtest.h>
#include <heapdump/stats.h>

// Verify that malloc and free calls are intercepted by the instrumentation by observing heapdump's
// counters.
// - The global counters may also be incremented as a result of other threads allocating or
//   deallocating in the background. Therefore, we can only test that they are incremented by *at
//   least* the amount of memory that we malloc/free.
// - The thread-local counters should reflect our malloc/free calls exactly.
TEST(StatsTest, CountersReflectMallocAndFree) {
  heapdump_global_stats init_global_stats;
  heapdump_thread_local_stats init_local_stats;
  heapdump_get_stats(&init_global_stats, &init_local_stats);

  const size_t kAllocSize = 1'000'000;
  void *ptr = malloc(kAllocSize);

  // Prevent the compiler from observing that we don't use the allocated memory block and optimizing
  // our malloc+free calls away.
  __asm__("" : "=r"(ptr) : "0"(ptr));

  heapdump_global_stats after_malloc_global_stats;
  heapdump_thread_local_stats after_malloc_local_stats;
  heapdump_get_stats(&after_malloc_global_stats, &after_malloc_local_stats);

  // Verify global stats.
  EXPECT_GE(after_malloc_global_stats.total_allocated_bytes,
            init_global_stats.total_allocated_bytes + kAllocSize);
  EXPECT_GE(after_malloc_global_stats.total_deallocated_bytes,
            init_global_stats.total_deallocated_bytes);

  // Verify local stats.
  EXPECT_EQ(after_malloc_local_stats.total_allocated_bytes,
            init_local_stats.total_allocated_bytes + kAllocSize);

  EXPECT_EQ(after_malloc_local_stats.total_deallocated_bytes,
            init_local_stats.total_deallocated_bytes);

  free(ptr);

  heapdump_global_stats after_free_global_stats;
  heapdump_thread_local_stats after_free_local_stats;
  heapdump_get_stats(&after_free_global_stats, &after_free_local_stats);

  // Verify global stats.
  EXPECT_GE(after_free_global_stats.total_allocated_bytes,
            after_malloc_global_stats.total_allocated_bytes);
  EXPECT_GE(after_free_global_stats.total_deallocated_bytes,
            after_malloc_global_stats.total_deallocated_bytes + kAllocSize);

  // Verify local stats.
  EXPECT_EQ(after_free_local_stats.total_allocated_bytes,
            after_malloc_local_stats.total_allocated_bytes);
  EXPECT_EQ(after_free_local_stats.total_deallocated_bytes,
            after_malloc_local_stats.total_deallocated_bytes + kAllocSize);
}

TEST(StatsTest, GlobalNullDoesNotCrash) {
  heapdump_thread_local_stats local_stats;
  heapdump_get_stats(nullptr, &local_stats);
}

TEST(StatsTest, ThreadLocalNullDoesNotCrash) {
  heapdump_global_stats global_stats;
  heapdump_get_stats(&global_stats, nullptr);
}

// Spawn a new thread and verify that its thread-local counters start from zero.
TEST(StatsTest, ThreadLocalShouldStartFromZero) {
  heapdump_thread_local_stats thread_local_stats;

  std::thread thread([&thread_local_stats]() { heapdump_get_stats(nullptr, &thread_local_stats); });
  thread.join();

  EXPECT_EQ(0u, thread_local_stats.total_allocated_bytes);
  EXPECT_EQ(0u, thread_local_stats.total_deallocated_bytes);
}
