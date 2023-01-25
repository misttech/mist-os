// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "handoff-prep.h"

#include <lib/boot-options/boot-options.h>
#include <lib/llvm-profdata/llvm-profdata.h>
#include <lib/memalloc/pool-mem-config.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/trivial-allocator/new.h>
#include <stdio.h>
#include <string-file.h>
#include <zircon/assert.h>

#include <ktl/algorithm.h>
#include <phys/allocation.h>
#include <phys/elf-image.h>
#include <phys/handoff.h>
#include <phys/main.h>
#include <phys/new.h>
#include <phys/symbolize.h>

#include "log.h"

#include <ktl/enforce.h>

namespace {

// Carve out some physical pages requested for testing before handing off.
void FindTestRamReservation(RamReservation& ram) {
  ZX_ASSERT_MSG(!ram.paddr, "Must use kernel.test.ram.reserve=SIZE without ,ADDRESS!");

  memalloc::Pool& pool = Allocation::GetPool();

  // Don't just use Pool::Allocate because that will use the first (lowest)
  // address with space.  The kernel's PMM initialization doesn't like the
  // earliest memory being split up too small, and anyway that's not very
  // representative of just a normal machine with some device memory elsewhere,
  // which is what the test RAM reservation is really meant to simulate.
  // Instead, find the highest-addressed, most likely large chunk that is big
  // enough and just make it a little smaller, which is probably more like what
  // an actual machine with a little less RAM would look like.

  auto it = pool.end();
  while (true) {
    if (it == pool.begin()) {
      break;
    }
    --it;
    if (it->type == memalloc::Type::kFreeRam && it->size >= ram.size) {
      uint64_t aligned_start = (it->addr + it->size - ram.size) & -uint64_t{ZX_PAGE_SIZE};
      uint64_t aligned_end = aligned_start + ram.size;
      if (aligned_start >= it->addr && aligned_end <= aligned_start + ram.size) {
        if (pool.UpdateFreeRamSubranges(memalloc::Type::kTestRamReserve, aligned_start, ram.size)
                .is_ok()) {
          ram.paddr = aligned_start;
          if (gBootOptions->phys_verbose) {
            // Dump out the memory usage again to show the reservation.
            printf("%s: Physical memory after kernel.test.ram.reserve carve-out:\n", ProgramName());
            pool.PrintMemoryRanges(ProgramName());
          }
          return;
        }
        // Don't try another spot if something went wrong.
        break;
      }
    }
  }

  printf("%s: ERROR: Cannot reserve %#" PRIx64
         " bytes of RAM for kernel.test.ram.reserve request!\n",
         ProgramName(), ram.size);
}

}  // namespace

void HandoffPrep::Init(ktl::span<ktl::byte> buffer) {
  // TODO(fxbug.dev/84107): Use the buffer inside the data ZBI via a
  // SingleHeapAllocator.  Later allocator() will return a real(ish) allocator.
  allocator_.allocate_function() = AllocateFunction(buffer);

  fbl::AllocChecker ac;
  handoff_ = new (allocator(), ac) PhysHandoff;
  ZX_ASSERT_MSG(ac.check(), "handoff buffer too small for PhysHandoff!");
}

void HandoffPrep::SetInstrumentation() {
  auto publish_vmo = [this](ktl::string_view name, size_t content_size) {
    return PublishVmo(name, content_size);
  };
  for (const ElfImage* module : gSymbolize->modules()) {
    module->PublishInstrumentation(publish_vmo);
  }
}

ktl::span<ktl::byte> HandoffPrep::PublishVmo(ktl::string_view name, size_t content_size) {
  if (content_size == 0) {
    return {};
  }

  fbl::AllocChecker ac;
  HandoffVmo* handoff_vmo = new (gPhysNew<memalloc::Type::kPhysScratch>, ac) HandoffVmo;
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu scratch bytes for HandoffVmo",
                sizeof(*handoff_vmo));

  handoff_vmo->vmo.set_name(name);

  ktl::span buffer = New(handoff_vmo->vmo.data, ac, content_size);
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu bytes for %.*s", content_size,
                static_cast<int>(name.size()), name.data());
  ZX_DEBUG_ASSERT(buffer.size() == content_size);

  vmos_.push_front(handoff_vmo);
  return buffer;
}

void HandoffPrep::FinishVmos() {
  fbl::AllocChecker ac;
  ktl::span phys_vmos = New(handoff()->vmos, ac, vmos_.size());
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu * %zu-byte PhysVmo", vmos_.size(),
                sizeof(PhysVmo));
  ZX_DEBUG_ASSERT(phys_vmos.size() == vmos_.size());

  for (PhysVmo& phys_vmo : phys_vmos) {
    phys_vmo = ktl::move(vmos_.pop_front()->vmo);
  }
}

BootOptions& HandoffPrep::SetBootOptions(const BootOptions& boot_options) {
  fbl::AllocChecker ac;
  BootOptions* handoff_options = New(handoff()->boot_options, ac, *gBootOptions);
  ZX_ASSERT_MSG(ac.check(), "cannot allocate handoff BootOptions!");

  if (handoff_options->test_ram_reserve) {
    FindTestRamReservation(*handoff_options->test_ram_reserve);
  }

  return *handoff_options;
}

void HandoffPrep::PublishLog(ktl::string_view name, Log&& log) {
  if (log.empty()) {
    return;
  }

  const size_t content_size = log.size_bytes();
  Allocation buffer = ktl::move(log).TakeBuffer();
  ZX_ASSERT(content_size <= buffer.size_bytes());

  ktl::span copy = PublishVmo(name, content_size);
  ZX_ASSERT(copy.size_bytes() == content_size);
  memcpy(copy.data(), buffer.get(), content_size);
}
