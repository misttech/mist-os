// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_INTERRUPT_KEY_ALLOCATOR_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_INTERRUPT_KEY_ALLOCATOR_H_

#include <zircon/assert.h>

#include <unordered_set>

namespace bt_hci_marvell {

// Assign unique 64-bit identifiers that will be used as interrupt keys. In the most common use
// case, we'll allocate only a handful of these. Note that this class is not thread-safe.
class InterruptKeyAllocator {
 public:
  // We don't expect to have a double-digit number of keys, so set an arbitrary, high limit that
  // will indicate if something has gone sideways.
  static constexpr size_t kMaxNumberOfActiveKeys = 100;

  uint64_t CreateKey() {
    uint64_t candidate_key;

    // If we reach the limit, we'll want to reconsider our assumptions about how the driver
    // operates.
    ZX_ASSERT(used_keys_.size() < kMaxNumberOfActiveKeys);

    // We'll be prepared for wrapping, although this is incredibly unlikely.
    do {
      candidate_key = next_interrupt_key_++;
    } while (used_keys_.find(candidate_key) != used_keys_.end());

    // Add to our set of keys currently in use.
    used_keys_.insert(candidate_key);
    return candidate_key;
  }
  void RemoveKey(uint64_t key) { used_keys_.erase(key); }

 protected:
  // Semi-exposed for testing
  uint64_t next_interrupt_key_ = 0;

 private:
  // Set of all keys currently in use
  std::unordered_set<uint64_t> used_keys_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_INTERRUPT_KEY_ALLOCATOR_H_
