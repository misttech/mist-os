// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2008-2014 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

/**
 * @file
 * @brief  Event wait and signal functions for threads.
 * @defgroup event Events
 *
 * An event is a subclass of a wait queue.
 *
 * Threads wait for events, with optional timeouts.
 *
 * Events are "signaled", releasing waiting threads to continue.
 * Signals may be one-shot signals (Event::AUTOUNSIGNAL), in which
 * case one signal releases only one thread, at which point it is
 * automatically cleared. Otherwise, signals release all waiting threads
 * to continue immediately until the signal is manually cleared with
 * Event::Unsignal().
 *
 * @{
 */

#include "kernel/event.h"

#include <assert.h>
#include <debug.h>
#include <lib/fit/defer.h>
#include <lib/kconcurrent/chainlock.h>
#include <lib/kconcurrent/chainlock_transaction.h>
#include <lib/zircon-internal/macros.h>
#include <sys/types.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <kernel/auto_preempt_disabler.h>
#include <kernel/scheduler.h>
#include <kernel/spinlock.h>
#include <kernel/thread.h>

/**
 * @brief  Destruct an Event object.
 *
 * Event's resources are freed and it may no longer be used.
 * Will panic if there are any threads still waiting.
 */
Event::~Event() {
  DEBUG_ASSERT(magic_ == kMagic);

  magic_ = 0;
  result_.store(kNotSignaled, ktl::memory_order_relaxed);
  flags_ = Flags(0);
}

zx_status_t Event::WaitWorker(const Deadline& deadline, Interruptible interruptible,
                              uint signal_mask) {
  DEBUG_ASSERT(magic_ == kMagic);
  DEBUG_ASSERT(!arch_blocking_disallowed());

  // Start by grabbing our wait queue's lock.  The state of the event is only
  // allowed to change from un-signaled to signaled when we are holding this
  // lock, so by holding it here, we can check the state of the signal and fast
  // abort if we need to, or descend into the wait queue and be certain to fully
  // block in the queue before releasing the lock.
  const auto do_transaction =
      [&]() TA_REQ(chainlock_transaction_token) -> ChainLockTransaction::Result<zx_status_t> {
    wait_.get_lock().AcquireFirstInChain();

    zx_status_t result = result_.load(ktl::memory_order_relaxed);
    if (result == kNotSignaled) {
      // Looks like we are not currently signaled.  Now try to obtain the
      // current thread's lock so we can block it.
      Thread* current_thread = Thread::Current::Get();
      if (!current_thread->get_lock().AcquireOrBackoff()) {
        wait_.get_lock().Release();
        return ChainLockTransaction::Action::Backoff;
      }

      ChainLockTransaction::Finalize();

      // We got the lock, go ahead and block the thread.  This will
      // automatically release the queue's lock after the thread has been added
      // to the queue and is committed to blocking.  We will need release the
      // thread's lock ourselves after it wakes up, as it will be obtained as it
      // becomes scheduled.
      result = wait_.BlockEtc(current_thread, deadline, signal_mask, ResourceOwnership::Normal,
                              interruptible);
      current_thread->get_lock().Release();
      return result;
    }

    /* signaled, we're going to fall through */
    if (flags_ & Event::AUTOUNSIGNAL) {
      /* autounsignal flag lets one thread fall through before unsignaling */
      result_.store(kNotSignaled, ktl::memory_order_relaxed);
    }

    wait_.get_lock().Release();
    return result;
  };

  return ChainLockTransaction::UntilDone(IrqSaveOption, CLT_TAG("Event::WaitWorker"),
                                         do_transaction);
}

/**
 * @brief  Signal an event
 *
 * Signals an event.  If Event::AUTOUNSIGNAL is set in the event
 * object's flags, only one waiting thread is allowed to proceed.  Otherwise,
 * all waiting threads are allowed to proceed until such time as
 * Event::Unsignal() is called.
 *
 * @param e           Event object
 * @param wait_result What status a wait call will return to the
 *                    thread or threads that are woken up.
 */
void Event::Signal(zx_status_t wait_result) {
  DEBUG_ASSERT(magic_ == kMagic);
  DEBUG_ASSERT(wait_result != kNotSignaled);

  // In order to transition from not-signaled to signaled, we must be
  // holding our wait queue's lock.
  const auto do_transaction =
      [&]() TA_REQ(chainlock_transaction_token) -> ChainLockTransaction::Result<> {
    ChainLockGuard guard{wait_.get_lock()};

    // If we are already signaled, we are finished.  We should be able to assert
    // that there are no waiters right now.
    if (result_.load(ktl::memory_order_relaxed) != kNotSignaled) {
      DEBUG_ASSERT(wait_.Count() == 0);
      return ChainLockTransaction::Done;
    }

    // If there are no threads waiting in the event, we can just mark it
    // signaled and get out.
    if (wait_.Count() == 0) {
      result_.store(wait_result, ktl::memory_order_relaxed);
      return ChainLockTransaction::Done;
    }

    // Try to lock with one or all of the threads for wake.
    ktl::optional<Thread::UnblockList> maybe_unblock_list =
        (flags_ & Event::AUTOUNSIGNAL) ? WaitQueueLockOps::LockForWakeOne(wait_, wait_result)
                                       : WaitQueueLockOps::LockForWakeAll(wait_, wait_result);

    // If we failed to lock, we need to drop the queue lock, then try again.
    if (!maybe_unblock_list.has_value()) {
      return ChainLockTransaction::Action::Backoff;
    }

    // We have all of our locks now, time to proceed with the wake operations (if any)
    ChainLockTransaction::Finalize();

    // Success.  If we not an auto-reset event, or we failed to find anyone to
    // wake, make sure to set the event to the signaled state.
    const bool has_threads_to_wake = !maybe_unblock_list.value().is_empty();
    if (!(flags_ & Event::AUTOUNSIGNAL) || !has_threads_to_wake) {
      result_.store(wait_result, ktl::memory_order_relaxed);
    }

    // We've finished our bookkeeping.  Go ahead and drop the queue lock, then
    // unblock all of the threads.
    guard.Release();
    if (has_threads_to_wake) {
      Scheduler::Unblock(ktl::move(maybe_unblock_list).value());
    }
    return ChainLockTransaction::Done;
  };
  ChainLockTransaction::UntilDone(IrqSaveOption, CLT_TAG("Event::Signal"), do_transaction);
}

/**
 * @brief  Clear the "signaled" property of an event
 *
 * Used mainly for event objects without the Event::AUTOUNSIGNAL
 * flag.  Once this function is called, threads that call Event::Wait()
 * functions will once again need to wait until the event object
 * is signaled.
 *
 * @param e  Event object
 *
 * @return  Returns ZX_OK on success.
 */
zx_status_t Event::Unsignal() {
  DEBUG_ASSERT(magic_ == kMagic);
  result_.store(kNotSignaled, ktl::memory_order_relaxed);
  return ZX_OK;
}
