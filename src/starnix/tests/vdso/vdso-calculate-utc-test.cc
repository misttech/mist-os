// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <thread>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/starnix/kernel/vdso/vdso-calculate-time.h"

vvar_data vvar;

namespace {
int64_t reference_value;
const int64_t INITIAL_REFERENCE_TIME_VALUE = 10'000;
const int64_t REFERENCE_INCREMENT_SIZE = 100;

struct VvarTransform {
  int64_t boot_to_utc_reference_offset;
  int64_t boot_to_utc_synthetic_offset;
  uint32_t boot_to_utc_reference_ticks;
  uint32_t boot_to_utc_synthetic_ticks;
  int64_t apply(int64_t reference_time_nsec) const {
    return ((reference_time_nsec - boot_to_utc_reference_offset) * boot_to_utc_synthetic_ticks /
            boot_to_utc_reference_ticks) +
           boot_to_utc_synthetic_offset;
  }
};

VvarTransform transform1 = {.boot_to_utc_reference_offset = 1001,
                            .boot_to_utc_synthetic_offset = 2002,
                            .boot_to_utc_reference_ticks = 3003,
                            .boot_to_utc_synthetic_ticks = 4004};

VvarTransform transform2 = {.boot_to_utc_reference_offset = 160,
                            .boot_to_utc_synthetic_offset = 230,
                            .boot_to_utc_reference_ticks = 5,
                            .boot_to_utc_synthetic_ticks = 20};

class VdsoCalculateUtcTest : public ::testing::Test {
 protected:
  void SetUp() override {
    reference_value = INITIAL_REFERENCE_TIME_VALUE;
    // Seq_num says that data can be read.
    vvar.seq_num.store(4, std::memory_order_release);
    vvar.boot_to_utc_reference_offset.store(transform1.boot_to_utc_reference_offset,
                                            std::memory_order_release);
    vvar.boot_to_utc_synthetic_offset.store(transform1.boot_to_utc_synthetic_offset,
                                            std::memory_order_release);
    vvar.boot_to_utc_reference_ticks.store(transform1.boot_to_utc_reference_ticks,
                                           std::memory_order_release);
    vvar.boot_to_utc_synthetic_ticks.store(transform1.boot_to_utc_synthetic_ticks,
                                           std::memory_order_release);
  }
};

void writer_thread(std::atomic_bool *should_stop) {
  vvar.seq_num.store(0, std::memory_order_release);
  int i = 0;
  // Writer thread constantly updates vvar until should_stop becomes true
  while (!should_stop->load()) {
    i++;
    const VvarTransform &current_transform = (i % 2) ? transform1 : transform2;
    std::atomic_fetch_add(&vvar.seq_num, 1);
    vvar.boot_to_utc_reference_offset.store(current_transform.boot_to_utc_reference_offset,
                                            std::memory_order_release);
    vvar.boot_to_utc_synthetic_offset.store(current_transform.boot_to_utc_synthetic_offset,
                                            std::memory_order_release);
    vvar.boot_to_utc_reference_ticks.store(current_transform.boot_to_utc_reference_ticks,
                                           std::memory_order_release);
    vvar.boot_to_utc_synthetic_ticks.store(current_transform.boot_to_utc_synthetic_ticks,
                                           std::memory_order_release);
    std::atomic_fetch_add(&vvar.seq_num, 1);
  }
}
}  // namespace

// Custom function which replaces the normal calculate_monotonic_time_nsec.
// By adjusting the value of monotonic_value, the monotonic time can be custom set.
int64_t calculate_monotonic_time_nsec() { return reference_value; }

TEST_F(VdsoCalculateUtcTest, ValidVvarAccess) {
  int64_t expected_value =
      (calculate_monotonic_time_nsec() - transform1.boot_to_utc_reference_offset) *
          transform1.boot_to_utc_synthetic_ticks / transform1.boot_to_utc_reference_ticks +
      transform1.boot_to_utc_synthetic_offset;
  int64_t utc_result = calculate_utc_time_nsec();
  ASSERT_EQ(utc_result, expected_value);
}

TEST_F(VdsoCalculateUtcTest, InvalidVvarAccess) {
  // Seq_num says that an update is in progress.
  vvar.seq_num.store(5, std::memory_order_release);
  int64_t utc_result = calculate_utc_time_nsec();
  ASSERT_EQ(utc_result, kUtcInvalid);
}

TEST_F(VdsoCalculateUtcTest, UtcCalculationIncreasesWithMono) {
  int64_t prev_utc_value = calculate_utc_time_nsec();
  ASSERT_NE(prev_utc_value, kUtcInvalid);
  for (int i = 0; i < 100; i++) {
    // Reference time increases. Check that utc time also increases.
    reference_value += REFERENCE_INCREMENT_SIZE;
    int64_t current_utc_value = calculate_utc_time_nsec();
    ASSERT_NE(current_utc_value, kUtcInvalid);
    ASSERT_GT(current_utc_value, prev_utc_value);
    prev_utc_value = current_utc_value;
  }
}

TEST_F(VdsoCalculateUtcTest, SeqlockThreadSafe) {
  // A writer thread constantly updates vvar data so that the transform is one of 2 valid values.
  // The loop below constantly reads vvar data and checks that the calculated utc time is
  // either one of the outputs that one of the valid transforms would produce, or kUtcInvalid.
  std::atomic_bool should_stop = false;
  std::thread writer(writer_thread, &should_stop);
  int64_t possible_utc_values[2] = {transform1.apply(INITIAL_REFERENCE_TIME_VALUE),
                                    transform2.apply(INITIAL_REFERENCE_TIME_VALUE)};
  int64_t valid_value_counter[2] = {0, 0};
  while (valid_value_counter[0] < 500 || valid_value_counter[1] < 500) {
    int64_t utc_result = calculate_utc_time_nsec();
    ASSERT_THAT(utc_result,
                testing::AnyOf(testing::Eq(kUtcInvalid), testing::Eq(possible_utc_values[0]),
                               testing::Eq(possible_utc_values[1])));
    // Count the number of times each of the possible utc values is calculated.
    // Stop the loop and stop the writing thread once each value has been calculated at least
    // 500 times.
    if (utc_result == possible_utc_values[0]) {
      valid_value_counter[0]++;
    } else if (utc_result == possible_utc_values[1]) {
      valid_value_counter[1]++;
    }
  }
  should_stop.store(true);
  writer.join();
}
