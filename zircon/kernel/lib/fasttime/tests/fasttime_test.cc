// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/fasttime/time.h>
#include <lib/fdio/io.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

namespace {

constexpr char kVmoFileDir[] = "/boot/kernel";

TEST(FasttimeTest, UnaccessibleTicks) {
  constexpr fasttime::internal::TimeValues values = {
      .version = fasttime::internal::kFasttimeVersion,
      .ticks_per_second = 100000,
      .mono_ticks_offset = 12345678,
      .ticks_to_time_numerator = 125000,
      .ticks_to_time_denominator = 300000,
      .usermode_can_access_ticks = false,
      .use_a73_errata_mitigation = false,
  };
  // Test that libfasttime returns the sentinel value of ZX_TIME_INFINITE_PAST from all calls
  // when the ticks register is not usermode accessible.
  EXPECT_EQ(compute_monotonic_time(values), ZX_TIME_INFINITE_PAST);
  EXPECT_EQ(compute_monotonic_ticks(values), ZX_TIME_INFINITE_PAST);

  // Test that the unchecked variants of these calls do not return the sential value.
  EXPECT_NE(fasttime::internal::compute_monotonic_time<
                fasttime::internal::FasttimeVerificationMode::kSkip>(values),
            ZX_TIME_INFINITE_PAST);
  EXPECT_NE(fasttime::internal::compute_monotonic_ticks<
                fasttime::internal::FasttimeVerificationMode::kSkip>(values),
            ZX_TIME_INFINITE_PAST);

  // Check that the functions exposed in the fasttime namespace return the sentinel value.
  const zx_vaddr_t time_values_addr = reinterpret_cast<zx_vaddr_t>(&values);
  EXPECT_EQ(fasttime::compute_monotonic_time(time_values_addr), ZX_TIME_INFINITE_PAST);
  EXPECT_EQ(fasttime::compute_monotonic_ticks(time_values_addr), ZX_TIME_INFINITE_PAST);
}

TEST(FasttimeTest, MismatchedVersions) {
  constexpr fasttime::internal::TimeValues values = {
      .version = fasttime::internal::kFasttimeVersion + 1,
      .ticks_per_second = 100000,
      .mono_ticks_offset = 12345678,
      .ticks_to_time_numerator = 125000,
      .ticks_to_time_denominator = 300000,
      .usermode_can_access_ticks = true,
      .use_a73_errata_mitigation = false,
  };

  // Test that a mismatch between the version of libfasttime and the version in the time_values
  // struct returns the sentinel value of ZX_TIME_INFINITE_PAST.
  EXPECT_FALSE(check_fasttime_version(values));
  EXPECT_EQ(compute_monotonic_time(values), ZX_TIME_INFINITE_PAST);
  EXPECT_EQ(compute_monotonic_ticks(values), ZX_TIME_INFINITE_PAST);

  // Test that the unchecked variants of these calls do not return the sential value.
  EXPECT_NE(fasttime::internal::compute_monotonic_time<
                fasttime::internal::FasttimeVerificationMode::kSkip>(values),
            ZX_TIME_INFINITE_PAST);
  EXPECT_NE(fasttime::internal::compute_monotonic_ticks<
                fasttime::internal::FasttimeVerificationMode::kSkip>(values),
            ZX_TIME_INFINITE_PAST);

  // Check that the functions exposed in the fasttime namespace return the sentinel value.
  const zx_vaddr_t time_values_addr = reinterpret_cast<zx_vaddr_t>(&values);
  EXPECT_EQ(fasttime::compute_monotonic_time(time_values_addr), ZX_TIME_INFINITE_PAST);
  EXPECT_EQ(fasttime::compute_monotonic_ticks(time_values_addr), ZX_TIME_INFINITE_PAST);
}

TEST(FasttimeTest, ComputeMonotonicTicks) {
  fbl::unique_fd dir_fd(open(kVmoFileDir, O_RDONLY | O_DIRECTORY));
  fbl::unique_fd time_values_fd(openat(dir_fd.get(), kTimeValuesVmoName, O_RDONLY));
  zx::vmo time_values_vmo;
  ASSERT_EQ(fdio_get_vmo_exact(time_values_fd.get(), time_values_vmo.reset_and_get_address()),
            ZX_OK);

  uint64_t vmo_size;
  ASSERT_EQ(time_values_vmo.get_size(&vmo_size), ZX_OK);

  fzl::OwnedVmoMapper time_values_mapper;
  ASSERT_EQ(time_values_mapper.Map(std::move(time_values_vmo), 0, vmo_size, ZX_VM_PERM_READ),
            ZX_OK);
  const zx_vaddr_t time_values_addr = reinterpret_cast<zx_vaddr_t>(time_values_mapper.start());

  // Ensure that compute_monotonic_ticks and zx_ticks_get return the same values.
  // Unfortunately, some time may elapse between these calls, so we cannot simply assert equality.
  // Instead, we invoke both functions consecutively and ascertain that the result of the first
  // call is less than that of the second.
  zx::ticks zircon_now = zx::ticks::now();
  zx::ticks fasttime_now = zx::ticks(fasttime::compute_monotonic_ticks(time_values_addr));
  EXPECT_LE(zircon_now, fasttime_now);

  fasttime_now = zx::ticks(fasttime::compute_monotonic_ticks(time_values_addr));
  zircon_now = zx::ticks::now();
  EXPECT_LE(fasttime_now, zircon_now);
}

TEST(FasttimeTest, ComputeMonotonicTime) {
  fbl::unique_fd dir_fd(open(kVmoFileDir, O_RDONLY | O_DIRECTORY));
  fbl::unique_fd time_values_fd(openat(dir_fd.get(), kTimeValuesVmoName, O_RDONLY));
  zx::vmo time_values_vmo;
  ASSERT_EQ(fdio_get_vmo_exact(time_values_fd.get(), time_values_vmo.reset_and_get_address()),
            ZX_OK);

  uint64_t vmo_size;
  ASSERT_EQ(time_values_vmo.get_size(&vmo_size), ZX_OK);

  fzl::OwnedVmoMapper time_values_mapper;
  ASSERT_EQ(time_values_mapper.Map(std::move(time_values_vmo), 0, vmo_size, ZX_VM_PERM_READ),
            ZX_OK);
  const zx_vaddr_t time_values_addr = reinterpret_cast<zx_vaddr_t>(time_values_mapper.start());

  // Ensure that compute_monotonic_time and zx_clock_get_monotonic return the same values.
  // Unfortunately, some time may elapse between these calls, so we cannot assert equality.
  // Instead, we invoke both functions consecutively and ascertain that the result of the first
  // call is less than that of the second.
  zx::time zircon_time = zx::clock::get_monotonic();
  zx::time fasttime_time = zx::time(fasttime::compute_monotonic_time(time_values_addr));
  EXPECT_LE(zircon_time, fasttime_time);

  fasttime_time = zx::time(fasttime::compute_monotonic_time(time_values_addr));
  zircon_time = zx::clock::get_monotonic();
  EXPECT_LE(fasttime_time, zircon_time);
}

}  // namespace
