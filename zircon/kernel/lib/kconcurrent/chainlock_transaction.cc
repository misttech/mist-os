// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/kconcurrent/chainlock_transaction.h>

#include <kernel/thread.h>

namespace kconcurrent {

void ChainLockTransaction::PreemptDisable() {
  Thread::Current::preemption_state().PreemptDisable();
}
void ChainLockTransaction::PreemptReenable() {
  Thread::Current::preemption_state().PreemptReenable();
}

void ChainLockTransaction::EagerReschedDisable() {
  Thread::Current::preemption_state().EagerReschedDisable();
}
void ChainLockTransaction::EagerReschedReenable() {
  Thread::Current::preemption_state().EagerReschedReenable();
}

RescheduleContext SchedulerUtils::PrepareForReschedule() TA_REQ(chainlock_transaction_token) {
  DEBUG_ASSERT(arch_ints_disabled());
  Thread* const current_thread = Thread::Current::Get();
  current_thread->get_lock().AssertHeld();

  ChainLockTransaction* const orig_clt = ChainLockTransaction::Active();
  orig_clt->debug_.AssertNumLocksHeld(1);

  const ChainLock::Token orig_token{orig_clt->active_token_};
  const ChainLock::Token sched_token{ChainLock::CreateSchedToken()};

  ChainLock::AssertTokenIsNotSchedToken(orig_token);

  ChainLock::ReplaceToken(orig_clt->active_token_, sched_token);
  current_thread->get_lock().ReplaceLockToken(sched_token);

  return RescheduleContext{orig_clt, orig_token};
}

void SchedulerUtils::RestoreRescheduleContext(const RescheduleContext& ctx) {
  // Called after a reschedule operation where the current thread kept running
  // (no context switch).  Things which should be true right now.
  //
  // 1) There should be an active transaction, and the token being used by the
  //    active transaction should be a special "scheduler token"
  // 2) There should be exactly one lock held.
  // 3) The one lock held should be current thread's lock.
  // 4) The active transaction should not have changed.
  // 5) The token we are restoring has to be a "non-scheduler" token.
  //
  ChainLockTransaction* const active_clt = ChainLockTransaction::Active();
  Thread* const current_thread = Thread::Current::Get();

  ChainLock::AssertTokenIsSchedToken(active_clt->active_token_);  // #1
  active_clt->debug_.AssertNumLocksHeld(1);                       // #2
  current_thread->get_lock().AssertHeld();                        // #3
  DEBUG_ASSERT(active_clt == ctx.orig_transaction);               // #4
  ChainLock::AssertTokenIsNotSchedToken(ctx.orig_token);          // #5

  // Go ahead and restore transaction, as well as the token in both the
  // transaction and the current thread's lock.
  ChainLock::ReplaceToken(active_clt->active_token_, ctx.orig_token);
  current_thread->get_lock().ReplaceLockToken(ctx.orig_token);
}

void SchedulerUtils::PostContextSwitchLockHandoff(const RescheduleContext& ctx,
                                                  Thread* previous_thread) {
  // Called after a reschedule operation where a new thread was selected to run.
  // Things which should be true right now.
  //
  // 1) There should be an active transaction, and the token being used by the
  //    active transaction should be a special "scheduler token"
  // 2) There should be exactly two locks held.
  // 3) One of the locks held should be current thread's lock.
  // 4) The other lock held should be the previous_thread's lock (should be
  //    guaranteed by the TA_REL static annotation on this method.
  // 5) The active transaction should have changed to the new current thread's
  //    active transaction.
  // 6) The token we are restoring has to be a "non-scheduler" token.
  //
  [[maybe_unused]] ChainLockTransaction* const active_clt = ChainLockTransaction::Active();
  Thread* const current_thread = Thread::Current::Get();

  ChainLock::AssertTokenIsSchedToken(active_clt->active_token_);  // #1
  active_clt->debug_.AssertNumLocksHeld(2);                       // #2
  current_thread->get_lock().AssertHeld();                        // #3
  DEBUG_ASSERT((active_clt != ctx.orig_transaction) &&            // #5
               (ctx.orig_transaction != nullptr));                // #5
  ChainLock::AssertTokenIsNotSchedToken(ctx.orig_token);          // #6

  // Restore the original lock token.
  ChainLock::ReplaceToken(ctx.orig_transaction->active_token_, ctx.orig_token);
  current_thread->get_lock().ReplaceLockToken(ctx.orig_token);
  previous_thread->get_lock().ReplaceLockToken(ctx.orig_token);

  // Swap out the active transaction and drop the previous thread's lock.
  arch_get_curr_percpu()->active_cl_transaction = ctx.orig_transaction;
  previous_thread->get_lock().Release();
}

void SchedulerUtils::TrampolineLockHandoff() {
  ChainLockTransaction::AssertActive();

  // Assert that the current thread's lock was "acquired" here.  It was
  // actually acquired during the context switch from
  // Scheduler::RescheduleCommon, but we now need to release it before letting
  // the new thread run.  The currently registered CLT should be using the
  // scheduler token that was used to acquire the new thread's lock during
  // reschedule common.
  Thread* const current_thread = Thread::Current::Get();
  current_thread->get_lock().AssertAcquired();

  // Construct the fake chainlock transaction we will be restoring.  It should:
  //
  // 1) Not attempt to register itself with the per-cpu data structure.  There
  //    should already be a registered CLT, and attempting to instantiate a new
  //    one using the typical constructor would trigger an ASSERT.  This should
  //    look like a transaction which had been sitting on a stack, but swapped
  //    out using PrepareForReschedule at some point in the past.
  // 2) Report that there are currently two locks held (because there are).
  //    We have the previously running thread's lock, and the current (new)
  //    thread's lock held (obtained by Scheduler::RescheduleCommon).
  // 3) Report that the transaction has been finalized, as it would have been
  //    had we come in via RescheduleCommon.
  //
  ChainLockTransactionNoIrqSave clt{internal::TrampolineTransactionTag{}};

  // Now perform our lock handoff.  This will release our previous thread's
  // lock, and restore the transaction we just instantiated as the active
  // transaction, and replace the current thread's lock's token with the new
  // non-scheduler token.
  Scheduler::LockHandoffInternal(RescheduleContext{&clt, clt.active_token_}, current_thread);

  // Finally, we just need to release the current thread's lock and we are
  // finished.
  current_thread->get_lock().Release();
}

}  // namespace kconcurrent
