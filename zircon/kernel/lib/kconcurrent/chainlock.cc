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

  const LockResult result = Base::AcquireInternalSingleAttempt(clt.active_token());
  if (result == LockResult::kOk) {
    clt.OnAcquire();
  } else {
    clt.RecordConflictStart();
  }

  return result;
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

bool ChainLock::TryAcquireInternal() {
  ChainLockTransaction& clt = ChainLockTransaction::ActiveRef();
  clt.AssertNotFinalized();

  if constexpr (kCltTraceAccountingEnabled) {
    if (const LockResult result = AcquireInternalSingleAttempt(clt); result == LockResult::kOk) {
      return true;
    } else {
      DEBUG_ASSERT_MSG(result != LockResult::kCycleDetected, "Cycle detected during TryAcquire");
    }
  }

  if (Base::TryAcquire(clt.active_token()) == true) {
    clt.OnAcquire();
    return true;
  }

  return false;
}

void ChainLock::ReleaseInternal() {
  ChainLockTransaction& clt = ChainLockTransaction::ActiveRef();

  DEBUG_ASSERT_MSG(clt.active_token().value() == Base::get_token().value(),
                   "Token mismatch during release (transaction token 0x%lx, lock token 0x%lx)",
                   clt.active_token().value(), Base::get_token().value());
  clt.OnRelease();

  return Base::Release();
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

  const LockResult result = Base::Acquire(clt.active_token());
  if (result == LockResult::kOk) {
    clt.OnAcquire();
  }
  return result;
}

void ChainLock::AssertHeld() const {
  return Base::AssertHeld(ChainLockTransaction::ActiveRef().active_token());
}

bool ChainLock::is_held() const {
  return Base::is_held(ChainLockTransaction::ActiveRef().active_token());
}

}  // namespace kconcurrent
