// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/handoff/handoff.h>
#include <lib/mistos/zx/vmo.h>

#include <lk/init.h>
#include <phys/handoff.h>

namespace {

HandoffEnd gEnd;

void handoff_init(uint level) { gEnd = EndHandoff(); }

}  // namespace

zx::unowned_vmo GetZbi() {
  fbl::AllocChecker ac;
  auto value = fbl::MakeRefCountedChecked<zx::Value>(&ac, gEnd.zbi.get());
  ZX_ASSERT(ac.check());
  return ktl::move(zx::unowned_vmo(value));
}

LK_INIT_HOOK(handoff, handoff_init, LK_INIT_LEVEL_USER - 1)
