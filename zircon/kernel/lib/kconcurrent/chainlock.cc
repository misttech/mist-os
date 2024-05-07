// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/kconcurrent/chainlock.h>
#include <lib/kconcurrent/chainlock_transaction.h>

namespace kconcurrent {

template <typename>
inline ChainLock::LockResult ChainLock::AcquireInternalSingleAttempt(ChainLockTransaction& clt) {
  static_assert(kCltTraceAccountingEnabled,
                "Calls to AcquireInternalSingleAttempt must only be made when ChainLock trace "
                "accounting is enabled.");

  if (clt.has_active_conflict()) {
    return LockResult::kRetry;
  }

  const Base::AcquireInternalResult result = Base::AcquireInternalSingleAttempt(clt.active_token());
  if (result.result == LockResult::kOk) {
    clt.OnAcquire();
  } else {
    clt.RecordConflictStart();

    // If we have been told to back off, and we fail to record the backoff
    // conflict in the lock, it is because the lock's token must have changed,
    // just now.  Change our error from "backoff" to "retry" so we end up trying
    // again instead of completely unwinding.
    if ((result.result == LockResult::kBackoff) && !RecordBackoff(result.observed_token)) {
      return LockResult::kRetry;
    }
  }

  return result.result;
}

void ChainLock::AcquireUnconditionallyInternal() {
  ChainLockTransaction& clt = ChainLockTransaction::ActiveRef();
  clt.AssertNotFinalized();

  if constexpr (kCltTraceAccountingEnabled) {
    if (AcquireInternalSingleAttempt(clt) == LockResult::kOk) {
      Base::MarkHeld();
      return;
    }
  }

  Base::AcquireUnconditionally(clt.active_token());
  clt.OnAcquire();
}

template <ChainLock::FinalizedTransactionAllowed FTAllowed>
bool ChainLock::TryAcquireInternal() {
  ChainLockTransaction& clt = ChainLockTransaction::ActiveRef();

  if constexpr (FTAllowed != FinalizedTransactionAllowed::Yes) {
    clt.AssertNotFinalized();
  }

  if constexpr (kCltTraceAccountingEnabled) {
    if (const LockResult result = AcquireInternalSingleAttempt(clt); result == LockResult::kOk) {
      return true;
    } else {
      DEBUG_ASSERT_MSG(result != LockResult::kCycleDetected, "Cycle detected during TryAcquire");
    }
  }

  if (const Base::AcquireInternalResult result =
          Base::AcquireInternalSingleAttempt(clt.active_token());
      result.result == LockResult::kOk) {
    clt.OnAcquire();
    return true;
  } else {
    DEBUG_ASSERT_MSG(result.result != LockResult::kCycleDetected,
                     "Cycle detected during TryAcquire");
    if (result.result == LockResult::kBackoff) {
      RecordBackoff(result.observed_token);
    }
    return false;
  }
}

template bool ChainLock::TryAcquireInternal<ChainLock::FinalizedTransactionAllowed::Yes>();
template bool ChainLock::TryAcquireInternal<ChainLock::FinalizedTransactionAllowed::No>();

void ChainLock::ReleaseInternal() {
  ChainLockTransaction& clt = ChainLockTransaction::ActiveRef();

  DEBUG_ASSERT_MSG(clt.active_token().value() == Base::get_token().value(),
                   "Token mismatch during release (transaction token 0x%lx, lock token 0x%lx)",
                   clt.active_token().value(), Base::get_token().value());
  clt.OnRelease();

  // Release the lock itself, _then_ observer and clear the contention mask.  This sets up a race
  // where:
  // 1) Given a previous token of X
  // 2) After the release, a new transaction might claim the lock with a token of Y
  // 3) A different transaction with a token of Z (Z > Y) might mark the lock as being contended
  // 4) We might observe and clear this contention, even though it is for a different transaction
  //    than ours (who just released the lock).
  //
  // The result in this situation would be a spurious wakeup of the transaction
  // with a token of Z. While non-ideal, this should be rare, and is much better
  // than accidentally missing a wakeup.
  Base::Release();
  cpu_mask_t wake_mask = contention_mask_.exchange(0, ktl::memory_order_acq_rel);
  for (cpu_num_t cpu = 0; wake_mask; ++cpu, wake_mask >>= 1) {
    if (wake_mask & 0x1) {
      percpu::Get(cpu).chain_lock_conflict_id.fetch_add(1, ktl::memory_order_release);
    }
  }
}

bool ChainLock::RecordBackoff(Token observed_token) {
  // We have observed that someone else is holding this lock and that we have
  // lower priority, so we need to back off.  Record this in the state of the
  // lock.  This allows the thread currently holding the lock to signal us once
  // the lock has been dropped so we can try again.
  //
  // The mechanism works as follows.
  //
  // Each chain lock has a "contention mask".  When the contention mask is
  // non-zero at the time the lock is released, it means that one or more
  // threads wanted this lock at some point in the past, but was forced to back
  // off, and may be waiting in a ChainLockTransaction::Relax operation.
  //
  // To record a backoff, a thread will:
  // 1) Observe its current CPU's conflict_id.
  // 2) Set the contention mask bit corresponding to `current_cpu_id % NUM_BITS(mask)`
  // 3) If the lock has not changed state, record the conflict ID it observed
  //    in (1) in the state of currently active chain lock transaction.
  // 4) Back off to the Relax point, and wait until the current CPU's conflict
  //    ID has changed before proceeding to try again.
  //
  // To release a lock, a thread will:
  // 1) Set the state of the lock to UNLOCKED
  // 2) Exchange 0 with the contention mask.
  // 3) For each bit, B, which was set in the contention mask, increment the
  //    conflict_id of each CPU for which ((cpu.id % NUM_BITS(mask)) == B).
  //
  // So, when a thread (A) successfully records that it needed to back off in a
  // lock, it knows that there is another thread (B) who will eventually release
  // the lock, and that B will end up changing A's CPU's conflict in a way where
  // the new conflict ID will never match the conflict ID which had been
  // initially observed by A.
  const cpu_num_t curr_cpu = arch_curr_cpu_num();
  struct percpu* pcpu = arch_get_curr_percpu();
  const cpu_mask_t contention_bit = (1u << curr_cpu);

  const uint64_t conflict_id = pcpu->chain_lock_conflict_id.load(ktl::memory_order_acquire);
  contention_mask_.fetch_or(contention_bit, ktl::memory_order_acq_rel);
  const Token lock_token = this->state_.load(ktl::memory_order_acquire);
  if (lock_token.value() == observed_token.value()) {
    pcpu->active_cl_transaction->conflict_id_ = conflict_id;
    return true;
  }
  return false;
}

void ChainLock::AssertAcquiredInternal() const {
  Base::AssertAcquired(ChainLockTransaction::ActiveRef().active_token());
}

bool ChainLock::MarkNeedsReleaseIfHeldInternal() const {
  return Base::MarkNeedsReleaseIfHeld(ChainLockTransaction::ActiveRef().active_token());
}

ChainLock::LockResult ChainLock::Acquire() {
  ChainLockTransaction& clt = ChainLockTransaction::ActiveRef();
  clt.AssertNotFinalized();

  if constexpr (kCltTraceAccountingEnabled) {
    if (const LockResult result = AcquireInternalSingleAttempt(clt); result != LockResult::kRetry) {
      return result;
    }
  }

  while (true) {
    const Base::AcquireInternalResult result = Base::AcquireInternal(clt.active_token());
    if (result.result == LockResult::kOk) {
      clt.OnAcquire();
    } else if ((result.result == LockResult::kBackoff) && !RecordBackoff(result.observed_token)) {
      continue;
    }

    return result.result;
  }
}

void ChainLock::AssertHeld() const {
  return Base::AssertHeld(ChainLockTransaction::ActiveRef().active_token());
}

bool ChainLock::is_held() const {
  return Base::is_held(ChainLockTransaction::ActiveRef().active_token());
}

}  // namespace kconcurrent
