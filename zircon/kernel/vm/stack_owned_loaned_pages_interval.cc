// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/kconcurrent/chainlock_transaction.h>
#include <lib/zircon-internal/macros.h>

#include <kernel/thread.h>
#include <ktl/limits.h>
#include <object/thread_dispatcher.h>
#include <vm/page.h>
#include <vm/stack_owned_loaned_pages_interval.h>

#include <ktl/enforce.h>

void StackOwnedLoanedPagesInterval::PrepareForWaiter() {
  canary_.Assert();
  // For now we don't need a CAS loop in here because SOLPI lock is held by
  // callers of PrepareForWaiter() and PrepareForWaiter() is the only mutator of
  // is_ready_for_waiter_.  Even if we did have a CAS loop, the caller would
  // still need to guarantee somehow that the interval won't get deleted out
  // from under this call.  Currently that's guaranteed by the current SOLPI
  // lock hold interval being the same interval that set
  // kObjectOrStackOwnerHasWaiter.
  //
  // Because all setters of is_ready_for_waiter_ hold the SOLPI lock, we could
  // use memory_order_relaxed here, but for now we're using acquire for all
  // loads of is_ready_for_waiter_.
  //
  // TODO(johngro):  Not only does this not need to be acquire/release, I'm
  // reasonably sure it does not need to be atomic in order to avoid a formal
  // data race.  The variable should only be written (at most) once, below here,
  // while holding the SOLPI lock.
  //
  // By the time a SOLPI instance's destructor loads this variable, it must have
  // cleared out the ownership bookkeeping of every page it had owned, meaning
  // that no new waiters can join the party.  If at least one page that there
  // was a waiter in this SOLPI instance, it must have set the
  // is_read_for_waiter flag inside the SOLPI lock, and the owner must have
  // syncronized-with that thread when clearing the page bookkeeping.  So, we
  // know that the destructor of the SOLPI instance (where the flag is read)
  // must happen-after the flag was set.  IOW - it should be impossible for any
  // thread to be performing a read of the flag concurrent with the only write
  // operation (below).
  if (is_ready_for_waiter_.load(ktl::memory_order_acquire)) {
    return;
  }

  // Thanks to the lock, we know that the current thread is the only thread setting
  // is_ready_for_waiter_, so we can just set it using a store().  We also need to prepare the
  // owned_wait_queue_ to have a waiter that can transmit its priority via priority inheritance to
  // the stack-owning thread.
  DEBUG_ASSERT(owning_thread_);
  DEBUG_ASSERT(Thread::Current::Get() != owning_thread_);
  owned_wait_queue_.emplace();

  // See above, we probably do not need release semantics here.
  is_ready_for_waiter_.store(true, ktl::memory_order_release);
}

// static
StackOwnedLoanedPagesInterval& StackOwnedLoanedPagesInterval::current() {
  Thread* current_thread = Thread::Current::Get();
  // The caller should only call current() when the caller knows there must be a current interval,
  // and just needs to know which interval is the outer-most on this thread's stack.
  //
  // Stack ownership of a loaned page requires having a StackOwnedLoanedPagesInterval on the
  // caller's stack.
  DEBUG_ASSERT_MSG(current_thread->stack_owned_loaned_pages_interval(),
                   "StackOwnedLoanedPagesInterval missing");
  return *current_thread->stack_owned_loaned_pages_interval_;
}

// static
StackOwnedLoanedPagesInterval* StackOwnedLoanedPagesInterval::maybe_current() {
  Thread* current_thread = Thread::Current::Get();
  return current_thread->stack_owned_loaned_pages_interval_;
}

