// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/interrupt_key_allocator.h"

#include <unordered_set>

#include <zxtest/zxtest.h>

namespace bt_hci_marvell {

class TestableKeyAllocator : public InterruptKeyAllocator {
 public:
  // Simple accessors to protected members
  uint64_t GetNextKey() { return next_interrupt_key_; }
  void SetNextKey(uint64_t new_next_key) { next_interrupt_key_ = new_next_key; }
  constexpr size_t GetMaxNumberOfKeys() { return kMaxNumberOfActiveKeys; }
};

class InterruptKeyAllocatorTest : public zxtest::Test {
 public:
  InterruptKeyAllocatorTest() {
    size_t max_key_count = key_alloc_.GetMaxNumberOfKeys();
    allocated_keys_.reserve(max_key_count);
    allocated_keys_.assign(max_key_count, -1);
  }

  // Allocate a new key (save in specified index of our tracking vector), and verify that it is
  // unique to all of the active keys.
  void Allocate(size_t ndx) {
    uint64_t new_key = key_alloc_.CreateKey();

    // New key should not have been already seen
    EXPECT_EQ(allocated_key_set_.find(new_key), allocated_key_set_.end());

    // Keep track of values seen
    allocated_keys_.at(ndx) = new_key;
    allocated_key_set_.insert(new_key);
  }

  // Deallocate an active key (found in specified index of our tracking vector), and remove from
  // the active key set.
  void Deallocate(size_t ndx) {
    key_alloc_.RemoveKey(allocated_keys_[ndx]);
    allocated_key_set_.erase(allocated_keys_[ndx]);
  }

 protected:
  TestableKeyAllocator key_alloc_;
  std::vector<uint64_t> allocated_keys_;
  std::unordered_set<uint64_t> allocated_key_set_;
};

TEST_F(InterruptKeyAllocatorTest, BasicOperation) {
  size_t max_key_count = key_alloc_.GetMaxNumberOfKeys();

  // Verify that we can allocate the maximum number of keys
  for (size_t i = 0; i < max_key_count; i++) {
    Allocate(i);
  }

  // Test adding and removing keys one at a time
  ASSERT_LT(10, max_key_count);
  for (size_t i = 0; i < 10; i++) {
    Deallocate(i);
    Allocate(i);
  }

  // Remove a few keys, reach into the guts of the key allocator and reset the internal counter
  // that is used to generate new keys, and verify that we still don't see duplicates when we
  // allocate new keys.
  ASSERT_LT(23, max_key_count);
  Deallocate(4);
  Deallocate(14);
  Deallocate(23);
  key_alloc_.SetNextKey(allocated_keys_[0]);
  Allocate(4);
  Allocate(14);
  Allocate(23);

  // And finally, deallocate everything
  for (size_t i = 0; i < max_key_count; i++) {
    Deallocate(i);
  }
}

}  // namespace bt_hci_marvell
