// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/handoff/handoff.h>

#include <lk/init.h>
#include <phys/handoff.h>

namespace {

HandoffEnd gEnd;

void handoff_init(uint level) { gEnd = EndHandoff(); }

}  // namespace

fbl::RefPtr<VmObjectDispatcher> GetZbi() {
  return fbl::RefPtr<VmObjectDispatcher>::Downcast(gEnd.zbi->dispatcher());
}

LK_INIT_HOOK(handoff, handoff_init, LK_INIT_LEVEL_USER - 1)
