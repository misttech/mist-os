// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/handoff/handoff.h"

#include <lib/mistos/zx/vmo.h>

#include <ktl/move.h>
#include <lk/init.h>
#include <phys/handoff.h>

namespace {
HandoffEnd gEnd;
}  // namespace

void mistos_init(HandoffEnd handoff_end) { gEnd = ktl::move(handoff_end); }

zx::unowned_vmo GetZbi() {
  fbl::AllocChecker ac;
  auto value = fbl::MakeRefCountedChecked<zx::Value>(&ac, gEnd.zbi.get());
  ZX_ASSERT(ac.check());
  return ktl::move(zx::unowned_vmo(value));
}
