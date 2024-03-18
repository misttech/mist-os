// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "kernel/attribution.h"

fbl::DoublyLinkedList<AttributionObjectNode*> AttributionObjectNode::all_nodes_;

#if KERNEL_BASED_MEMORY_ATTRIBUTION
fbl::RefPtr<AttributionObject> AttributionObject::kernel_attribution_object_;
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
  DEBUG_ASSERT(node->InContainer());
  all_nodes_.erase(*node);
}

AttributionObject* AttributionObjectNode::DowncastToAttributionObject() {
  return node_type_ == NodeType::AttributionObject ? static_cast<AttributionObject*>(this)
                                                   : nullptr;
}

#if KERNEL_BASED_MEMORY_ATTRIBUTION
void AttributionObject::KernelAttributionInit() TA_NO_THREAD_SAFETY_ANALYSIS {
  fbl::AllocChecker ac;
  kernel_attribution_object_ = fbl::MakeRefCountedChecked<AttributionObject>(&ac);
  ASSERT(ac.check());
}
#endif

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
