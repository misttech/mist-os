// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/kconcurrent/chainlock.h>
#include <lib/kconcurrent/chainlock_transaction.h>

namespace kconcurrent {

// Notes on the use of templates in the AcquireInternal and AcquireUnconditionallyInternal methods:
//
// Both of these methods (in addition to TryAcquireInternal) make use of
// AcquireInternalSingleAttempt, but only in the case that Trace Accounting is
// enabled.  We never want anyone to be calling this method unless the compile
// time feature is enabled, and we want to give anyone who accidentally calls
// this method when the feature is disabled a clean error message at compile
// time.  IOW - We'd rather they didn't have to discover the mistake until
// runtime, and we also don't want their error message to be a bunch of
// incomprehensible C++ template expansion errors.
//
// So, we have a static_assert in AcquireInternalSingleAttempt, and the method
// is templated (even though we make no use of the template argument), meaning
// that someone has to actually try to expand the templated method in order for
// the static_assert to be evaluated (and potentially fail).
//
// Everywhere we make use of the function, we always use the following pattern:
//
// ```
// void Func() {
//   // stuff
//   if constexpr (kCltTraceAccountingEnabled) {
//     AcquireInternalSingleAttempt(clt);
//   }
//   // stuff
// }
// ```
//
// If accounting is disabled, then the predicate of the constexpr is false, and
// the body is checked for basic stuff like syntax, but none of the types inside
// are evaluated or expanded.
//
// Unfortunately, this is not 100% true.  This behavior of `if constexpr` only
// holds when the statement is evaluated in a "templated entity", such as a
// templated class, or a templated function/method.  If there is no template,
// then both halves of the `if constexpr` are fully checked. So, `Func` from the
// example above is not templated, which means that the body of the `if` is
// evaluated even with Trace Accounting disabled, triggering the static assert.
//
// To work around this unfortunate behavior, we have actually made
// AcquireInternal and AcquireUnconditionallyInternal templated entities
// (TryAcquireInternal was already) in order to prevent the compiler from fully
// checking the false half of any `if constexprs` inside of them.  A few points
// on this:
// 1) The templates are all on private methods; none of this hot garbage is
//    exposed to users.
// 2) The template arguments are actually never used.  They are just there to
//    make `if constexpr` behave the way we want.
// 3) The templated typename is always void, and only the void version are
//    expanded at compile time.  No one should ever try to do anything but call
//    the default version of the method (which specifies void), but if they do,
//    they should fail to link because of the limited explicit expansions.
//
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

template <typename>
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

  // Take care with how we manage the release operation.  We need to set the
  // internal state of the lock to "unlocked", but we also need to observe the
  // contention mask to know whether or not there are CPUs waiting for the
  // signal that it is OK to try to restart their transaction.
  //
  // If we observe the contention mask before we set the internal state to
  // Unlocked, we set up a race where we might fail to signal a CPU.  The
  // sequence would be:
  //
  // 1) T1 loads the contention mask into a local variable; it is currently 0 (no contention)
  // 2) T2 Observes the lock state and sees that it needs to back off.
  // 3) T2 Records its CPU's conflict ID.
  // 4) T2 Records its CPU ID in the lock's contention mask.
  // 5) T2 backs off and waits for its CPU's conflict ID to change.
  // 6) T1 sets the lock state to unlocked.
  // 7) T1's observed contention mask is 0, so it is finished.  T2 is now stuck.
  //
  // Doing things the other way sets up a different problem.  Say that the lock
  // being release is the lock for a Thread object.  During release, we
  //
  // 1) Set the state of the lock to unlocked.
  // 2) Observe the contention mask.
  //
  // As soon as we have set the state of the lock to unlocked, the Thread it is
  // protecting is free to exit.  If it exits and destroys its memory before
  // step #2, we will have a UAF situation.
  //
  // On of the properties we would really like to preserve for these locks is,
  // "If the lock is unlocked, then there is CPU which is going to touch any of
  // the lock's internal storage for any reason except to acquire the lock".
  // Obeying this rule eliminates the UAF potential described above (as well as
  // many other potential variations).
  //
  // So, to ensure this, we go through a 2-step release process.  We own the
  // lock right now with some token value T.  Anyone who attempts to obtain the
  // lock with a token (A), where A > T is going to attempt to record a conflict
  // an back off.  So, we replace the lock's state with the maximum possible
  // value for a token.  This is not a token which is ever going to be generated
  // or used (see the 64-bit-integers are Very Large argument for why), and we
  // know that it is greater than all other possible lock token values.  Any
  // attempt to obtain the lock when the token value is Max is going to simply
  // keep trying; they will not back off.
  //
  // Once we have replaced the token value with Max, we can observe the
  // contention mask.  If another thread observed the original token value and
  // is trying to record a conflict, they will either finish their update before
  // we swapped the token for Max (in which case we will see the contention
  // bit), or they won't, in which case they will see that the lock's state has
  // changed and will cancel their backoff.
  //
  // Finally, once we have observed the contention mask, we can update the
  // lock's state to unlocked, poke any CPUs who are in the contention mask, and
  // we are finished.
  //
  // TODO(johngro): Look into relaxing the memory ordering being used here.  I
  // _think_ that we can perform this store to the lock's state using relaxed
  // semantics, but I'm not going to take that chance without having first built
  // a model and run it through CDSChecker.
  //
  this->state_.store(kMaxToken, ktl::memory_order_release);
  cpu_mask_t wake_mask = contention_mask_.exchange(0, ktl::memory_order_acq_rel);
  Base::Release();
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

template <typename>
ChainLock::LockResult ChainLock::AcquireInternal() {
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
  return LockResult::kOk;
}

void ChainLock::AssertHeld() const {
  return Base::AssertHeld(ChainLockTransaction::ActiveRef().active_token());
}

bool ChainLock::is_held() const {
  return Base::is_held(ChainLockTransaction::ActiveRef().active_token());
}

// Force expansion.  These are the only versions of this template which will
// ever be allowed.
template ChainLock::LockResult ChainLock::AcquireInternal<void>();
template void ChainLock::AcquireUnconditionallyInternal<void>();

}  // namespace kconcurrent
