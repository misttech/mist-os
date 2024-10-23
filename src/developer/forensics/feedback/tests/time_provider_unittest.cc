// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/time_provider.h"

#include <fuchsia/time/cpp/fidl.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/lib/timekeeper/test_clock.h"

namespace forensics::feedback {
namespace {

constexpr timekeeper::time_utc kTime((zx::hour(7) + zx::min(14) + zx::sec(52)).get());
constexpr char kTimeStr[] = "1970-01-01 07:14:52 GMT";

class TimeProviderTest : public UnitTestFixture {
 public:
  TimeProviderTest() {
    zx_clock_create_args_v1_t clock_args{.backstop_time = 0};
    FX_CHECK(zx::clock::create(0u, &clock_args, &clock_handle_) == ZX_OK);
  }

 protected:
  void SignalLoggingQualityClock() {
    if (const zx_status_t status =
            clock_handle_.signal(/*clear_mask=*/0,
                                 /*set_mask=*/fuchsia::time::SIGNAL_UTC_CLOCK_LOGGING_QUALITY);
        status != ZX_OK) {
      FX_PLOGS(FATAL, status) << "Failed to achieve logging quality clock";
    }
  }

  void SetUpTimeProvider() {
    auto clock = std::make_unique<timekeeper::TestClock>();
    clock->SetUtc(kTime);

    time_provider_ = std::make_unique<TimeProvider>(
        dispatcher(), zx::unowned_clock(clock_handle_.get_handle()), std::move(clock));
  }

  zx::clock clock_handle_;
  std::unique_ptr<TimeProvider> time_provider_;
};

TEST_F(TimeProviderTest, Check_LoggingQualityClock) {
  SetUpTimeProvider();
  EXPECT_FALSE(time_provider_->Get().at(kDeviceUtcTimeKey).HasValue());

  SignalLoggingQualityClock();
  RunLoopUntilIdle();

  ASSERT_TRUE(time_provider_->Get().at(kDeviceUtcTimeKey).HasValue());
  EXPECT_EQ(time_provider_->Get().at(kDeviceUtcTimeKey).Value(), kTimeStr);
}

TEST_F(TimeProviderTest, Check_LoggingQualityClockBeforeFeedback) {
  SignalLoggingQualityClock();
  SetUpTimeProvider();
  RunLoopUntilIdle();

  ASSERT_TRUE(time_provider_->Get().at(kDeviceUtcTimeKey).HasValue());
  EXPECT_EQ(time_provider_->Get().at(kDeviceUtcTimeKey).Value(), kTimeStr);
}

TEST_F(TimeProviderTest, Check_NotReadyOnClockStarted) {
  SetUpTimeProvider();
  EXPECT_FALSE(time_provider_->Get().at(kDeviceUtcTimeKey).HasValue());

  ASSERT_EQ(clock_handle_.update(zx::clock::update_args().set_value(zx::time(kTime.get()))), ZX_OK);
  RunLoopUntilIdle();

  EXPECT_FALSE(time_provider_->Get().at(kDeviceUtcTimeKey).HasValue());
}

TEST_F(TimeProviderTest, Check_NotReadyOnClockSynchronized) {
  SetUpTimeProvider();
  EXPECT_FALSE(time_provider_->Get().at(kDeviceUtcTimeKey).HasValue());

  ASSERT_EQ(clock_handle_.signal(
                /*clear_mask=*/0, /*set_mask=*/fuchsia::time::SIGNAL_UTC_CLOCK_SYNCHRONIZED),
            ZX_OK);
  RunLoopUntilIdle();

  EXPECT_FALSE(time_provider_->Get().at(kDeviceUtcTimeKey).HasValue());
}

}  // namespace
}  // namespace forensics::feedback
