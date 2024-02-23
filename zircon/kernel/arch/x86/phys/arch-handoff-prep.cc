// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/code-patching/code-patches.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zbi-format/zbi.h>
#include <zircon/assert.h>

#include <ktl/byte.h>
#include <ktl/span.h>
#include <phys/arch/arch-handoff.h>
#include <phys/handoff.h>

#include "handoff-prep.h"

#include <ktl/enforce.h>

ArchPatchInfo ArchPreparePatchInfo() { return {}; }

void HandoffPrep::ArchHandoff(const ArchPatchInfo& patch_info) {}

void HandoffPrep::ArchSummarizeMiscZbiItem(const zbi_header_t& header,
                                           ktl::span<const ktl::byte> payload) {
  ZX_DEBUG_ASSERT(handoff_);
  ArchPhysHandoff& arch_handoff = handoff_->arch_handoff;

  switch (header.type) {
    case ZBI_TYPE_FRAMEBUFFER:
      ZX_ASSERT(payload.size() >= sizeof(zbi_swfb_t));
      arch_handoff.framebuffer = *reinterpret_cast<const zbi_swfb_t*>(payload.data());
      SaveForMexec(header, payload);
      break;
  }
}
