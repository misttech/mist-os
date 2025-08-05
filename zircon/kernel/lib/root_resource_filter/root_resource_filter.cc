// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/counters.h>
#include <lib/memalloc/range.h>
#include <lib/root_resource_filter.h>
#include <lib/root_resource_filter_internal.h>
#include <stdio.h>
#include <trace.h>
#include <zircon/syscalls/resource.h>

#include <ktl/algorithm.h>
#include <ktl/byte.h>
#include <lk/init.h>
#include <phys/handoff.h>
#include <vm/pmm.h>
#include <vm/vm_object_paged.h>
#include <vm/vm_object_physical.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

KCOUNTER(resource_ranges_denied, "resource.denied_ranges")

namespace {
// The global singleton filter
RootResourceFilter g_root_resource_filter;
}  // namespace

void RootResourceFilter::Finalize() {
  // Add all (normalized) RAM ranges to the set of regions to deny,
  auto filter_for_ram = [](memalloc::Type type) -> ktl::optional<memalloc::Type> {
    // Treat reserved test RAM as MMIO.
    if (type == memalloc::Type::kTestRamReserve || !memalloc::IsRamType(type)) {
      return {};
    }
    return memalloc::Type::kFreeRam;
  };
  auto add_region = [this](const memalloc::Range& range) {
    // If we cannot add the range to our set of regions to deny, it can only be
    // because we failed a heap allocation which should be impossible at this
    // point. If it does happen, panic. We cannot run if we cannot enforce the
    // deny list.
    zx_status_t status = mmio_deny_.AddRegion({.base = range.addr, .size = range.size},
                                              RegionAllocator::AllowOverlap::No);
    ASSERT(status == ZX_OK);
    return true;
  };
  memalloc::NormalizeRanges(gPhysHandoff->memory.get(), add_region, filter_for_ram);

  // Ditto for the NVRAM range, which backs kernel state like the crashlog.
  if (const ktl::optional<MappedMemoryRange> nvram = gPhysHandoff->nvram) {
    mmio_deny_.AddRegion({.base = nvram->paddr, .size = nvram->size_bytes()},
                         RegionAllocator::AllowOverlap::No);
  }

  // Dump the deny list at spew level for debugging purposes.
  if (DPRINTF_ENABLED_FOR_LEVEL(SPEW)) {
    dprintf(SPEW, "Final MMIO Deny list is:\n");
    mmio_deny_.WalkAvailableRegions([](const ralloc_region_t* region) -> bool {
      dprintf(SPEW, "Region [%#lx, %#lx)\n", region->base, region->base + region->size);
      return true;  // Keep printing, don't stop now!
    });
#ifdef __x86_64__
    ioport_deny_.WalkAvailableRegions([](const ralloc_region_t* region) -> bool {
      dprintf(SPEW, "IoPort [%#lx, %#lx)\n", region->base, region->base + region->size);
      return true;  // Keep printing, don't stop now!
    });
#endif
  }
}

bool RootResourceFilter::IsRegionAllowed(uintptr_t base, size_t size, zx_rsrc_kind_t kind) const {
  switch (kind) {
    case ZX_RSRC_KIND_MMIO:
      return !mmio_deny_.TestRegionIntersects({.base = base, .size = size},
                                              RegionAllocator::TestRegionSet::Available);
#ifdef __x86_64__
    case ZX_RSRC_KIND_IOPORT:
      return !ioport_deny_.TestRegionIntersects({.base = base, .size = size},
                                                RegionAllocator::TestRegionSet::Available);
#endif
    default:
      return true;
  };
}

void root_resource_filter_add_deny_region(uintptr_t base, size_t size, zx_rsrc_kind_t kind) {
  // We only enforce deny regions for MMIO right now. In the future, if someone
  // wants to limit other regions as well (perhaps the I/O port space for x64),
  // they need to come back here and add another RegionAllocator instance to
  // enforce the rules for the new zone.
  g_root_resource_filter.AddDenyRegion(base, size, kind);
}

bool root_resource_filter_can_access_region(uintptr_t base, size_t size, zx_rsrc_kind_t kind) {
  // Keep track of the number of regions that we end up denying. Typically, in
  // a properly operating system (aside from explicit tests) this should be 0.
  // Anything else probably indicates either malice or a bug somewhere.
  if (!g_root_resource_filter.IsRegionAllowed(base, size, kind)) {
    LTRACEF("WARNING - Denying range request [%016lx, %016lx) kind (%u)\n", base, base + size,
            kind);
    kcounter_add(resource_ranges_denied, 1);
    return false;
  }

  return true;
}

// Finalize the ZBI filter just before we start user mode. This will add the
// RAM regions described by the ZBI into the filter, and then subtract out
// the reserved RAM regions so that userspace can create MMIO resource ranges
// which target reserved RAM.
static void finalize_root_resource_filter(uint) { g_root_resource_filter.Finalize(); }
LK_INIT_HOOK(finalize_root_resource_filter, finalize_root_resource_filter, LK_INIT_LEVEL_USER - 1)
