// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/fs_test/fs_test_fixture.h"

#include <zircon/errors.h>
#include <zircon/syscalls/clock.h>
#include <zircon/utc.h>

#include <gtest/gtest-spi.h>

namespace fs_test {

BaseFilesystemTest::~BaseFilesystemTest() {
  if (fs_.is_mounted()) {
    EXPECT_EQ(fs_.Unmount().status_value(), ZX_OK);
  }
  EXPECT_EQ(fs_.Fsck().status_value(), ZX_OK);
}

void BaseFilesystemTest::RunSimulatedPowerCutTest(const PowerCutOptions& options,
                                                  const std::function<void()>& test_function) {
  ASSERT_FALSE(fs().options().use_ram_nand);                   // This only works with ram-disks.
  ASSERT_EQ(fs().GetRamDisk()->Wake().status_value(), ZX_OK);  // This resets counts.

  // Make sure the test function runs without any failures.
  ASSERT_NO_FATAL_FAILURE(test_function());

  ramdisk_block_write_counts_t counts;
  ASSERT_EQ(ramdisk_get_block_counts(fs().GetRamDisk()->client(), &counts), ZX_OK);

  std::cout << "Total block count: " << counts.received << std::endl;

  // Now repeatedly stop writes after a certain block number.
  for (uint64_t block_cut = 1; block_cut < counts.received; block_cut += options.stride) {
    ASSERT_EQ(fs().GetRamDisk()->SleepAfter(block_cut).status_value(), ZX_OK);
    {
      // Ignore any test failures whilst we are doing this.
      testing::TestPartResultArray result;
      testing::ScopedFakeTestPartResultReporter reporter(
          testing::ScopedFakeTestPartResultReporter::InterceptMode::INTERCEPT_ALL_THREADS, &result);
      test_function();
    }
    ASSERT_EQ(fs().Unmount().status_value(), ZX_OK);
    ASSERT_EQ(fs().GetRamDisk()->Wake().status_value(), ZX_OK);
    ASSERT_EQ(fs().Fsck().status_value(), ZX_OK);
    if (options.reformat) {
      ASSERT_EQ(fs().Format().status_value(), ZX_OK);
    }
    ASSERT_EQ(fs().Mount().status_value(), ZX_OK);
  }
}

BaseFilesystemTest::ClockSwapper::ClockSwapper() {
  // First, get the current UTC reference clock details so we can obtain the backstop time.
  zx_clock_details_v1_t utc_details;
  ZX_ASSERT(zx_clock_get_details(zx_utc_reference_get(), ZX_CLOCK_ARGS_VERSION(1), &utc_details) ==
            ZX_OK);
  ZX_ASSERT(utc_details.backstop_time > 0);
  // Create a fake monotonic clock to replace the UTC reference with.
  zx_handle_t fake_clock;
  ZX_ASSERT(zx_clock_create(ZX_CLOCK_OPT_MONOTONIC | ZX_CLOCK_OPT_CONTINUOUS, nullptr,
                            &fake_clock) == ZX_OK);
  // Update the clock with the UTC reference backstop time. This also starts the clock ticking.
  const zx_clock_update_args_v2_t args{.synthetic_value = utc_details.backstop_time};
  ZX_ASSERT(zx_clock_update(fake_clock,
                            ZX_CLOCK_ARGS_VERSION(2) | ZX_CLOCK_UPDATE_OPTION_SYNTHETIC_VALUE_VALID,
                            &args) == ZX_OK);
  // Swap the UTC reference clock with our fake.
  ZX_ASSERT(zx_utc_reference_swap(fake_clock, &utc_clock_) == ZX_OK);
}

BaseFilesystemTest::ClockSwapper::~ClockSwapper() {
  // Swap the original UTC reference clock back.
  zx_handle_t fake_clock;
  ZX_ASSERT(zx_utc_reference_swap(utc_clock_, &fake_clock) == ZX_OK);
  ZX_ASSERT(zx_handle_close(fake_clock) == ZX_OK);
}

}  // namespace fs_test