// static
void StackOwnedLoanedPagesInterval::WaitUntilContiguousPageNotStackOwned(vm_page_t* page) {
  // Due to not holding the PmmNode lock, we can't check loaned directly, and it may have been unset
  // recently in any case, but in that case we'll notice via !is_stack_owned() instead.
  //
  // Need to take lock at this point, because avoiding deletion of the OwnedWaitQueue
  // requires holding the lock while applying kObjectOrStackOwnerHasWaiter to the page, to
  // prevent the StackOwnedLoanedPagesInterval thread from removing the stack_owner from the page
  // and deleting the OwnedWaitQueue.
  //
  // Before we acquire the lock we do a check whether a stack_owner is still set. This is
  // just to avoid acquiring the lock on the off chance that the stack ownership interval
  // is already over.  This isn't particularly likely to be the case, and we'd be fine without
  // this check.  But since we're about to take the lock, let's avoid an unnecessary acquire
  // if we can.
  if (!page->object.is_stack_owned()) {
    // StackOwnedLoanedPagesInterval is already removed from the page, so no need to
    // acquire the lock.  Go around and observe the new page state.
    return;
  }

  // Acquire StackOwnedLoanedPagesInterval lock, and attempt to flag this page's
  // interval-owner as having a waiter.  If we fail, it means that the page's
  // assigned StackOwnedLoanedPagesInterval was cleared, and we can skip
  // the wait operation.  If we succeed, however, then we know that the
  // stack-owner will need to obtain the SOLPI locks before it can clear the
  // owner of the page.
  AnnotatedAutoPreemptDisabler aapd;
  InterruptDisableGuard irqd;
  Guard<SpinLock, NoIrqSave> solpi_guard{&lock_};

  auto maybe_try_set_has_waiter_result = page->object.try_set_has_waiter();
  if (!maybe_try_set_has_waiter_result) {
    // stack_owner was cleared; no need to wait.
    return;
  }

  auto& try_set_has_waiter_result = maybe_try_set_has_waiter_result.value();
  auto& stack_owner = *try_set_has_waiter_result.stack_owner;
  // By doing PrepareForWaiter() only when necessary, we avoid pressure on the thread_lock in the
  // case where there's no page reclaiming thread needing to wait / transmit priority.
  if (try_set_has_waiter_result.first_setter) {
    stack_owner.PrepareForWaiter();
  }
  // PrepareForWaiter() was called previously, either by this thread or a different thread.
  DEBUG_ASSERT(stack_owner.is_ready_for_waiter_.load(ktl::memory_order_acquire));

  // At this point we know that the stack_owner won't be changed on the page while we hold
  // lock, which means the OwnedWaitQueue can't be deleted yet either, since deletion is
  // after uninstalling from the page.  So now we just need to block on the OwnedWaitQueue.
  //
  // Obtain all of the locks needed for a block-and-assign-owner operation, then
  // we can drop the SOLPI lock.  When the stack-owner of the pages comes along
  // later on and wants to clear the owner, they will:
  //
  // 1)  Notice that the has waiters bit has been set.
  // 2)  Obtain the SOLPI lock.
  // 3)  Clear the owner of the pages it owns, preventing any new waiters.  These
  //     pages are no longer owned.
  // 4)  Drop the SOLPI lock.  By acquiring and dropping the lock in order to
  //     clear the owner state, they are guaranteed of one of two things.
  // 4a) A waiter thread made it into the SOLPI lock, but after the stack-owner
  //     did.  By the time they made it into the lock, the discovered that the page
  //     they were interested no longer had an owner, and the unwound without
  //     waiting.
  // 4b) The waiter thread made it into the SOLPI lock first.  It marked the
  //     page's owner state as having waiters, and marked the SOLPI instance as
  //     having waiters as well.  It then traded the SOLPI lock for the locks
  //     required to block in the OWQ, and then blocked.
  // 5)  Later on, when the owner finally destroys their outer-most SOLPI
  //     instance, they will notice that the instance has waiters if any thread
  //     ever made it to step 4b.  They will then need to obtain the OWQ lock in
  //     order to wake all of the waiters.  Since the waiting thread obtained the
  //     OWQ lock and committed to waiting before dropping the SOLPI lock, it
  //     should be impossible for owner thread to attempt a wake operation before
  //     any thread who committed to blocking made it into the queue.

  Thread* const current_thread = Thread::Current::Get();
  ChainLockTransactionNoIrqSave clt{
      CLT_TAG("StackOwnedLoanedPagesInterval::WaitUntilContiguousPageNotStackOwned")};
  const OwnedWaitQueue::BAAOLockingDetails details =
      stack_owner.owned_wait_queue_->LockForBAAOOperation(current_thread,
                                                          stack_owner.owning_thread_);
  clt.Finalize();

  // Now that we have the page annotated to indicate that it has a waiter in
  // this StackOwnedLoanedPagesInterval, we know that the current stack-owner of
  // these pages is going to have to attempt to wake up all of the thread's
  // waiting in this SOLPI's OWQ.  We now hold that OWQ's lock as well, so we
  // can go ahead and drop the global SOLPI lock.  The current stack-owner is
  // going to have to grab that lock to wake us up, so it cannot stop us from
  // blocking in the queue at this point.  We can go ahead and drop the SOLPI
  // lock now.
  solpi_guard.Release();

  // If this is the first thread blocking in the OWQ, we expect the queue's
  // owner to be nullptr. For subsequent blocking threads, we expect it to match
  // our owning_thread_.
  DEBUG_ASSERT((stack_owner.owned_wait_queue_->owner() == nullptr) ||
               (stack_owner.owned_wait_queue_->owner() == stack_owner.owning_thread_));
  DEBUG_ASSERT(stack_owner.owned_wait_queue_->owner() != Thread::Current::Get());

  // This is a brief wait that's guaranteed not to get stuck (short of bugs elsewhere), with
  // priority inheritance propagated to the owning thread.  So no need for a deadline or
  // interruptible.
  //
  // Dropping into the block operation will release all of the locks we obtained
  // during LockForBAAOOperation involved in PI propagation, including this
  // queue's lock.  It will drop the current thread's lock as the thread becomes
  // de-scheduled (and a new thread is chosen), but will re-obtain the current
  // thread's lock later on once the thread has become re-scheduled, so we will
  // need to explicitly drop the current thread's lock on the way out after we
  // wake up.
  zx_status_t block_status = stack_owner.owned_wait_queue_->BlockAndAssignOwnerLocked(
      current_thread, Deadline::infinite(), details, ResourceOwnership::Normal, Interruptible::No);
  current_thread->get_lock().Release();

  // For this wait queue, no other status is possible since no other status is ever passed to
  // OwnedWaitQueue::WakeAll() for this wait queue and Block() doesn't have any other sources of
  // failures assuming no bugs here.
  DEBUG_ASSERT(block_status == ZX_OK);
}

void StackOwnedLoanedPagesInterval::WakeWaitersAndClearOwner(Thread* current_thread) {
  DEBUG_ASSERT(current_thread == Thread::Current::Get());

  [[maybe_unused]] const OwnedWaitQueue::WakeThreadsResult result = [&]() {
    AnnotatedAutoEagerReschedDisabler aaerd;
    return owned_wait_queue_->WakeThreads(ktl::numeric_limits<uint32_t>::max());
  }();

  // We should be able to assert that we woke at least one thread.
  DEBUG_ASSERT(result.woken > 0);
}
