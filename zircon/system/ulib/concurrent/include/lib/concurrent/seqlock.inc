// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#pragma once

#include <lib/concurrent/seqlock.h>

namespace concurrent {
namespace internal {

template <typename Osal>
typename SeqLock<Osal>::ReadTransactionToken SeqLock<Osal>::BeginReadTransaction() {
  SequenceNumber seq_num;

  while (((seq_num = seq_num_.load(std::memory_order_acquire)) & 0x1) != 0) {
    Osal::ArchYield();
  }

  return ReadTransactionToken{seq_num};
}

template <typename Osal>
bool SeqLock<Osal>::TryBeginReadTransaction(ReadTransactionToken& out_token,
                                            zx_duration_t timeout) {
  return TryBeginReadTransactionDeadline(out_token, Osal::GetClockMonotonic() + timeout);
}

template <typename Osal>
bool SeqLock<Osal>::TryBeginReadTransactionDeadline(ReadTransactionToken& out_token,
                                                    zx_time_t deadline) {
  while (((out_token.seq_num_ = seq_num_.load(std::memory_order_acquire)) & 0x1) != 0) {
    if (Osal::GetClockMonotonic() >= deadline) {
      return false;
    }
    Osal::ArchYield();
  }

  return true;
}

template <typename Osal>
bool SeqLock<Osal>::EndReadTransaction(ReadTransactionToken token) {
  if ((token.seq_num_ & 0x1) != 0) {
    return false;
  }

  return seq_num_.load(std::memory_order_relaxed) == token.seq_num_;
}

template <typename Osal>
void SeqLock<Osal>::Acquire() {
  while (true) {
    SequenceNumber expected;

    // Wait until we observe an even sequence number.
    while (((expected = seq_num_.load(std::memory_order_relaxed)) & 0x1) != 0) {
      Osal::ArchYield();
    }

    // Attempt to increment the even number we observed to be an odd number,
    // with Acquire semantics on the RMW if we succeed.
    if (seq_num_.compare_exchange_strong(expected, expected + 1,
                                         std::memory_order_acquire,
                                         std::memory_order_relaxed)) {
      break;
    }
  }
}

template <typename Osal>
bool SeqLock<Osal>::TryAcquire(zx_duration_t timeout) {
  return TryAcquireDeadline(Osal::GetClockMonotonic() + timeout);
}

template <typename Osal>
bool SeqLock<Osal>::TryAcquireDeadline(zx_time_t deadline) {
  while (true) {
    SequenceNumber expected;

    // Wait until we observe an even sequence number.  Bail out if we exceed our
    // deadline.
    while (((expected = seq_num_.load(std::memory_order_relaxed)) & 0x1) != 0) {
      if (Osal::GetClockMonotonic() >= deadline) {
        return false;
      }
      Osal::ArchYield();
    }

    // Attempt to increment the even number we observed to be an odd number,
    // with Acquire semantics on the RMW if we succeed.
    if (seq_num_.compare_exchange_strong(expected, expected + 1,
                                         std::memory_order_acquire,
                                         std::memory_order_relaxed)) {
      return true;
    }

    // Make sure we check our deadline again.  We don't want to be in a
    // situation where the relaxed load (at the start of the while) always sees
    // an even number, but the subsequent CMPX always fails, causing us to never
    // check our deadline.
    if (Osal::GetClockMonotonic() >= deadline) {
      return false;
    }
  }
}

template <typename Osal>
void SeqLock<Osal>::Release() {
  [[maybe_unused]] SequenceNumber before = seq_num_.fetch_add(1, std::memory_order_release);
  ZX_DEBUG_ASSERT((before & 0x1) != 0);
}

}  // namespace internal
}  // namespace concurrent
