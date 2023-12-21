// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/hot-plug-detection.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/async-loop/loop.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <atomic>
#include <optional>

#include <fbl/auto_lock.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/lib/testing/predicates/status.h"

namespace amlogic_display {

namespace {

struct GpioResources {
  fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> client;
  zx::interrupt interrupt;
};

class HotPlugDetectionTest : public ::gtest::RealLoopFixture {
 public:
  void SetUp() override {
    zx_status_t status =
        zx::interrupt::create(zx::resource(), 0u, ZX_INTERRUPT_VIRTUAL, &pin_gpio_interrupt_);
    ASSERT_OK(status);

    zx::interrupt gpio_interrupt;
    status = pin_gpio_interrupt_.duplicate(ZX_RIGHT_SAME_RIGHTS, &gpio_interrupt);
    ASSERT_OK(status);

    pin_gpio_.SetInterrupt(zx::ok(std::move(gpio_interrupt)));
    pin_gpio_.SetDefaultReadResponse(zx::ok(uint8_t{0u}));
  }

  fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> GetPinGpioClient() { return pin_gpio_.Connect(); }

  GpioResources GetPinGpioResources() {
    fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> pin_gpio_client = GetPinGpioClient();
    zx::interrupt pin_gpio_interrupt = PerformBlockingWork([&]() -> zx::interrupt {
      fidl::WireResult result =
          fidl::WireCall(pin_gpio_client)->GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_HIGH);
      ZX_ASSERT_MSG(result.ok(), "FIDL connection failed: %s", result.status_string());
      fidl::WireResultUnwrapType<fuchsia_hardware_gpio::Gpio::GetInterrupt>& interrupt_value =
          result.value();
      ZX_ASSERT_MSG(interrupt_value.is_ok(), "GPIO GetInterrupt failed: %s",
                    zx_status_get_string(interrupt_value.error_value()));
      return std::move(interrupt_value.value()->irq);
    });

    return {
        .client = std::move(pin_gpio_client),
        .interrupt = std::move(pin_gpio_interrupt),
    };
  }

  std::unique_ptr<HotPlugDetection> CreateAndInitHotPlugDetection() {
    GpioResources pin_gpio_resources = GetPinGpioResources();
    auto hpd = std::make_unique<HotPlugDetection>(
        std::move(pin_gpio_resources.client), std::move(pin_gpio_resources.interrupt),
        [this](HotPlugDetectionState state) { RecordHotPlugDetectionState(state); });

    // HotPlugDetection::Init() sets up the GPIO using synchronous FIDL calls.
    // The fake GPIO FIDL server can only be bound on the test thread's default
    // dispatcher, so Init() must be called on another thread.
    zx::result<> init_result = PerformBlockingWork([&] { return hpd->Init(); });
    EXPECT_OK(init_result.status_value());

    return hpd;
  }

  void RecordHotPlugDetectionState(HotPlugDetectionState state) {
    fbl::AutoLock lock(&mutex_);
    recorded_detection_states_.push_back(state);
  }

  std::vector<HotPlugDetectionState> GetHotPlugDetectionStates() const {
    fbl::AutoLock lock(&mutex_);
    return recorded_detection_states_;
  }

 protected:
  fake_gpio::FakeGpio pin_gpio_;
  zx::interrupt pin_gpio_interrupt_;

