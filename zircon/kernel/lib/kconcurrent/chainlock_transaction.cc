// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/kconcurrent/chainlock_transaction.h>

#include <kernel/thread.h>

namespace kconcurrent {

void ChainLockTransaction::PreemptDisable() {
  Thread::Current::preemption_state().PreemptDisable();
}
void ChainLockTransaction::PreemptReenable() {
  Thread::Current::preemption_state().PreemptReenable();
}

void ChainLockTransaction::EagerReschedDisable() {
  Thread::Current::preemption_state().EagerReschedDisable();
}
void ChainLockTransaction::EagerReschedReenable() {
  Thread::Current::preemption_state().EagerReschedReenable();
}

}  // namespace kconcurrent
