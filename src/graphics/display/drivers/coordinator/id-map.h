// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ID_MAP_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ID_MAP_H_

#include <cstddef>
#include <functional>

#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>

namespace display {

// Helper for allowing structs which are identified by unique ids to be put in a hashmap.
template <typename PtrType, typename IdType>
class IdMappable {
 private:
  // Private forward-declarations to define the hash table.
  using IdMappableNodeState = fbl::SinglyLinkedListNodeState<PtrType>;
  struct IdMappableTraits {
    static IdMappableNodeState& node_state(IdMappable& node) { return node.id_mappable_state_; }
  };
  using HashTableLinkedListType = fbl::SinglyLinkedListCustomTraits<PtrType, IdMappableTraits>;

 public:
  using Map = fbl::HashTable</*KeyType=*/IdType, PtrType, /*BucketType=*/HashTableLinkedListType>;

  static size_t GetHash(IdType id) { return std::hash<IdType>()(id); }
  IdType GetKey() const { return id; }

  IdType id;

 private:
  IdMappableNodeState id_mappable_state_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ID_MAP_H_
