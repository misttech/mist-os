// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/default.h>
#include <lib/driver/power/cpp/wake-lease.h>
#include <lib/zx/clock.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls/object.h>
#include <zircon/time.h>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

namespace power_lib_test {

class WakeLeaseTest : public gtest::RealLoopFixture {};

TEST_F(WakeLeaseTest, TestWakeLeaseTimeouts) {
  fidl::Endpoints<fuchsia_power_system::ActivityGovernor> endpoints =
      fidl::Endpoints<fuchsia_power_system::ActivityGovernor>();
  // We're exploiting properties of the WakeLease implementation here, in
  // particular that it only talks to SAG, which we aren't faking, if it
  // needs a lease, not if it already has one. This means that WakeLease can
  // hold the contradictory thoughts in its head that we have a wake lease,
  // because it was deposited, but we are also suspended, because it hasn't
  // been told otherwise. Neat!

  fdf_power::WakeLease test_lease = fdf_power::WakeLease(async_get_default_dispatcher(),
                                                         "test-lease", std::move(endpoints.client));

  EXPECT_EQ(test_lease.GetNextTimeout(), ZX_TIME_INFINITE);
  zx::eventpair h1, h2, h3;
  zx::eventpair::create(0, &h1, &h2);
  h1.duplicate(ZX_RIGHT_SAME_RIGHTS, &h3);
  zx::time expire_time = zx::clock::get_monotonic() + zx::hour(1);
  test_lease.DepositWakeLease(std::move(h3), expire_time);
  EXPECT_EQ(test_lease.GetNextTimeout(), expire_time.get());

  h1.duplicate(ZX_RIGHT_SAME_RIGHTS, &h3);
  zx::time expire_time2 = expire_time - zx::min(1);
  test_lease.DepositWakeLease(std::move(h3), expire_time2);
  EXPECT_EQ(test_lease.GetNextTimeout(), expire_time.get());

  h1.duplicate(ZX_RIGHT_SAME_RIGHTS, &h3);
  zx::time expire_time3 = expire_time + zx::min(1);
  test_lease.DepositWakeLease(std::move(h3), expire_time3);
  EXPECT_EQ(test_lease.GetNextTimeout(), expire_time3.get());

  // Acquire a wake lease, specifying a timeout. The time the timeout is set
  // for should be between the time before the call and the time after the
  // call, plus the timeout.
  zx::duration timeout = zx::min(10);
  zx::time before = zx::clock::get_monotonic();
  test_lease.AcquireWakeLease(timeout);
  zx::time after = zx::clock::get_monotonic();
  EXPECT_GE(test_lease.GetNextTimeout(), (before + timeout).get());
  EXPECT_LE(test_lease.GetNextTimeout(), (after + timeout).get());

  // Acquire a new wake lease, but with a shorter timeout, which we expect to
  // be changed.
  timeout = zx::min(5);
  before = zx::clock::get_monotonic();
  test_lease.AcquireWakeLease(timeout);
  after = zx::clock::get_monotonic();
  zx_time_t next_timeout = test_lease.GetNextTimeout();
  EXPECT_GE(next_timeout, (before + timeout).get());
  EXPECT_LE(next_timeout, (after + timeout).get());

  // Since the wake lease has never heard anything about whether the system is
  // suspended or resumed, it assumes we're suspended, tell it we're resumed
  test_lease.SetSuspended(false);

  // Now tell it we need to keep the system awake until a given time
  before = zx::clock::get_monotonic();
  EXPECT_FALSE(test_lease.HandleInterrupt(timeout));
  // Add a little slack here because we get some small delays later on
  // since the implementation translates from an absolute time to an offset
  // and then posts the task at that offset a bit after calculating it. Local
  // testing indicates tens of microseconds of skew.
  after = zx::clock::get_monotonic() + zx::sec(2);

  // Right now we expect no reclamation task for the eventpair because the wake
  // lease considers the system awake.
  EXPECT_EQ(test_lease.GetNextTimeout(), next_timeout);

  // Now, let's "suspend", which should trigger a lease, and therefore a
  // task to reclaim it.
  test_lease.SetSuspended(true);
  next_timeout = test_lease.GetNextTimeout();
  EXPECT_GE(next_timeout, (before + timeout).get());
  EXPECT_LE(next_timeout, (after + timeout).get());
}
}  // namespace power_lib_test
