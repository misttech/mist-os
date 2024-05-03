// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/concurrent/chainlock.h>
#include <stdint.h>
#include <zircon/compiler.h>

#include <atomic>
#include <chrono>
#include <thread>

#include <zxtest/zxtest.h>

namespace test {

using concurrent::ChainLock;

TEST(ChainLock, UncontestedAcquire) {
  // The lock itself.
  ChainLock lock;

  // To lock the lock, we are going to need a token, and a specific result code.
  ChainLock::Token token;
  ChainLock::LockResult result = lock.Acquire(token);

  // We should always get the lock, there is no one to contest it.
  ASSERT_EQ(ChainLock::LockResult::kOk, result);

  // We need to prove to the static analyzer that we hold the lock before we release it.
  lock.AssertHeld(token);
  lock.Release();
}

TEST(ChainLock, BackoffAcquire) {
  ChainLock lock;
  ChainLock::Token token1;
  ChainLock::Token token2;

  // Obtain the lock with the first token.
  ChainLock::LockResult result = lock.Acquire(token1);
  ASSERT_EQ(ChainLock::LockResult::kOk, result);

  // Now, attempt to acquire the lock with the second token.  The second token
  // was created after the first, so the first token should have priority.  When
  // we see that the lock is contested, and contested by someone with priority,
  // we should be told that we need to backoff and try again later.
  result = lock.Acquire(token2);
  EXPECT_EQ(ChainLock::LockResult::kBackoff, result);

  // We are holding the lock, however, just using token 1, not token 2.  We need
  // to assert this before the static analyzer will allow is to drop the lock.
  lock.AssertHeld(token1);
  lock.Release();
}

TEST(ChainLock, CyclicAcquire) {
  ChainLock lock;
  ChainLock::Token token;

  // Obtain the lock.
  ChainLock::LockResult result = lock.Acquire(token);
  ASSERT_EQ(ChainLock::LockResult::kOk, result);

  // Now try to obtain it again with the same token.  The result should be that
  // we detect a cycle.
  result = lock.Acquire(token);
  EXPECT_EQ(ChainLock::LockResult::kCycleDetected, result);

  // We still need to drop the lock that we are holding, however.
  lock.AssertHeld(token);
  lock.Release();
}

TEST(ChainLock, SpinAcquire) {
  // This test is a bit more complicated than all of the other tests, because it
  // will require us to use at least one more thread.  We are going to set up a
  // situation where someone with a lower priority is holding the lock, and a
  // higher priority acquisition operation ends up needing to wait for the lock.
  ChainLock lock;
  ChainLock::Token high_prio_token;
  ChainLock::Token low_prio_token;
  using namespace std::chrono_literals;

  // Obtain the lock using the lower priority token.
  ChainLock::LockResult result = lock.Acquire(low_prio_token);
  ASSERT_EQ(ChainLock::LockResult::kOk, result);

  enum class State {
    Initial,
    WaitingForLock,
    LockAcquired,
    DropLock,
  };
  std::atomic<State> state{State::Initial};

  // Create a thread to contest the lock, and have it use the higher priority
  // token.
  std::thread t1{[&lock, token = high_prio_token, &state]() -> void {
    // Indicate that we are ready for the test to begin.
    state.store(State::WaitingForLock);

    // Now attempt to obtain the lock.  This will _eventually_ succeed, but
    // not before the main test thread drops the lock.
    ChainLock::LockResult result = lock.Acquire(token);
    EXPECT_EQ(ChainLock::LockResult::kOk, result);

    // Indicate that we have successfully acquired the lock, then wait until
    // it is time to drop the lock.
    state.store(State::LockAcquired);
    while (state.load() != State::DropLock) {
      std::this_thread::sleep_for(1ms);
    }

    // Test finished, drop the lock and get out.
    lock.AssertHeld(token);
    lock.Release();
  }};

  // Wait until the test thread is about to acquire the lock, then wait just a
  // little longer to ensure that the test thread is actually spinning inside of
  // the lock.  Note that there is a very small chance that we don't actually
  // encounter contention with the test thread, and end up giving a false
  // positive, however it a very small chance.  Given the number of CI/CQ runs
  // we execute, and error here will not stay undetected for long.
  while (state.load() != State::WaitingForLock) {
    std::this_thread::sleep_for(1ms);
  }
  std::this_thread::sleep_for(100ms);

  // Go ahead and drop the lock, then wait until the thread has entered.
  lock.AssertHeld(low_prio_token);
  lock.Release();
  while (state.load() != State::LockAcquired) {
    std::this_thread::sleep_for(1ms);
  }

  // Now, if we attempt to acquire the lock with our lower-priority token, we
  // should be told that we need to back off.
  result = lock.Acquire(low_prio_token);
  EXPECT_EQ(ChainLock::LockResult::kBackoff, result);

  // Signal the thread that it is time to drop the lock and exit, then cleanup
  // and get out.
  state.store(State::DropLock);
  t1.join();
}

}  // namespace test
