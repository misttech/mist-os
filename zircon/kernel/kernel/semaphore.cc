// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "kernel/semaphore.h"

#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <kernel/thread_lock.h>

// Every call to this method must wake one waiter unless the queue is empty.
//
// When leaving this method, we must have either
//   - incremented a non-negative count or
//   - unblocked one thread and
//     - left an empty queue with a count of zero or
//     - left a non-empty queue with negative count
void Semaphore::Post() {
  // Is the count greater than or equal to zero?  If so, increment and return.
  // Take care to not increment a negative count.
  int64_t old_count = count_.load(ktl::memory_order_relaxed);
  while (old_count >= 0) {
    if (count_.compare_exchange_weak(old_count, old_count + 1, ktl::memory_order_release,
                                     ktl::memory_order_relaxed)) {
      return;
    }
  }

  // We observed a negative count and have not yet incremented it.  There might
  // be waiters waiting.  We'll need to acquire the lock and check.
  Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};

  // Because we hold the lock we know that no other thread can transition the
  // count from non-negative to negative or vice versa.  If the count is
  // non-negative, then there must be no watiers so just increment and return.
  old_count = count_.load(ktl::memory_order_relaxed);
  if (old_count >= 0) {
    [[maybe_unused]] uint32_t q;
    DEBUG_ASSERT_MSG((q = waitq_.Count()) == 0, "q=%u old_count=%ld\n", q, old_count);
    count_.fetch_add(1, ktl::memory_order_release);
    return;
  }

  // Because we hold the lock we know the number of waiters cannot change out
  // from under us.  At this point we know the count is negative.  Check for
  // waiters to wake.
  const uint32_t num_in_queue = waitq_.Count();
  if (num_in_queue == 0) {
    // There are no waiters to wake.  They must have timed out or been
    // interrupted.  Reset the count and perform our increment.
    count_.store(1, ktl::memory_order_release);
    return;
  }

  // At this point we know there's at least one waiter waiting.  We're committed
  // to waking.  However, we need to determine what the count should be when
  // drop the lock and return.  After we wake, if there are no waiters left, the
  // count should be reset to zero, otherwise it should remain negative.
  //
  // Because waking via |WakeOne| may reschedule the local CPU and allow another
  // thread to "interrupt" our critical section, it's crucial that we adjust the
  // count *before* calling |WakeOne|.
  if (num_in_queue == 1) {
    count_.store(0, ktl::memory_order_release);
  }
  [[maybe_unused]] const bool woke_one = waitq_.WakeOne(ZX_OK);
  DEBUG_ASSERT(woke_one);
}

// Handling failed waits -- Waits can fail due to timeout or thread signal.
// When a Wait fails, the caller cannot proceed to the resource guarded by the
// semaphore.  It's as if the waiter gave up and left.  We want to ensure failed
// waits do not impact other waiters.  While it seems like a failed wait should
// simply "undo" its decrement, it is not safe to do so in Wait.  Instead, we
// "fix" the count in subsequent call to Post.
//
// To understand why we can't simply fix the count in Wait after returning from
// Block, let's look at an alternative (buggy) implementation of Post and Wait.
// In this hypothetical implementation, Post increments and Wait decrements
// before Block, but also increments if Block returns an error.  With this
// hypothetical implementation in mind, consider the following sequence of
// operations:
//
//    Q  C
//    0  0
// 1W 1 -1  B
// 1T 0 -1
// 2P 0  0
// 3W 1 -1  B
// 1R 1  0
//
// The way to read the sequence above is that the wait queue (Q) starts empty
// and the count (C) starts at zero.  Thread1 calls Wait (1W) and blocks (B).
// Thread1's times out (1T) and is removed from the queue, but has not yet
// resumed execution.  Thread2 calls Post (2P), but does not unblock any threads
// because it finds an empty queue. Thread3 calls Wait (3W) and blocks (B).
// Thread1 returns from Block, resumes execution (1R), and increments the count.
// At this point Thread3 is blocked as it should be (two Posts and one failed
// Wait), however, a subsequent call to Post will not unblock it.  We have a
// "lost wakeup".
zx_status_t Semaphore::Wait(const Deadline& deadline) {
  // Is the count greater than zero?  If so, decrement and return.  Take care to
  // not decrement zero or a negative value.
  int64_t old_count = count_.load(ktl::memory_order_relaxed);
  while (old_count > 0) {
    if (count_.compare_exchange_weak(old_count, old_count - 1, ktl::memory_order_acquire,
                                     ktl::memory_order_relaxed)) {
      return ZX_OK;
    }
  }

  // We either observed that count is zero or negative.  We have not decremented
  // yet.  We may need to block.
  Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};

  // Because we hold the lock we know that no other thread can transition the
  // count from non-negative to negative or vice versa.  If we can decrement a
  // positive count, then we're done and don't need to block.
  old_count = count_.fetch_sub(1, ktl::memory_order_acquire);
  if (old_count > 0) {
    return ZX_OK;
  }

  // We either decremented zero or a negative value.  We must block.
  return waitq_.Block(deadline, Interruptible::Yes);
}
