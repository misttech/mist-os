// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "kernel/attribution.h"

fbl::DoublyLinkedList<AttributionObject*> AttributionObject::all_attribution_objects_;

AttributionObject AttributionObject::kernel_attribution_;

void AttributionObject::KernelAttributionInit() TA_NO_THREAD_SAFETY_ANALYSIS {
  AttributionObject::kernel_attribution_.Adopt();
  AttributionObject::kernel_attribution_.SetOwningKoid(0);
  all_attribution_objects_.push_back(&AttributionObject::kernel_attribution_);
}

AttributionObject::~AttributionObject() {
  AttributionObject::RemoveAttributionFromGlobalList(this);
}

void AttributionObject::AddAttributionToGlobalList(AttributionObject* obj) {
  ZX_DEBUG_ASSERT(obj != nullptr);
  Guard<Mutex> guard{AllAttributionObjectsLock::Get()};
  all_attribution_objects_.push_back(obj);
}

void AttributionObject::RemoveAttributionFromGlobalList(AttributionObject* obj) {
  ZX_DEBUG_ASSERT(obj != nullptr);
  Guard<Mutex> guard{AllAttributionObjectsLock::Get()};
  all_attribution_objects_.erase(*obj);
}
