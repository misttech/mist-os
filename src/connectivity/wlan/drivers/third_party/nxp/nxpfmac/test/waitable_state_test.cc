// Copyright (c) 2022 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/waitable_state.h"

#include <stdint.h>

#include <thread>

#include <zxtest/zxtest.h>

namespace {

using wlan::nxpfmac::WaitableState;

TEST(WaitableStateTest, StoringAndLoading) {
  // Test that the state can store and load values as expected.

  WaitableState<int> state(0);
  ASSERT_EQ(0, state.Load());

  state.Store(13);
  ASSERT_EQ(13, state.Load());

  // Assign with operator
  state = 42;
  // Use static_cast to trigger the use of the conversion operator
  ASSERT_EQ(42, static_cast<int>(state));
}

TEST(WaitableStateTest, WaitForState) {
  // Test that one thread can successfully wait for the state to store a specific value

  constexpr uint32_t kNumberOfWaits = 100;

  WaitableState<uint32_t> state(0);

  std::atomic<bool> running = true;
  std::mutex mutex;

  std::unique_lock lock(mutex);
  std::atomic<uint64_t> iterations_needed = 0;

  // Start a thread that stores all values in the range 0 to kNumberOfWaits - 1.
  std::thread storing_thread([&]() {
    for (uint32_t i = 0; running; ++i) {
      std::lock_guard lock(mutex);
      state.Store(i % kNumberOfWaits);
      ++iterations_needed;
    }
  });

  // Then wait for each of the values in that range, sooner or later they should all be set.
  for (uint32_t i = 0; i < kNumberOfWaits; ++i) {
    state.Wait(lock, i);
  }

  running = false;
  // Unlock before attempting to join the thread.
  lock.unlock();
  storing_thread.join();
}

TEST(WaitableStateTest, Comparisons) {
  enum class Color { Red, Green, Blue };

  WaitableState<Color> state(Color::Red);
  ASSERT_TRUE(state == Color::Red);
  ASSERT_TRUE(state != Color::Green);
  ASSERT_TRUE(state != Color::Blue);
  ASSERT_FALSE(state != Color::Red);
  ASSERT_FALSE(state == Color::Green);
  ASSERT_FALSE(state == Color::Blue);

  state = Color::Green;
  ASSERT_TRUE(state != Color::Red);
  ASSERT_TRUE(state == Color::Green);
  ASSERT_TRUE(state != Color::Blue);
  ASSERT_FALSE(state == Color::Red);
  ASSERT_FALSE(state != Color::Green);
  ASSERT_FALSE(state == Color::Blue);

  state = Color::Blue;
  ASSERT_TRUE(state != Color::Red);
  ASSERT_TRUE(state != Color::Green);
  ASSERT_TRUE(state == Color::Blue);
  ASSERT_FALSE(state == Color::Red);
  ASSERT_FALSE(state == Color::Green);
  ASSERT_FALSE(state != Color::Blue);
}

TEST(WaitableStateTest, WaitForWithTimeout) {
  // Test that WaitFor correctly times out after the given time and that it returns false.

  constexpr zx_duration_t kTimeout = ZX_MSEC(5);
  enum class State { Broken, On, Off };

  WaitableState<State> state(State::Off);

  std::mutex mutex;
  std::unique_lock lock(mutex);

  const zx_time_t start = zx_clock_get_monotonic();

  // WaitFor returns false on timeout, make sure it does.
  ASSERT_FALSE(state.WaitFor(lock, State::On, kTimeout));

  zx_duration_t elapsed = zx_clock_get_monotonic() - start;
  // At least kTimeout amount of time should have passed.
  ASSERT_GE(elapsed, kTimeout);
}

TEST(WaitableStateTest, WaitForWithSuccess) {
  // Test that WaitFor correctly returns true when the state changes to the expected state.

  constexpr zx_duration_t kTimeout = ZX_SEC(60);
  enum class State { Broken, On, Off };
  constexpr State kExpectedState = State::On;

  WaitableState<State> state(State::Off);

  std::mutex mutex;

  std::thread waiting_thread([&]() {
    std::unique_lock lock(mutex);
    ASSERT_TRUE(state.WaitFor(lock, kExpectedState, kTimeout));
    ASSERT_EQ(kExpectedState, state);
  });
  {
    std::unique_lock lock(mutex);
    state = kExpectedState;
  }
  waiting_thread.join();
}

}  // namespace