  mutable fbl::Mutex mutex_;
  std::vector<HotPlugDetectionState> recorded_detection_states_ TA_GUARDED(&mutex_);
};

TEST_F(HotPlugDetectionTest, NoHotplugEvents) {
  std::unique_ptr<HotPlugDetection> hpd = CreateAndInitHotPlugDetection();
}

TEST_F(HotPlugDetectionTest, DisplayPlug) {
  std::unique_ptr<HotPlugDetection> hpd = CreateAndInitHotPlugDetection();

  pin_gpio_interrupt_.trigger(0u, zx::clock::get_monotonic());
  pin_gpio_.SetDefaultReadResponse(zx::ok(uint8_t{1u}));

  RunLoopUntil([&] { return GetHotPlugDetectionStates().size() >= 1; });
  EXPECT_THAT(GetHotPlugDetectionStates(), testing::ElementsAre(HotPlugDetectionState::kDetected));
  EXPECT_EQ(pin_gpio_.GetPolarity(), fuchsia_hardware_gpio::GpioPolarity::kLow);
}

TEST_F(HotPlugDetectionTest, DisplayPlugUnplug) {
  std::unique_ptr<HotPlugDetection> hpd = CreateAndInitHotPlugDetection();

  // Simulate plugging the display.
  pin_gpio_interrupt_.trigger(0u, zx::clock::get_monotonic());
  pin_gpio_.SetDefaultReadResponse(zx::ok(uint8_t{1u}));
  RunLoopUntil([&] { return GetHotPlugDetectionStates().size() >= 1; });
  EXPECT_THAT(GetHotPlugDetectionStates(), testing::ElementsAre(HotPlugDetectionState::kDetected));

  // Simulate unplugging the display.
  pin_gpio_interrupt_.trigger(0u, zx::clock::get_monotonic());
  pin_gpio_.SetDefaultReadResponse(zx::ok(uint8_t{0u}));
  RunLoopUntil([&] { return GetHotPlugDetectionStates().size() >= 2; });
  EXPECT_THAT(
      GetHotPlugDetectionStates(),
      testing::ElementsAre(HotPlugDetectionState::kDetected, HotPlugDetectionState::kNotDetected));

  EXPECT_EQ(pin_gpio_.GetPolarity(), fuchsia_hardware_gpio::GpioPolarity::kHigh);
}

TEST_F(HotPlugDetectionTest, SpuriousPlugInterrupt) {
  std::unique_ptr<HotPlugDetection> hpd = CreateAndInitHotPlugDetection();

  const size_t num_state_changes_before_hotplug_gpio_read = pin_gpio_.GetStateLog().size();

  std::atomic<bool> hotplug_gpio_read = false;
  pin_gpio_interrupt_.trigger(0u, zx::clock::get_monotonic());
  pin_gpio_.PushReadCallback([&](fake_gpio::FakeGpio& gpio) {
    hotplug_gpio_read.store(true, std::memory_order_relaxed);
    return zx::ok(uint8_t{0});
  });
  RunLoopUntil([&] { return hotplug_gpio_read.load(std::memory_order_relaxed); });

  const size_t num_state_changes_after_hotplug_gpio_read = pin_gpio_.GetStateLog().size();

  // The GPIO state (polarity, input / output config) should not change if the
  // GPIO reading doesn't change.
  EXPECT_EQ(num_state_changes_after_hotplug_gpio_read, num_state_changes_before_hotplug_gpio_read);

  EXPECT_THAT(GetHotPlugDetectionStates(), testing::IsEmpty());
}

TEST_F(HotPlugDetectionTest, SpuriousUnplugInterrupt) {
  std::unique_ptr<HotPlugDetection> hpd = CreateAndInitHotPlugDetection();

  std::atomic<bool> first_hotplug_gpio_read = false;
  pin_gpio_interrupt_.trigger(0u, zx::clock::get_monotonic());
  pin_gpio_.PushReadCallback([&](fake_gpio::FakeGpio& gpio) {
    first_hotplug_gpio_read.store(true, std::memory_order_relaxed);
    return zx::ok(uint8_t{1});
  });
  RunLoopUntil([&] {
    return first_hotplug_gpio_read.load(std::memory_order_relaxed) &&
           GetHotPlugDetectionStates().size() >= 1;
  });

  EXPECT_THAT(GetHotPlugDetectionStates(), testing::ElementsAre(HotPlugDetectionState::kDetected));

  const size_t num_state_changes_before_second_hotplug_gpio_read = pin_gpio_.GetStateLog().size();

  std::atomic<bool> second_hotplug_gpio_read = false;
  pin_gpio_interrupt_.trigger(0u, zx::clock::get_monotonic());
  pin_gpio_.PushReadCallback([&](fake_gpio::FakeGpio& gpio) {
    second_hotplug_gpio_read.store(true, std::memory_order_relaxed);
    return zx::ok(uint8_t{1});
  });
  RunLoopUntil([&] { return second_hotplug_gpio_read.load(std::memory_order_relaxed); });

  const size_t num_state_changes_after_second_hotplug_gpio_read = pin_gpio_.GetStateLog().size();

  EXPECT_THAT(GetHotPlugDetectionStates(), testing::ElementsAre(HotPlugDetectionState::kDetected));

  // The GPIO state (polarity, input / output config) should not change if the
  // GPIO reading doesn't change.
  EXPECT_EQ(num_state_changes_after_second_hotplug_gpio_read,
            num_state_changes_before_second_hotplug_gpio_read);
}

}  // namespace

}  // namespace amlogic_display
