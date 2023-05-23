// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "kernel/attribution.h"

fbl::DoublyLinkedList<AttributionObjectNode*> AttributionObjectNode::all_nodes_;
fbl::DoublyLinkedList<AttributionObjectsCursor*> AttributionObjectsCursor::all_cursors_;

// Save some memory if kernel-based memory attribution is disabled.
#if KERNEL_BASED_MEMORY_ATTRIBUTION
AttributionObject AttributionObject::kernel_attribution_;
#endif

void AttributionObjectNode::AddToGlobalListLocked(AttributionObjectNode* where,
                                                  AttributionObjectNode* node) {
  if (likely(where != nullptr)) {
    all_nodes_.insert(*where, node);
  } else {
    all_nodes_.push_back(node);
  }
}

void AttributionObjectNode::RemoveFromGlobalListLocked(AttributionObjectNode* node) {
  // We are about to remove |node| from the list: advance any iterator that
  // currently points to it.
  AttributionObjectsCursor::SkipNodeBeingRemoved(node);

  DEBUG_ASSERT(node->InContainer());
  all_nodes_.erase(*node);
}

AttributionObject* AttributionObjectNode::DowncastToAttributionObject() {
  return node_type_ == NodeType::AttributionObject ? static_cast<AttributionObject*>(this)
                                                   : nullptr;
}

void AttributionObject::KernelAttributionInit() TA_NO_THREAD_SAFETY_ANALYSIS {
  AttributionObject::kernel_attribution_.Adopt();
}

AttributionObject::~AttributionObject() {
  if (!InContainer()) {
    return;
  }

  Guard<CriticalMutex> guard{AllAttributionObjectsLock::Get()};
  RemoveFromGlobalListLocked(this);
}

void AttributionObject::AddToGlobalListWithKoid(AttributionObjectNode* where,
                                                zx_koid_t owning_koid) {
  owning_koid_ = owning_koid;

  Guard<CriticalMutex> guard{AllAttributionObjectsLock::Get()};
  AddToGlobalListLocked(where, this);
}

AttributionObjectsCursor::AttributionObjectsCursor(AttributionObjectNode* begin,
                                                   AttributionObjectNode* end) {
  Guard<CriticalMutex> guard{AttributionObjectNode::AllAttributionObjectsLock::Get()};
  next_ = AttributionObjectNode::all_nodes_.make_iterator(*begin);
  end_ = AttributionObjectNode::all_nodes_.make_iterator(*end);
  all_cursors_.push_back(this);
}

AttributionObjectsCursor::~AttributionObjectsCursor() {
  Guard<CriticalMutex> guard{AttributionObjectNode::AllAttributionObjectsLock::Get()};
  DEBUG_ASSERT(InContainer());
  all_cursors_.erase(*this);
}

ktl::optional<zx_info_memory_attribution_t> AttributionObjectsCursor::Next() {
  Guard<CriticalMutex> guard{AttributionObjectNode::AllAttributionObjectsLock::Get()};

  // Try to find the next non-sentinel node.
  while (next_ != end_) {
    AttributionObjectNode& curr = *next_++;
    if (AttributionObject* obj = curr.DowncastToAttributionObject()) {
      return obj->ToInfoEntry();
    }
  }

  return ktl::nullopt;
}

void AttributionObjectsCursor::SkipNodeBeingRemoved(AttributionObjectNode* node) {
  for (AttributionObjectsCursor& cursor : all_cursors_) {
    // Is this cursor currently pointing to the node about to be removed?
    if (cursor.next_.CopyPointer() == node) {
      // AttributionObjectsCursor requires |end_| not to be removed while the cursor exists. This
      // guarantees that advancing the cursor will not fall through the end of its range.
      DEBUG_ASSERT(cursor.next_ != cursor.end_);
      ++cursor.next_;
    }
  }
}
