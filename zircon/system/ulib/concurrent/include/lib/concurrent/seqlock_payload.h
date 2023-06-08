// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_CONCURRENT_SEQLOCK_PAYLOAD_H_
#define LIB_CONCURRENT_SEQLOCK_PAYLOAD_H_

#include <lib/concurrent/common.h>
#include <lib/concurrent/copy.h>

namespace concurrent {

template <typename T, typename SeqLockType>
class SeqLockPayload : protected WellDefinedCopyable<T> {
 public:
  // The sync options we use for this payload are specified for us based on the
  // lock type we plan to use this payload with.
  static constexpr SyncOpt kSyncOpt = SeqLockType::kCopyWrapperSyncOpt;

  // Forwarding constructor for our payload, and an explicitly default destructor.
  template <typename... Args>
  SeqLockPayload(Args&&... args) : WellDefinedCopyable<T>(std::forward<Args>(args)...) {
    // The payload sync option selected by the lock should always be AcqRelOps,
    // or None (in the case where the lock is using fences).  Assert this at
    // compile time.
    static_assert((kSyncOpt == SyncOpt::AcqRelOps) || (kSyncOpt == SyncOpt::None));
  }
  ~SeqLockPayload() = default;

  // No copy, no move
  SeqLockPayload(const SeqLockPayload<T, SeqLockType>&) = delete;
  SeqLockPayload(SeqLockPayload<T, SeqLockType>&&) = delete;
  SeqLockPayload<T, SeqLockType>& operator=(const SeqLockPayload<T, SeqLockType>&) = delete;
  SeqLockPayload<T, SeqLockType>& operator=(SeqLockPayload<T, SeqLockType>&&) = delete;

  // Specific versions of Read/Update which always use the sync-opt dictated to
  // us by our associated lock type.
  void Read(T& dst) const { WellDefinedCopyable<T>::template Read<kSyncOpt>(dst); }
  void Update(const T& src) { WellDefinedCopyable<T>::template Update<kSyncOpt>(src); }

  // Finally, manually expose the |unsynchronized_get| from our base class.
  using WellDefinedCopyable<T>::unsynchronized_get;
};

}  // namespace concurrent

#endif  // LIB_CONCURRENT_SEQLOCK_PAYLOAD_H_
