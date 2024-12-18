// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <assert.h>
#include <bits.h>
#include <debug.h>
#include <inttypes.h>
#include <lib/arch/riscv64/feature.h>
#include <lib/arch/riscv64/page-table.h>
#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/fit/defer.h>
#include <lib/heap.h>
#include <lib/ktrace.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/aspace.h>
#include <arch/ops.h>
#include <arch/riscv64/feature.h>
#include <arch/riscv64/mmu.h>
#include <arch/riscv64/sbi.h>
#include <kernel/mp.h>
#include <kernel/mutex.h>
#include <ktl/algorithm.h>
#include <phys/arch/arch-handoff.h>
#include <vm/arch_vm_aspace.h>
#include <vm/physmap.h>
#include <vm/pmm.h>
#include <vm/vm.h>

#include "asid_allocator.h"

#define LOCAL_TRACE 0
#define TRACE_CONTEXT_SWITCH 0

/* ktraces just local to this file */
#define LOCAL_KTRACE_ENABLE 0

// TODO-rvbringup: figure out why this isn't working
#if 0
#define LOCAL_KTRACE(string, args...) \
  KTRACE_CPU_INSTANT_ENABLE(LOCAL_KTRACE_ENABLE, "kernel:probe", label, ##args)
#else
#define LOCAL_KTRACE(string, args...)
#endif

// Static relocated base to prepare for KASLR. Used at early boot and by gdb
// script to know the target relocated address.
// TODO(https://fxbug.dev/42098994): Choose it randomly.
uint64_t kernel_relocated_base = kArchHandoffVirtualAddress;

// The main translation table for the kernel. Used by the one kernel address space
// when kernel only threads are active.
alignas(PAGE_SIZE) pte_t riscv64_kernel_translation_table[RISCV64_MMU_PT_ENTRIES];

// The physical address of a copy of the above table with memory identity
// mapped at 0, stored for loading from secondary CPUs.
paddr_t riscv64_kernel_bootstrap_translation_table_phys;

namespace {

// 256 top level page tables that are always mapped in the kernel half of all root
// page tables. This allows for no need to explicitly maintain consistency between
// the official kernel page table root and all of the user address spaces as they
// come and go.
alignas(PAGE_SIZE) pte_t
    riscv64_kernel_top_level_page_tables[RISCV64_MMU_PT_ENTRIES / 2][RISCV64_MMU_PT_ENTRIES];

// Track the size and capability of the hardware ASID, and if its in use.
uint64_t riscv_asid_mask;
bool riscv_use_asid;
AsidAllocator asid_allocator;

KCOUNTER(cm_flush_call, "mmu.consistency_manager.flush_call")
KCOUNTER(cm_asid_invalidate, "mmu.consistency_manager.asid_invalidate")
KCOUNTER(cm_global_invalidate, "mmu.consistency_manager.global_invalidate")
KCOUNTER(cm_page_run_invalidate, "mmu.consistency_manager.page_run_invalidate")
KCOUNTER(cm_single_page_invalidate, "mmu.consistency_manager.single_page_invalidate")
KCOUNTER(cm_local_page_invalidate, "mmu.consistency_manager.local_page_invalidate")

KCOUNTER(vm_mmu_protect_make_execute_calls, "vm.mmu.protect.make_execute_calls")
KCOUNTER(vm_mmu_protect_make_execute_pages, "vm.mmu.protect.make_execute_pages")

// Return the asid that should be assigned to the kernel aspace.
uint16_t kernel_asid() {
  // When using ASIDs, the kernel is assigned KERNEL_ASID (1) instead of UNUSED_ASID (0)
  // for two reasons:
  // a) To keep it logically separate from UNUSED_ASID for debug and assert reasons.
  // b) A note in SiFive documentation for various cores that says
  //   "Supervisor software that uses ASIDs should use a nonzero ASID value to refer to the same
  //   address space across all harts in the supervisor execution environment (SEE) and should not
  //   use an ASID value of 0. If supervisor software does not use ASIDs, then the ASID field in the
  //   satp CSR should be set to 0."
  // Unclear if this is simply a suggestion or hardware will perform some sort of optimization based
  // on this.
  return riscv_use_asid ? MMU_RISCV64_KERNEL_ASID : MMU_RISCV64_UNUSED_ASID;
}

// given a va address and the level, compute the index in the current PT
constexpr uint vaddr_to_index(vaddr_t va, uint level) {
  // levels count down from PT_LEVELS - 1
  DEBUG_ASSERT(level < RISCV64_MMU_PT_LEVELS);

  // canonicalize the address
  va &= RISCV64_MMU_CANONICAL_MASK;

  uint index =
      ((va >> PAGE_SIZE_SHIFT) >> (level * RISCV64_MMU_PT_SHIFT)) & (RISCV64_MMU_PT_ENTRIES - 1);
  LTRACEF_LEVEL(3, "canonical va %#lx, level %u = index %#x\n", va, level, index);

  return index;
}

constexpr uintptr_t page_size_per_level(uint level) {
  // levels count down from PT_LEVELS - 1
  DEBUG_ASSERT(level < RISCV64_MMU_PT_LEVELS);

  return 1UL << (PAGE_SIZE_SHIFT + level * RISCV64_MMU_PT_SHIFT);
}

constexpr uintptr_t page_mask_per_level(uint level) { return page_size_per_level(level) - 1; }

// Convert user level mmu flags to flags that go in leaf descriptors.
pte_t mmu_flags_to_pte_attr(uint flags, bool global) {
  pte_t attr = RISCV64_PTE_V;
  attr |= RISCV64_PTE_A | RISCV64_PTE_D;
  attr |= (flags & ARCH_MMU_FLAG_PERM_USER) ? RISCV64_PTE_U : 0;
  attr |= (flags & ARCH_MMU_FLAG_PERM_READ) ? RISCV64_PTE_R : 0;
  attr |= (flags & ARCH_MMU_FLAG_PERM_WRITE) ? RISCV64_PTE_W : 0;
  attr |= (flags & ARCH_MMU_FLAG_PERM_EXECUTE) ? RISCV64_PTE_X : 0;
  attr |= (global) ? RISCV64_PTE_G : 0;

  // Svpbmt support
  if (gRiscvFeatures[arch::RiscvFeature::kSvpbmt]) {
    switch (flags & ARCH_MMU_FLAG_CACHE_MASK) {
      case ARCH_MMU_FLAG_CACHED:
        attr |= RISCV64_PTE_PBMT_PMA;
        break;
      case ARCH_MMU_FLAG_UNCACHED:
      case ARCH_MMU_FLAG_WRITE_COMBINING:
        attr |= RISCV64_PTE_PBMT_NC;
        break;
      case ARCH_MMU_FLAG_UNCACHED_DEVICE:
        attr |= RISCV64_PTE_PBMT_IO;
        break;
    }
  }

  return attr;
}

// Convert leaf pte flags to mmu flags.
uint mmu_flags_from_pte(pte_t pte) {
  uint mmu_flags = 0;
  mmu_flags |= (pte & RISCV64_PTE_U) ? ARCH_MMU_FLAG_PERM_USER : 0;
  mmu_flags |= (pte & RISCV64_PTE_R) ? ARCH_MMU_FLAG_PERM_READ : 0;
  mmu_flags |= (pte & RISCV64_PTE_W) ? ARCH_MMU_FLAG_PERM_WRITE : 0;
  mmu_flags |= (pte & RISCV64_PTE_X) ? ARCH_MMU_FLAG_PERM_EXECUTE : 0;

  // Svpbmt feature
  if (gRiscvFeatures[arch::RiscvFeature::kSvpbmt]) {
    switch (pte & RISCV64_PTE_PBMT_MASK) {
      case RISCV64_PTE_PBMT_PMA:
        // PMA state basically means default cache paramaters, as determined by physical address.
        // Don't actually report it as CACHED here since we can't know here what the actual
        // underlying physical range's type is.
        break;
      case RISCV64_PTE_PBMT_NC:
        mmu_flags |= ARCH_MMU_FLAG_UNCACHED;
        break;
      case RISCV64_PTE_PBMT_IO:
        mmu_flags |= ARCH_MMU_FLAG_UNCACHED_DEVICE;
        break;
      default:
        panic("unexpected pte value %" PRIx64, pte);
    }
  }

  return mmu_flags;
}

// Construct a non leaf page table entry.
// For all inner page tables for the entire kernel hierarchy, set the global bit.
constexpr pte_t mmu_non_leaf_pte(paddr_t pa, bool global) {
  return riscv64_pte_pa_to_pte(pa) | (global ? RISCV64_PTE_G : 0) | RISCV64_PTE_V;
}

void update_pte(volatile pte_t* pte, pte_t newval) { *pte = newval; }

zx::result<size_t> first_used_page_table_entry(const volatile pte_t* page_table) {
  const size_t count = 1U << (PAGE_SIZE_SHIFT - 3);

  for (size_t i = 0; i < count; i++) {
    pte_t pte = page_table[i];
    if (riscv64_pte_is_valid(pte)) {
      return zx::ok(i);
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

bool page_table_is_clear(const volatile pte_t* page_table) {
  const zx::result<size_t> index_result = first_used_page_table_entry(page_table);
  if (index_result.is_error()) {
    LTRACEF("page table at %p is clear\n", page_table);
  } else {
    LTRACEF("page_table at %p still in use, index %zu is %#" PRIx64 "\n", page_table, *index_result,
            page_table[*index_result]);
  }
  return index_result.is_error();
}

constexpr Riscv64AspaceType AspaceTypeFromFlags(uint mmu_flags) {
  // Kernel/Guest flags are mutually exclusive. Ensure at most 1 is set.
  DEBUG_ASSERT(((mmu_flags & ARCH_ASPACE_FLAG_KERNEL) != 0) +
                   ((mmu_flags & ARCH_ASPACE_FLAG_GUEST) != 0) <=
               1);
  if (mmu_flags & ARCH_ASPACE_FLAG_KERNEL) {
    return Riscv64AspaceType::kKernel;
  }
  if (mmu_flags & ARCH_ASPACE_FLAG_GUEST) {
    return Riscv64AspaceType::kGuest;
  }
  return Riscv64AspaceType::kUser;
}

constexpr ktl::string_view Riscv64AspaceTypeName(Riscv64AspaceType type) {
  switch (type) {
    case Riscv64AspaceType::kKernel:
      return "kernel";
    case Riscv64AspaceType::kUser:
      return "user";
    case Riscv64AspaceType::kGuest:
      return "guest";
  }
  __builtin_abort();
}

constexpr bool IsUserBaseSizeValid(vaddr_t base, size_t size) {
  // Make sure size is > 0 and the addition of base + size is contained entirely within
  // the user half of the canonical address space.
  if (size == 0) {
    return false;
  }

  if (!IS_PAGE_ALIGNED(base) || !IS_PAGE_ALIGNED(size)) {
    return false;
  }

  if (base & kRiscv64CanonicalAddressMask) {
    return false;
  }

  uint64_t computed_user_aspace_top = 0;
  if (add_overflow(base, size, &computed_user_aspace_top)) {
    return false;
  }

  if ((computed_user_aspace_top - 1) & kRiscv64CanonicalAddressMask) {
    return false;
  }

  return true;
}

// Converts a symbol in the kernel to its physical address based on knowledge of
// where the kernel is loaded virtually and physically. Only works for data within
// the kernel proper.
paddr_t kernel_virt_to_phys(const void* va) {
  uintptr_t pa = reinterpret_cast<uintptr_t>(va);
  pa += get_kernel_base_phys() - kernel_relocated_base;

  return pa;
}

// Argument to SfenceVma.  Used to perform TLB invalidation on an optional range
// with an optional ASID.  When no range is present, the target is all
// addresses.  When no ASID is present the target is invalidated for all ASIDs.
struct SfenceVmaArgs {
  struct Range {
    vaddr_t base;
    size_t size;
  };
  ktl::optional<Range> range;
  ktl::optional<uint16_t> asid;
};

// Issues a sequence of sfence.vma instructions as specified by SfenceVmaArgs.
void SfenceVma(void* _args) {
  DEBUG_ASSERT(arch_ints_disabled());
  auto* args = reinterpret_cast<SfenceVmaArgs*>(_args);

  if (args->range.has_value()) {
    // With range.
    const vaddr_t base = args->range->base;
    const vaddr_t end = base + args->range->size;
    if (args->asid.has_value()) {
      // With range, one ASID.
      const uint16_t asid = args->asid.value();
      for (vaddr_t va = base; va < end; va += PAGE_SIZE) {
        riscv64_tlb_flush_address_one_asid(va, asid);
      }
    } else {
      // With range, all ASIDs.
      for (vaddr_t va = base; va < end; va += PAGE_SIZE) {
        riscv64_tlb_flush_address_all_asids(va);
      }
    }
  } else {
    if (args->asid.has_value()) {
      // All addresses, one ASID.
      const uint16_t asid = args->asid.value();
      riscv64_tlb_flush_asid(asid);
    } else {
      // All addresses, all ASIDs.
      riscv64_tlb_flush_all();
    }
  }
}

}  // namespace

// A consistency manager that tracks TLB updates, walker syncs and free pages in an effort to
// minimize MBs (by delaying and coalescing TLB invalidations) and switching to full ASID
// invalidations if too many TLB invalidations are requested.
// The aspace lock *must* be held over the full operation of the ConsistencyManager, from
// construction to deletion. The lock must be held continuously to deletion, and specifically till
// the actual TLB invalidations occur, due to strategy employed here of only invalidating actual
// vaddrs with changing entries, and not all vaddrs an operation applies to. Otherwise the following
// scenario is possible
//  1. Thread 1 performs an Unmap and removes PTE entries, but drops the lock prior to invalidation.
//  2. Thread 2 performs an Unmap, no PTE entries are removed, no invalidations occur
//  3. Thread 2 now believes the resources (pages) for the region are no longer accessible, and
//     returns them to the pmm.
//  4. Thread 3 attempts to access this region and is now able to read/write to returned pages as
//     invalidations have not occurred.
// This scenario is possible as the mappings here are not the source of truth of resource
// management, but a cache of information from other parts of the system. If thread 2 wanted to
// guarantee that the pages were free it could issue it's own TLB invalidations for the vaddr range,
// even though it found no entries. However this is not the strategy employed here at the moment.
class Riscv64ArchVmAspace::ConsistencyManager {
 public:
  explicit constexpr ConsistencyManager(Riscv64ArchVmAspace& aspace) TA_REQ(aspace.lock_)
      : aspace_(aspace) {}
  ~ConsistencyManager() {
    Flush();
    if (!list_is_empty(&to_free_)) {
      pmm_free(&to_free_);
    }
  }

  // Queue a TLB entry for flushing. This may get turned into a complete ASID flush.
  void FlushEntry(vaddr_t va, bool terminal) {
    LTRACEF("va %#lx, asid %#x, terminal %u\n", va, aspace_.asid_, terminal);

    DEBUG_ASSERT(IS_PAGE_ALIGNED(va));
    DEBUG_ASSERT(aspace_.IsValidVaddr(va));

    if (full_flush_) {
      // If we've already decided to do a full flush, nothing more to track here.
      return;
    }

    // If we're asked to flush a non terminal entry, we're going to need to dump the entire ASID
    // so skip tracking this VA and exit now.
    if (!terminal) {
      full_flush_ = true;
      return;
    }

    // Check we have queued too many entries already.
    if (num_pending_tlb_runs_ >= kMaxPendingTlbRuns) {
      // Most of the time we will now prefer to invalidate the entire ASID, the exception is if
      // this aspace is for the kernel, in which all pages are global and we need to flush
      // them one at a time.
      if (!aspace_.IsKernel()) {
        full_flush_ = true;
        return;
      }

      // Kernel case: Flush what pages we've cached up until now and reset counter to zero.
      Flush();
    }

    if (num_pending_tlb_runs_ > 0) {
      // See if this entry completes the previous run or is the start of the previous run.
      // The latter catches a fairly common case of multiple flushes of the same page in a row.
      auto& run = pending_tlbs_[num_pending_tlb_runs_ - 1];
      if ((run.va + run.count * PAGE_SIZE == va)) {
        run.count++;
        return;
      }
      if (run.va == va) {
        return;
      }
    }

    // Start a new run of entries to track
    pending_tlbs_[num_pending_tlb_runs_].va = va;
    pending_tlbs_[num_pending_tlb_runs_].count = 1;
    num_pending_tlb_runs_++;
  }

  // Performs any pending synchronization of TLBs and page table walkers. Includes the MB to ensure
  // TLB flushes have completed prior to returning to user.
  void Flush() {
    AssertHeld(aspace_.lock_);
    kcounter_add(cm_flush_call, 1);
    if (!full_flush_ && num_pending_tlb_runs_ == 0) {
      return;
    }
    // Need a mb to synchronize any page table updates prior to flushing the TLBs.
    mb();

    // Check if we should just be performing a full ASID invalidation.
    if (full_flush_) {
      aspace_.FlushAsid();
      // If this is a restricted aspace, invalidate the associated unified aspace's ASID.
      if (aspace_.IsRestricted() && aspace_.referenced_aspace_ != nullptr) {
        Guard<Mutex> b{AssertOrderedLock, &aspace_.referenced_aspace_->lock_,
                       aspace_.referenced_aspace_->LockOrder()};
        aspace_.referenced_aspace_->FlushAsid();
      }
    } else {
      for (size_t i = 0; i < num_pending_tlb_runs_; i++) {
        const vaddr_t va = pending_tlbs_[i].va;
        const size_t count = pending_tlbs_[i].count;

        aspace_.FlushTLBEntryRun(va, count);
        // If this is a restricted aspace, invalidate the same run in the unified aspace.
        if (aspace_.IsRestricted() && aspace_.referenced_aspace_ != nullptr) {
          Guard<Mutex> b{AssertOrderedLock, &aspace_.referenced_aspace_->lock_,
                         aspace_.referenced_aspace_->LockOrder()};
          aspace_.referenced_aspace_->FlushTLBEntryRun(va, count);
        }
      }
    }

    // mb to ensure TLB flushes happen prior to returning to user.
    mb();
    num_pending_tlb_runs_ = 0;
    full_flush_ = false;
  }

  // Queue a page for freeing that is dependent on TLB flushing. This is for pages that were
  // previously installed as page tables and they should not be reused until the non-terminal TLB
  // flush has occurred.
  void FreePage(vm_page_t* page) { list_add_tail(&to_free_, &page->queue_node); }

 private:
  // Maximum number of TLB entries we will queue before switching to ASID invalidation.
  static constexpr uint32_t kMaxPendingTlbRuns = 8;

  // vm_page_t's to release to the PMM after the TLB invalidation occurs.
  list_node to_free_ = LIST_INITIAL_VALUE(to_free_);

  // The aspace we are invalidating TLBs for.
  const Riscv64ArchVmAspace& aspace_;

  // Perform a full flush of the entire ASID (or all ASIDs if a kernel aspace) in these cases:
  // 1) We've accumulated more than kMaxPendingTlbRuns run of pages, which are expensive to perform
  // because of cross cpu TLB shootdowns.
  // 2) If we've been asked to flush a non terminal page, which according to the RISC-V
  // privileged spec should involve clearing the entire ASID.
  bool full_flush_ = false;

  // Pending TLBs to flush are stored as a virtual address + a count of pages to flush in a run.
  uint32_t num_pending_tlb_runs_ = 0;

  // A run of pages to flush.
  struct {
    uint64_t va;
    size_t count;
  } pending_tlbs_[kMaxPendingTlbRuns];
};

zx_status_t Riscv64ArchVmAspace::Query(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) {
  Guard<Mutex> al{AssertOrderedLock, &lock_, LockOrder()};
  return QueryLocked(vaddr, paddr, mmu_flags);
}

zx_status_t Riscv64ArchVmAspace::QueryLocked(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) {
  uint level = RISCV64_MMU_PT_LEVELS - 1;

  canary_.Assert();
  LTRACEF("aspace %p, vaddr 0x%lx\n", this, vaddr);

  DEBUG_ASSERT(tt_virt_);

  DEBUG_ASSERT(IsValidVaddr(vaddr));
  if (!IsValidVaddr(vaddr)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  const volatile pte_t* page_table = tt_virt_;

  while (true) {
    ulong index = vaddr_to_index(vaddr, level);
    const pte_t pte = page_table[index];
    const paddr_t pte_addr = riscv64_pte_pa(pte);

    LTRACEF("va %#" PRIxPTR ", index %lu, level %u, pte %#" PRIx64 "\n", vaddr, index, level, pte);

    if (!riscv64_pte_is_valid(pte)) {
      return ZX_ERR_NOT_FOUND;
    }

    if (riscv64_pte_is_leaf(pte)) {
      if (paddr) {
        *paddr = pte_addr + (vaddr & page_mask_per_level(level));
      }
      if (mmu_flags) {
        *mmu_flags = mmu_flags_from_pte(pte);
      }
      LTRACEF("va 0x%lx, paddr 0x%lx, flags 0x%x\n", vaddr, paddr ? *paddr : ~0UL,
              mmu_flags ? *mmu_flags : ~0U);
      return ZX_OK;
    }

    page_table = static_cast<const volatile pte_t*>(paddr_to_physmap(pte_addr));
    level--;
  }
}

zx::result<paddr_t> Riscv64ArchVmAspace::AllocPageTable() {
  // Allocate a page from the pmm via function pointer passed to us in Init().
  // The default is pmm_alloc_page so test and explicitly call it to avoid any unnecessary
  // virtual functions.
  vm_page_t* page;
  paddr_t paddr;
  const zx_status_t status = likely(!test_page_alloc_func_)
                                 ? pmm_alloc_page(0, &page, &paddr)
                                 : test_page_alloc_func_(0, &page, &paddr);
  if (status != ZX_OK) {
    return zx::error_result(status);
  }
  DEBUG_ASSERT(is_physmap_phys_addr(paddr));

  page->set_state(vm_page_state::MMU);
  pt_pages_++;

  LOCAL_KTRACE("page table alloc");
  LTRACEF("allocated 0x%lx\n", paddr);

  return zx::ok(paddr);
}

void Riscv64ArchVmAspace::FreePageTable(void* vaddr, paddr_t paddr, ConsistencyManager& cm) {
  LTRACEF("vaddr %p paddr 0x%lx\n", vaddr, paddr);
  LOCAL_KTRACE("page table free");

  vm_page_t* const page = paddr_to_vm_page(paddr);
  DEBUG_ASSERT(page != nullptr);
  DEBUG_ASSERT(page->state() == vm_page_state::MMU);

  cm.FreePage(page);
  pt_pages_--;
}

zx_status_t Riscv64ArchVmAspace::SplitLargePage(vaddr_t vaddr, uint level, vaddr_t pt_index,
                                                volatile pte_t* page_table,
                                                ConsistencyManager& cm) {
  LTRACEF("vaddr %#lx, level %u, pt_index %#lx, page_table %p\n", vaddr, level, pt_index,
          page_table);

  const pte_t old_pte = page_table[pt_index];
  DEBUG_ASSERT(riscv64_pte_is_leaf(old_pte));

  LTRACEF("old leaf table entry is %#lx\n", old_pte);

  const zx::result<paddr_t> result = AllocPageTable();
  if (result.is_error()) {
    TRACEF("failed to allocate page table\n");
    return result.error_value();
  }
  const paddr_t paddr = result.value();

  const auto new_page_table = static_cast<volatile pte_t*>(paddr_to_physmap(paddr));

  // Inherit all of the page table entry bits that aren't part of the address.
  const pte_t new_page_attrs = old_pte & ~(RISCV64_PTE_PPN_MASK);

  LTRACEF("new page table filled with attrs %#lx | address\n", new_page_attrs);

  const size_t next_size = page_size_per_level(level - 1);
  for (uint64_t i = 0, mapped_paddr = riscv64_pte_pa(old_pte); i < RISCV64_MMU_PT_ENTRIES;
       i++, mapped_paddr += next_size) {
    // directly write to the pte, no need to update since this is
    // a completely new table
    new_page_table[i] = riscv64_pte_pa_to_pte(mapped_paddr) | new_page_attrs;
  }

  // Ensure page table initialization becomes visible prior to page table installation.
  wmb();

  update_pte(&page_table[pt_index], mmu_non_leaf_pte(paddr, IsKernel()));
  LTRACEF("pte %p[%#" PRIxPTR "] = %#" PRIx64 "\n", page_table, pt_index, page_table[pt_index]);

  // no need to update the page table count here since we're replacing a block entry with a table
  // entry.

  cm.FlushEntry(vaddr, false);

  return ZX_OK;
}

// Use the appropriate TLB flush instruction to globally flush the modified run of pages
// in the appropriate ASID, or across all ASIDs if the run is in the kernel or in a shared aspace.
void Riscv64ArchVmAspace::FlushTLBEntryRun(vaddr_t vaddr, size_t count) const {
  LTRACEF("vaddr %#lx, count %#lx, asid %#hx, kernel %u\n", vaddr, count, asid_, IsKernel());

  kcounter_add(cm_page_run_invalidate, 1);
  kcounter_add(cm_single_page_invalidate, static_cast<int64_t>(count));

  // Future optimization here and FlushAsid() when asids are disabled:
  // Based on which cpu has the aspace active, only send IPIs (either directly
  // or via SBI) to the cores from that list to shoot down TLBs.
  const size_t size = count * PAGE_SIZE;
  if (IsKernel() || IsShared()) {
    SfenceVmaArgs args{.range = SfenceVmaArgs::Range{.base = vaddr, .size = size}};
    mp_sync_exec(MP_IPI_TARGET_ALL, /* cpu_mask */ 0, &SfenceVma, &args);
  } else if (IsUser()) {
    // Flush just the aspace's asid
    SfenceVmaArgs args{.range = SfenceVmaArgs::Range{.base = vaddr, .size = size}, .asid = asid_};
    mp_sync_exec(MP_IPI_TARGET_ALL, /* cpu_mask */ 0, &SfenceVma, &args);
  } else {
    PANIC_UNIMPLEMENTED;
  }
}

// Flush an entire ASID on all cpus.
void Riscv64ArchVmAspace::FlushAsid() const {
  LTRACEF("asid %#hx, kernel %u\n", asid_, IsKernel());

  if (IsKernel() || IsShared()) {
    // Perform a full flush of all cpus across all ASIDs
    SfenceVmaArgs args{};
    mp_sync_exec(MP_IPI_TARGET_ALL, /* cpu_mask */ 0, &SfenceVma, &args);
    kcounter_add(cm_global_invalidate, 1);
  } else {
    // Perform a full flush of all cpus of a single ASID
    SfenceVmaArgs args{.asid = asid_};
    mp_sync_exec(MP_IPI_TARGET_ALL, /* cpu_mask */ 0, &SfenceVma, &args);
    kcounter_add(cm_asid_invalidate, 1);
  }
}

zx::result<size_t> Riscv64ArchVmAspace::UnmapPageTable(vaddr_t vaddr, vaddr_t vaddr_rel,
                                                       size_t size, EnlargeOperation enlarge,
                                                       uint level, volatile pte_t* page_table,
                                                       ConsistencyManager& cm) {
  const vaddr_t block_size = page_size_per_level(level);
  const vaddr_t block_mask = block_size - 1;

  LTRACEF("vaddr 0x%lx, vaddr_rel 0x%lx, size 0x%lx, level %u, page_table %p\n", vaddr, vaddr_rel,
          size, level, page_table);

  size_t unmap_size = 0;
  while (size) {
    const vaddr_t vaddr_rem = vaddr_rel & block_mask;
    const size_t chunk_size = ktl::min(size, block_size - vaddr_rem);
    const vaddr_t index = vaddr_to_index(vaddr_rel, level);

    pte_t pte = page_table[index];

    // If the input range partially covers a large page, attempt to split.
    if (level > 0 && riscv64_pte_is_valid(pte) && riscv64_pte_is_leaf(pte) &&
        chunk_size != block_size) {
      const zx_status_t status = SplitLargePage(vaddr, level, index, page_table, cm);
      // If the split failed then we just fall through and unmap the entire large page.
      if (likely(status == ZX_OK)) {
        pte = page_table[index];
      } else if (enlarge == EnlargeOperation::No) {
        return zx::error_result(status);
      }
    }

    // Check for an inner page table pointer.
    if (level > 0 && riscv64_pte_is_valid(pte) && !riscv64_pte_is_leaf(pte)) {
      const paddr_t page_table_paddr = riscv64_pte_pa(pte);
      volatile pte_t* next_page_table =
          static_cast<volatile pte_t*>(paddr_to_physmap(page_table_paddr));

      // Recurse a level.
      zx::result<size_t> result =
          UnmapPageTable(vaddr, vaddr_rem, chunk_size, enlarge, level - 1, next_page_table, cm);
      if (result.is_error()) {
        return result;
      }

      LTRACEF_LEVEL(2, "exited recursion: back at level %u\n", level);

      // If this is an entry corresponding to a top level kernel page table (MMU_PT_LEVELS - 1),
      // skip freeing it so that we always keep these kernel page tables populated in all address
      // spaces.
      const bool kernel_top_level_pt = (type_ == Riscv64AspaceType::kKernel) &&
                                       (index >= RISCV64_MMU_PT_KERNEL_BASE_INDEX) &&
                                       IsTopLevel(level);
      // Similarly, if this is an entry corresponding to a top level shared page table, skip
      // freeing it as there may be several unified aspaces referencing its contents.
      const bool shared_top_level_pt = IsShared() && IsTopLevel(level);
      if (!kernel_top_level_pt && !shared_top_level_pt &&
          (chunk_size == block_size || page_table_is_clear(next_page_table))) {
        // If we unmapped an entire page table leaf and/or the unmap made the level below us empty,
        // free the page table.
        LTRACEF("pte %p[0x%lx] = 0 (was page table phys %#lx virt %p)\n", page_table, index,
                page_table_paddr, next_page_table);
        update_pte(&page_table[index], 0);
        // If this is a restricted aspace and we are updating the top level page table, we need to
        // update the top level page of the associated unified aspace.
        if (IsTopLevel(level) && IsRestricted() && referenced_aspace_ != nullptr) {
          Guard<Mutex> b{AssertOrderedLock, &referenced_aspace_->lock_,
                         referenced_aspace_->LockOrder()};
          update_pte(&referenced_aspace_->tt_virt_[index], 0);
        }

        // We can safely defer TLB flushing as the consistency manager will not return the backing
        // page to the PMM until after the tlb is flushed.
        cm.FlushEntry(vaddr, false);
        FreePageTable(const_cast<pte_t*>(next_page_table), page_table_paddr, cm);
      }
    } else if (riscv64_pte_is_valid(pte)) {
      // Unmap this leaf page.
      LTRACEF("pte %p[0x%lx] = 0 (was phys %#lx)\n", page_table, index,
              riscv64_pte_pa(page_table[index]));
      update_pte(&page_table[index], 0);

      cm.FlushEntry(vaddr, true);
    } else {
      LTRACEF("pte %p[0x%lx] already clear\n", page_table, index);
    }
    vaddr += chunk_size;
    vaddr_rel += chunk_size;
    size -= chunk_size;
    unmap_size += chunk_size;
  }

  return zx::ok(unmap_size);
}

zx_status_t Riscv64ArchVmAspace::MapPageTable(pte_t attrs, bool ro, uint level,
                                              volatile pte_t* page_table,
                                              ExistingEntryAction existing_action,
                                              MappingCursor& cursor, ConsistencyManager& cm) {
  const vaddr_t block_size = page_size_per_level(level);
  uint index = vaddr_to_index(cursor.vaddr(), level);

  for (; index != RISCV64_MMU_PT_ENTRIES && cursor.size() != 0; ++index) {
    pte_t pte = page_table[index];

    // if we're at an unaligned address, and not trying to map a block larger than 1GB,
    // recurse one more level of the page table tree
    const bool level_valigned = IS_ALIGNED(cursor.vaddr_rel(), block_size);
    const bool level_paligned = IS_ALIGNED(cursor.paddr(), block_size);
    if (!level_valigned || !level_paligned || cursor.PageRemaining() < block_size || (level > 2)) {
      paddr_t page_table_paddr = 0;
      volatile pte_t* next_page_table = nullptr;

      if (!riscv64_pte_is_valid(pte)) {
        zx::result<paddr_t> result = AllocPageTable();
        if (result.is_error()) {
          TRACEF("failed to allocate page table\n");
          // The mapping wasn't fully updated, but there is work here that might need to be undone
          // as we may have allocated various levels of page tables. By consuming a single page we
          // make the cleanup operation think we have added a mapping here, causing it to check
          // the page table for potential cleanup.
          cursor.Consume(PAGE_SIZE);
          return result.status_value();
        }
        page_table_paddr = result.value();
        void* pt_vaddr = paddr_to_physmap(page_table_paddr);

        LTRACEF("allocated page table, vaddr %p, paddr 0x%lx\n", pt_vaddr, page_table_paddr);
        arch_zero_page(pt_vaddr);

        // ensure that the zeroing is observable from hardware page table walkers, as we need to
        // do this prior to writing the pte we cannot defer it using the consistency manager.
        mb();

        pte = mmu_non_leaf_pte(page_table_paddr, IsKernel());
        update_pte(&page_table[index], pte);
        // If this is a restricted aspace and we are mapping into the top level page, we need to
        // add the page table entry to the top level page of the associated unified aspace as well.
        if (IsTopLevel(level) && IsRestricted() && referenced_aspace_ != nullptr) {
          Guard<Mutex> b{AssertOrderedLock, &referenced_aspace_->lock_,
                         referenced_aspace_->LockOrder()};
          update_pte(&referenced_aspace_->tt_virt_[index], pte);
        }
        // We do not need to sync the walker, despite writing a new entry, as this is a
        // non-terminal entry and so is irrelevant to the walker anyway.
        LTRACEF("pte %p[%u] = %#" PRIx64 " (paddr %#lx)\n", page_table, index, pte,
                page_table_paddr);
        next_page_table = static_cast<volatile pte_t*>(pt_vaddr);
      } else if (!riscv64_pte_is_leaf(pte)) {
        page_table_paddr = riscv64_pte_pa(pte);
        LTRACEF("found page table %#" PRIxPTR "\n", page_table_paddr);
        next_page_table = static_cast<volatile pte_t*>(paddr_to_physmap(page_table_paddr));
      } else {
        return ZX_ERR_ALREADY_EXISTS;
      }
      DEBUG_ASSERT(next_page_table);

      zx_status_t result =
          MapPageTable(attrs, ro, level - 1, next_page_table, existing_action, cursor, cm);
      if (result != ZX_OK) {
        return result;
      }
    } else {
      const pte_t new_pte = riscv64_pte_pa_to_pte(cursor.paddr()) | attrs;

      const bool valid = riscv64_pte_is_valid(pte);
      if (unlikely(valid && existing_action == ExistingEntryAction::Error)) {
        LTRACEF("page table entry already in use, index %u, %#" PRIx64 "\n", index, pte);
        return ZX_ERR_ALREADY_EXISTS;
      } else if (valid && existing_action == ExistingEntryAction::Skip) {
        // Empty case to simplify the other branches.
      } else if (valid && existing_action == ExistingEntryAction::Upgrade &&
                 riscv64_pte_pa(pte) == cursor.paddr()) {
        // Doing an upgrade of an existing entry where the output address is not changing. This is
        // just a protect, which we can skip if either nothing is actually changing, or if we would
        // potentially be reducing permissions.
        if (!ro && new_pte != pte) {
          update_pte(&page_table[index], new_pte);
          cm.FlushEntry(cursor.vaddr(), true);
        }
      } else {
        // Either no current entry, or we need to upgrade the existing one, potentially performing
        // a break-before-make.
        if (valid && !ro) {
          // If the output address were not changing we would have hit the protect case above, so if
          // the new entry is not read only then we must perform break-before-make before installing
          // it. Failing to do this could result in writes being temporarily lost due to the
          // different output addresses.
          update_pte(&page_table[index], 0);
          cm.FlushEntry(cursor.vaddr(), true);
          // Must force the flush to happen now before installing the new entry. This will also
          // ensure the page table entries we wrote will be visible before we install it.
          cm.Flush();
        }
        LTRACEF("pte %p[%u] = %#" PRIx64 " (paddr %#lx)\n", page_table, index, pte, cursor.paddr());
        update_pte(&page_table[index], new_pte);

        // Flush the TLB on map as well, unlike most architectures.
        if (IsKernel()) {
          // Normally we only need a local fence here and secondary cpus at worse would only
          // get a spurious page fault. However, since spurious PFs are not tolerated in the
          // kernel we want to do a full flush via the ConsistencyManager for kernel addresses.
          cm.FlushEntry(cursor.vaddr(), true);
        } else if (IsUser()) {
          // Perform a local sfence.vma on the single page in the local asid. If another cpu were
          // to page fault on this user address, it will sfence.vma in its PF handler.
          riscv64_tlb_flush_address_one_asid(cursor.vaddr(), asid_);
          kcounter_add(cm_local_page_invalidate, 1);
        } else [[unlikely]] {
          PANIC_UNIMPLEMENTED;
        }
      }
      cursor.Consume(block_size);
    }
  }
  return ZX_OK;
}

zx_status_t Riscv64ArchVmAspace::ProtectPageTable(vaddr_t vaddr_in, vaddr_t vaddr_rel_in,
                                                  size_t size_in, pte_t attrs, uint level,
                                                  volatile pte_t* page_table,
                                                  ConsistencyManager& cm) {
  vaddr_t vaddr = vaddr_in;
  vaddr_t vaddr_rel = vaddr_rel_in;
  size_t size = size_in;

  const vaddr_t block_size = page_size_per_level(level);
  const vaddr_t block_mask = block_size - 1;

  LTRACEF("vaddr %#" PRIxPTR ", vaddr_rel %#" PRIxPTR ", size %#" PRIxPTR ", attrs %#" PRIx64
          ", level %u, page_table %p\n",
          vaddr, vaddr_rel, size, attrs, level, page_table);

  // vaddr_rel and size must be page aligned
  DEBUG_ASSERT(((vaddr_rel | size) & ((1UL << PAGE_SIZE_SHIFT) - 1)) == 0);

  while (size) {
    const vaddr_t vaddr_rem = vaddr_rel & block_mask;
    const size_t chunk_size = ktl::min(size, block_size - vaddr_rem);
    const vaddr_t index = vaddr_to_index(vaddr_rel, level);
    pte_t pte = page_table[index];

    // If the input range partially covers a large page, split the page.
    if (level > 0 && riscv64_pte_is_valid(pte) && riscv64_pte_is_leaf(pte) &&
        chunk_size != block_size) {
      zx_status_t s = SplitLargePage(vaddr, level, index, page_table, cm);
      if (unlikely(s != ZX_OK)) {
        return s;
      }
      pte = page_table[index];
    }

    if (level > 0 && riscv64_pte_is_valid(pte) && !riscv64_pte_is_leaf(pte)) {
      const paddr_t page_table_paddr = riscv64_pte_pa(pte);
      volatile pte_t* next_page_table =
          static_cast<volatile pte_t*>(paddr_to_physmap(page_table_paddr));

      // Recurse a level
      zx_status_t status =
          ProtectPageTable(vaddr, vaddr_rem, chunk_size, attrs, level - 1, next_page_table, cm);
      if (unlikely(status != ZX_OK)) {
        return status;
      }
    } else if (riscv64_pte_is_valid(pte)) {
      const pte_t new_pte = (pte & ~RISCV64_PTE_PERM_MASK) | attrs;

      LTRACEF("pte %p[%#" PRIxPTR "] = %#" PRIx64 " was %#" PRIx64 "\n", page_table, index, new_pte,
              pte);

      // Skip updating the page table entry if the new value is the same as before.
      if (new_pte != pte) {
        update_pte(&page_table[index], new_pte);

        cm.FlushEntry(vaddr, true);
      }
    } else {
      LTRACEF("page table entry does not exist, index %#" PRIxPTR ", %#" PRIx64 "\n", index, pte);
    }
    vaddr += chunk_size;
    vaddr_rel += chunk_size;
    size -= chunk_size;
  }

  return ZX_OK;
}

void Riscv64ArchVmAspace::HarvestAccessedPageTable(vaddr_t vaddr, vaddr_t vaddr_rel_in, size_t size,
                                                   uint level,
                                                   NonTerminalAction non_terminal_action,
                                                   TerminalAction terminal_action,
                                                   volatile pte_t* page_table,
                                                   ConsistencyManager& cm, bool* unmapped_out) {
  const vaddr_t block_size = page_size_per_level(level);
  const vaddr_t block_mask = block_size - 1;

  vaddr_t vaddr_rel = vaddr_rel_in;

  LTRACEF("vaddr 0x%lx, vaddr_rel 0x%lx, size 0x%lx, level %u, page_table %p\n", vaddr, vaddr_rel,
          size, level, page_table);

  // vaddr_rel and size must be page aligned
  DEBUG_ASSERT(((vaddr_rel | size) & ((1UL << PAGE_SIZE_SHIFT) - 1)) == 0);

  while (size) {
    const vaddr_t vaddr_rem = vaddr_rel & block_mask;
    const size_t chunk_size = ktl::min(size, block_size - vaddr_rem);
    const vaddr_t index = vaddr_to_index(vaddr_rel, level);

    pte_t pte = page_table[index];

    if (level > 0 && riscv64_pte_is_valid(pte) && riscv64_pte_is_leaf(pte) &&
        chunk_size != block_size) {
      // Ignore large pages, we do not support harvesting accessed bits from them. Having this empty
      // if block simplifies the overall logic.
    } else if (level > 0 && riscv64_pte_is_valid(pte) && !riscv64_pte_is_leaf(pte)) {
      // We're at an inner page table pointer node.
      const paddr_t page_table_paddr = riscv64_pte_pa(pte);
      volatile pte_t* next_page_table =
          static_cast<volatile pte_t*>(paddr_to_physmap(page_table_paddr));

      // NOTE: We currently cannot honor NonTerminalAction::FreeUnaccessed since accessed
      // information is not being tracked on inner nodes.

      // Recurse into the next level
      HarvestAccessedPageTable(vaddr, vaddr_rel, chunk_size, level - 1, non_terminal_action,
                               terminal_action, next_page_table, cm, unmapped_out);
    } else if (riscv64_pte_is_valid(pte) && (pte & RISCV64_PTE_A)) {
      const paddr_t pte_addr = riscv64_pte_pa(pte);
      const paddr_t paddr = pte_addr + vaddr_rem;

      vm_page_t* page = paddr_to_vm_page(paddr);
      // Mappings for physical VMOs do not have pages associated with them and so there's no state
      // to update on an access.
      if (likely(page)) {
        pmm_page_queues()->MarkAccessedDeferredCount(page);

        if (terminal_action == TerminalAction::UpdateAgeAndHarvest) {
          // Modifying the access flag does not require break-before-make for correctness and as we
          // do not support hardware access flag setting at the moment we do not have to deal with
          // potential concurrent modifications.
          pte = (pte & ~RISCV64_PTE_A);
          LTRACEF("pte %p[%#" PRIxPTR "] = %#" PRIx64 "\n", page_table, index, pte);
          update_pte(&page_table[index], pte);

          cm.FlushEntry(vaddr, true);
        }
      }
    }
    vaddr += chunk_size;
    vaddr_rel += chunk_size;
    size -= chunk_size;
  }
}

void Riscv64ArchVmAspace::MarkAccessedPageTable(vaddr_t vaddr, vaddr_t vaddr_rel_in, size_t size,
                                                uint level, volatile pte_t* page_table,
                                                ConsistencyManager& cm) {
  const vaddr_t block_size = page_size_per_level(level);
  const vaddr_t block_mask = block_size - 1;

  vaddr_t vaddr_rel = vaddr_rel_in;

  LTRACEF("vaddr 0x%lx, vaddr_rel 0x%lx, size 0x%lx, level %u, page_table %p\n", vaddr, vaddr_rel,
          size, level, page_table);

  // vaddr_rel and size must be page aligned
  DEBUG_ASSERT(((vaddr_rel | size) & ((1UL << PAGE_SIZE_SHIFT) - 1)) == 0);

  while (size) {
    const vaddr_t vaddr_rem = vaddr_rel & block_mask;
    const size_t chunk_size = ktl::min(size, block_size - vaddr_rem);
    const vaddr_t index = vaddr_to_index(vaddr_rel, level);

    pte_t pte = page_table[index];

    if (level > 0 && riscv64_pte_is_valid(pte) && riscv64_pte_is_leaf(pte) &&
        chunk_size != block_size) {
      // Ignore large pages as we don't support modifying their access flags. Having this empty if
      // block simplifies the overall logic.
    } else if (level > 0 && riscv64_pte_is_valid(pte) && !riscv64_pte_is_leaf(pte)) {
      const paddr_t page_table_paddr = riscv64_pte_pa(pte);
      volatile pte_t* next_page_table =
          static_cast<volatile pte_t*>(paddr_to_physmap(page_table_paddr));
      MarkAccessedPageTable(vaddr, vaddr_rem, chunk_size, level - 1, next_page_table, cm);
    } else if (riscv64_pte_is_valid(pte)) {
      pte |= RISCV64_PTE_A;
      update_pte(&page_table[index], pte);
    }
    vaddr += chunk_size;
    vaddr_rel += chunk_size;
    size -= chunk_size;
  }
}

zx::result<size_t> Riscv64ArchVmAspace::UnmapPages(vaddr_t vaddr, size_t size,
                                                   EnlargeOperation enlarge,
                                                   ConsistencyManager& cm) {
  LOCAL_KTRACE("mmu unmap", (vaddr & ~PAGE_MASK) | ((size >> PAGE_SIZE_SHIFT) & PAGE_MASK));
  uint level = RISCV64_MMU_PT_LEVELS - 1;
  return UnmapPageTable(vaddr, vaddr, size, enlarge, level, tt_virt_, cm);
}

zx_status_t Riscv64ArchVmAspace::ProtectPages(vaddr_t vaddr, size_t size, pte_t attrs) {
  LOCAL_KTRACE("mmu protect", (vaddr & ~PAGE_MASK) | ((size >> PAGE_SIZE_SHIFT) & PAGE_MASK));
  uint level = RISCV64_MMU_PT_LEVELS - 1;
  ConsistencyManager cm(*this);
  return ProtectPageTable(vaddr, vaddr, size, attrs, level, tt_virt_, cm);
}

zx_status_t Riscv64ArchVmAspace::MapContiguous(vaddr_t vaddr, paddr_t paddr, size_t count,
                                               uint mmu_flags, size_t* mapped) {
  canary_.Assert();
  LTRACEF("vaddr %#" PRIxPTR " paddr %#" PRIxPTR " count %zu flags %#x\n", vaddr, paddr, count,
          mmu_flags);

  DEBUG_ASSERT(tt_virt_);

  DEBUG_ASSERT(IsValidVaddr(vaddr));
  if (!IsValidVaddr(vaddr)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!(mmu_flags & ARCH_MMU_FLAG_PERM_READ)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // paddr and vaddr must be aligned.
  DEBUG_ASSERT(IS_PAGE_ALIGNED(vaddr));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(paddr));
  if (!IS_PAGE_ALIGNED(vaddr) || !IS_PAGE_ALIGNED(paddr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (count == 0) {
    return ZX_OK;
  }

  Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};

  if (mmu_flags & ARCH_MMU_FLAG_PERM_EXECUTE) {
    Riscv64VmICacheConsistencyManager cache_cm;
    cache_cm.SyncAddr(reinterpret_cast<vaddr_t>(paddr_to_physmap(paddr)), count * PAGE_SIZE);
  }

  ConsistencyManager cm(*this);
  const pte_t attrs = mmu_flags_to_pte_attr(mmu_flags, IsKernel());
  MappingCursor cursor(/*paddrs=*/&paddr, /*paddr_count=*/1, /*page_size=*/count * PAGE_SIZE,
                       /*vaddr=*/vaddr);
  const bool ro = (mmu_flags & ARCH_MMU_FLAG_PERM_RWX_MASK) == ARCH_MMU_FLAG_PERM_READ;
  zx_status_t result = MapPageTable(attrs, ro, RISCV64_MMU_PT_LEVELS - 1, tt_virt_,
                                    ExistingEntryAction::Error, cursor, cm);
  mb();
  if (result != ZX_OK) {
    if (cursor.vaddr() > vaddr) {
      zx::result<size_t> unmap_result =
          UnmapPages(vaddr, cursor.vaddr() - vaddr, EnlargeOperation::No, cm);
      ASSERT(unmap_result.is_ok());
    }
    return result;
  }
  DEBUG_ASSERT(cursor.size() == 0);

  if (mapped) {
    *mapped = count;
  }

  return ZX_OK;
}

zx_status_t Riscv64ArchVmAspace::Map(vaddr_t vaddr, paddr_t* phys, size_t count, uint mmu_flags,
                                     ExistingEntryAction existing_action, size_t* mapped) {
  canary_.Assert();

  DEBUG_ASSERT(tt_virt_);

  DEBUG_ASSERT(IsValidVaddr(vaddr));
  if (!IsValidVaddr(vaddr)) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  for (size_t i = 0; i < count; ++i) {
    DEBUG_ASSERT(IS_PAGE_ALIGNED(phys[i]));
    if (!IS_PAGE_ALIGNED(phys[i])) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  if (!(mmu_flags & ARCH_MMU_FLAG_PERM_READ)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // vaddr must be aligned.
  DEBUG_ASSERT(IS_PAGE_ALIGNED(vaddr));
  if (!IS_PAGE_ALIGNED(vaddr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (count == 0) {
    return ZX_OK;
  }

  {
    Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
    if (mmu_flags & ARCH_MMU_FLAG_PERM_EXECUTE) {
      Riscv64VmICacheConsistencyManager cache_cm;
      for (size_t idx = 0; idx < count; ++idx) {
        cache_cm.SyncAddr(reinterpret_cast<vaddr_t>(paddr_to_physmap(phys[idx])), PAGE_SIZE);
      }
    }

    ConsistencyManager cm(*this);
    const pte_t attrs = mmu_flags_to_pte_attr(mmu_flags, IsKernel());
    MappingCursor cursor(/*paddrs=*/phys, /*paddr_count=*/count, /*page_size=*/PAGE_SIZE,
                         /*vaddr=*/vaddr);
    const bool ro = (mmu_flags & ARCH_MMU_FLAG_PERM_RWX_MASK) == ARCH_MMU_FLAG_PERM_READ;
    zx_status_t result =
        MapPageTable(attrs, ro, RISCV64_MMU_PT_LEVELS - 1, tt_virt_, existing_action, cursor, cm);
    mb();
    if (result != ZX_OK) {
      if (cursor.vaddr() > vaddr) {
        zx::result<size_t> unmap_result =
            UnmapPages(vaddr, cursor.vaddr() - vaddr, EnlargeOperation::No, cm);
        ASSERT(unmap_result.is_ok());
      }
      return result;
    }
  }

  if (mapped) {
    // For ExistingEntryAction::Error, we should have mapped all the addresses we were asked to.
    // For ExistingEntryAction::Skip, we might have mapped less if we encountered existing entries,
    // but skipped entries contribute towards the total as well.
    *mapped = count;
  }

  return ZX_OK;
}

zx_status_t Riscv64ArchVmAspace::Unmap(vaddr_t vaddr, size_t count, EnlargeOperation enlarge,
                                       size_t* unmapped) {
  canary_.Assert();
  LTRACEF("vaddr %#" PRIxPTR " count %zu\n", vaddr, count);

  DEBUG_ASSERT(tt_virt_);

  DEBUG_ASSERT(IsValidVaddr(vaddr));

  if (!IsValidVaddr(vaddr)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  DEBUG_ASSERT(IS_PAGE_ALIGNED(vaddr));
  if (!IS_PAGE_ALIGNED(vaddr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};

  ConsistencyManager cm(*this);
  zx::result<size_t> result = UnmapPages(vaddr, count * PAGE_SIZE, enlarge, cm);

  if (unmapped) {
    *unmapped = result.is_ok() ? result.value() / PAGE_SIZE : 0u;
    DEBUG_ASSERT(*unmapped <= count);
  }

  return result.status_value();
}

zx_status_t Riscv64ArchVmAspace::Protect(vaddr_t vaddr, size_t count, uint mmu_flags,
                                         EnlargeOperation enlarge) {
  canary_.Assert();

  if (!IsValidVaddr(vaddr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!IS_PAGE_ALIGNED(vaddr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!(mmu_flags & ARCH_MMU_FLAG_PERM_READ)) {
    return ZX_ERR_INVALID_ARGS;
  }

  Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
  if (mmu_flags & ARCH_MMU_FLAG_PERM_EXECUTE) {
    // If mappings are going to become executable then we first need to sync their caches.
    // Unfortunately this needs to be done on kernel virtual addresses to avoid taking translation
    // faults, and so we need to first query for the physical address to then get the kernel virtual
    // address in the physmap.
    // This sync could be more deeply integrated into ProtectPages, but making existing regions
    // executable is very uncommon operation and so we keep it simple.
    vm_mmu_protect_make_execute_calls.Add(1);
    Riscv64VmICacheConsistencyManager cache_cm;
    size_t pages_synced = 0;
    for (size_t idx = 0; idx < count; idx++) {
      paddr_t paddr;
      uint flags;
      if (QueryLocked(vaddr + idx * PAGE_SIZE, &paddr, &flags) == ZX_OK &&
          (flags & ARCH_MMU_FLAG_PERM_EXECUTE)) {
        cache_cm.SyncAddr(reinterpret_cast<vaddr_t>(paddr_to_physmap(paddr)), PAGE_SIZE);
        pages_synced++;
      }
    }
    vm_mmu_protect_make_execute_pages.Add(pages_synced);
  }

  const pte_t attrs = mmu_flags_to_pte_attr(mmu_flags, IsKernel());
  return ProtectPages(vaddr, count * PAGE_SIZE, attrs);
}

zx_status_t Riscv64ArchVmAspace::HarvestAccessed(vaddr_t vaddr, size_t count,
                                                 NonTerminalAction non_terminal,
                                                 TerminalAction terminal) {
  canary_.Assert();
  LTRACEF("vaddr %#" PRIxPTR " count %zu\n", vaddr, count);

  if (!IS_PAGE_ALIGNED(vaddr) || !IsValidVaddr(vaddr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  Guard<Mutex> guard{AssertOrderedLock, &lock_, LockOrder()};

  const size_t size = count * PAGE_SIZE;
  LOCAL_KTRACE("mmu harvest accessed",
               (vaddr & ~PAGE_MASK) | ((size >> PAGE_SIZE_SHIFT) & PAGE_MASK));

  ConsistencyManager cm(*this);

  HarvestAccessedPageTable(vaddr, vaddr, size, RISCV64_MMU_PT_LEVELS - 1, non_terminal, terminal,
                           tt_virt_, cm, nullptr);
  return ZX_OK;
}

zx_status_t Riscv64ArchVmAspace::MarkAccessed(vaddr_t vaddr, size_t count) {
  canary_.Assert();
  LTRACEF("vaddr %#" PRIxPTR " count %zu\n", vaddr, count);

  if (!IS_PAGE_ALIGNED(vaddr) || !IsValidVaddr(vaddr)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};

  const size_t size = count * PAGE_SIZE;
  LOCAL_KTRACE("mmu mark accessed", (vaddr & ~PAGE_MASK) | ((size >> PAGE_SIZE_SHIFT) & PAGE_MASK));

  ConsistencyManager cm(*this);

  MarkAccessedPageTable(vaddr, vaddr, size, RISCV64_MMU_PT_LEVELS - 1, tt_virt_, cm);

  return ZX_OK;
}

bool Riscv64ArchVmAspace::ActiveSinceLastCheck(bool clear) {
  // Read whether any CPUs are presently executing.
  bool currently_active = num_active_cpus_.load(ktl::memory_order_relaxed) != 0;
  // Exchange the current notion of active, with the previously active information. This is the only
  // time a |false| value can potentially be written to active_since_last_check_, and doing an
  // exchange means we can never 'lose' a |true| value.
  bool previously_active =
      clear ? active_since_last_check_.exchange(currently_active, ktl::memory_order_relaxed)
            : active_since_last_check_.load(ktl::memory_order_relaxed);
  // Return whether we had previously been active. It is not necessary to also consider whether we
  // are currently active, since activating would also have active_since_last_check_ to true. In the
  // scenario where we race and currently_active is true, but we observe previously_active to be
  // false, this means that as of the start of this function ::ContextSwitch had not completed, and
  // so this aspace is still not actually active.
  return previously_active;
}

zx_status_t Riscv64ArchVmAspace::Init() {
  canary_.Assert();
  LTRACEF("aspace %p, base %#" PRIxPTR ", size 0x%zx, type %*s\n", this, base_, size_,
          static_cast<int>(Riscv64AspaceTypeName(type_).size()),
          Riscv64AspaceTypeName(type_).data());

  Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};

  // Validate that the base + size is valid and doesn't wrap.
  DEBUG_ASSERT(size_ > 0 || IsUnified());
  DEBUG_ASSERT(IS_PAGE_ALIGNED(base_));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(size_));
  [[maybe_unused]] uintptr_t unused;
  DEBUG_ASSERT(!add_overflow(base_, size_ - 1, &unused));

  if (type_ == Riscv64AspaceType::kKernel) {
    // At the moment we can only deal with address spaces as globally defined.
    DEBUG_ASSERT(base_ == KERNEL_ASPACE_BASE);
    DEBUG_ASSERT(size_ == KERNEL_ASPACE_SIZE);

    tt_virt_ = riscv64_kernel_translation_table;
    tt_phys_ = kernel_virt_to_phys(riscv64_kernel_translation_table);
    asid_ = kernel_asid();
  } else {
    if (type_ == Riscv64AspaceType::kUser) {
      DEBUG_ASSERT_MSG(IsUnified() || IsUserBaseSizeValid(base_, size_),
                       "base %#" PRIxPTR " size 0x%zx", base_, size_);
      if (!IsUserBaseSizeValid(base_, size_) && !IsUnified()) {
        return ZX_ERR_INVALID_ARGS;
      }

      // If using asids, assign a unique asid per process. If not, set the UNUSED
      // asid to this address space, which will be the same between all aspaces.
      if (riscv_use_asid) {
        auto status = asid_allocator.Alloc();
        if (status.is_error()) {
          printf("RISC-V: out of ASIDs!\n");
          return status.status_value();
        }
        asid_ = status.value();
      } else {
        asid_ = MMU_RISCV64_UNUSED_ASID;
      }
    } else {
      return ZX_ERR_NOT_SUPPORTED;
    }

    // allocate a top level page table to serve as the translation table
    const zx::result<paddr_t> result = AllocPageTable();
    if (result.is_error()) {
      return result.error_value();
    }
    const paddr_t pa = result.value();

    volatile pte_t* va = static_cast<volatile pte_t*>(paddr_to_physmap(pa));
    tt_virt_ = va;
    tt_phys_ = pa;

    // zero the top level translation table and copy the kernel memory mapping.
    memset((void*)tt_virt_, 0, PAGE_SIZE / 2);
    memcpy((void*)(tt_virt_ + RISCV64_MMU_PT_ENTRIES / 2),
           (void*)(riscv64_kernel_translation_table + RISCV64_MMU_PT_ENTRIES / 2), PAGE_SIZE / 2);
  }
  pt_pages_ = 1;

  LTRACEF("tt_phys %#" PRIxPTR " tt_virt %p\n", tt_phys_, tt_virt_);

  return ZX_OK;
}

zx_status_t Riscv64ArchVmAspace::InitRestricted() {
  role_ = Riscv64AspaceRole::kRestricted;
  return Init();
}

zx_status_t Riscv64ArchVmAspace::InitShared() {
  role_ = Riscv64AspaceRole::kShared;
  zx_status_t status = Init();
  if (status != ZX_OK) {
    return status;
  }

  Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};

  // Prepopulate the portion of the top level page table spanned by this aspace by allocating the
  // necessary second level entries.
  const uint top_level = RISCV64_MMU_PT_LEVELS - 1;
  const uint start = vaddr_to_index(base_, top_level);
  const uint end = vaddr_to_index(base_ + size_, top_level) - 1;
  for (uint i = start; i <= end; i++) {
    zx::result<paddr_t> result = AllocPageTable();
    if (result.is_error()) {
      LTRACEF("failed to allocate second level page table for shared aspace\n");
      return result.error_value();
    }
    paddr_t page_table_paddr = result.value();
    void* pt_vaddr = paddr_to_physmap(page_table_paddr);
    arch_zero_page(pt_vaddr);

    // Ensure that the zeroing is observable from hardware page table walkers, as we need to
    // do this prior to writing the pte we cannot defer it using the consistency manager.
    mb();

    pte_t pte = mmu_non_leaf_pte(page_table_paddr, false);
    update_pte(&tt_virt_[i], pte);
  }
  return ZX_OK;
}

zx_status_t Riscv64ArchVmAspace::InitUnified(ArchVmAspaceInterface& s, ArchVmAspaceInterface& r) {
  canary_.Assert();

  // The base_ and size_ of a unified aspace are expected to be zero.
  DEBUG_ASSERT(size_ == 0);
  DEBUG_ASSERT(base_ == 0);

  role_ = Riscv64AspaceRole::kUnified;
  zx_status_t status = Init();
  if (status != ZX_OK) {
    return status;
  }
  Riscv64ArchVmAspace& shared = static_cast<Riscv64ArchVmAspace&>(s);
  Riscv64ArchVmAspace& restricted = static_cast<Riscv64ArchVmAspace&>(r);
  {
    Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
    referenced_aspace_ = &restricted;
    shared_aspace_ = &shared;
  }

  const uint top_level = RISCV64_MMU_PT_LEVELS - 1;
  const uint restricted_start = vaddr_to_index(restricted.base_, top_level);
  const uint restricted_end = vaddr_to_index(restricted.base_ + restricted.size_, top_level) - 1;
  const uint shared_start = vaddr_to_index(shared.base_, top_level);
  const uint shared_end = vaddr_to_index(shared.base_ + shared.size_, top_level) - 1;
  DEBUG_ASSERT(restricted_end < shared_start);

  // Validate that the restricted aspace is empty and set its metadata.
  {
    Guard<Mutex> a{AssertOrderedLock, &restricted.lock_, restricted.LockOrder()};
    DEBUG_ASSERT(restricted.tt_virt_);
    DEBUG_ASSERT(restricted.IsRestricted());
    DEBUG_ASSERT(restricted.num_references_ == 0);
    DEBUG_ASSERT(restricted.referenced_aspace_ == nullptr);
    for (uint i = restricted_start; i <= restricted_end; i++) {
      DEBUG_ASSERT(restricted.tt_virt_[i] == 0);
    }
    restricted.num_references_++;
    restricted.referenced_aspace_ = this;
  }

  // Copy all mappings from the shared aspace and set its metadata.
  {
    Guard<Mutex> a{AssertOrderedLock, &shared.lock_, shared.LockOrder()};
    DEBUG_ASSERT(shared.tt_virt_);
    DEBUG_ASSERT(shared.IsShared());
    for (uint i = shared_start; i <= shared_end; i++) {
      tt_virt_[i] = shared.tt_virt_[i];
    }
    shared.num_references_++;
  }
  return ZX_OK;
}

void Riscv64ArchVmAspace::DisableUpdates() {
  // TODO-rvbringup: add machinery for this and the update checker logic
}

void Riscv64ArchVmAspace::FreeTopLevelPage() {
  vm_page_t* page = paddr_to_vm_page(tt_phys_);
  DEBUG_ASSERT(page);
  pmm_free_page(page);
  pt_pages_--;

  tt_phys_ = 0;
  tt_virt_ = nullptr;
}

zx_status_t Riscv64ArchVmAspace::Destroy() {
  canary_.Assert();
  LTRACEF("aspace %p\n", this);

  // Not okay to destroy the kernel address space.
  DEBUG_ASSERT(type_ != Riscv64AspaceType::kKernel);

  if (IsUnified()) {
    return DestroyUnified();
  }
  return DestroyIndividual();
}

zx_status_t Riscv64ArchVmAspace::DestroyUnified() {
  DEBUG_ASSERT(IsUnified());

  Riscv64ArchVmAspace* restricted = nullptr;
  Riscv64ArchVmAspace* shared = nullptr;
  {
    Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
    restricted = referenced_aspace_;
    shared = shared_aspace_;
    shared_aspace_ = nullptr;
    referenced_aspace_ = nullptr;
  }
  {
    Guard<Mutex> a{AssertOrderedLock, &shared->lock_, shared->LockOrder()};
    // The shared page table should be referenced by at least this page table, and could be
    // referenced by many other unified page tables.
    DEBUG_ASSERT(shared->num_references_ > 0);
    shared->num_references_--;
  }
  {
    Guard<Mutex> a{AssertOrderedLock, &restricted->lock_, restricted->LockOrder()};
    // The restricted_aspace_ page table can only be referenced by a singular unified page table.
    DEBUG_ASSERT(restricted->num_references_ == 1);
    restricted->num_references_--;
  }

  Guard<Mutex> a{AssertOrderedLock, &lock_, LockOrder()};
  if (riscv_use_asid) {
    // Flush the ASID associated with this aspace
    FlushAsid();

    // Free any ASID.
    auto status = asid_allocator.Free(asid_);
    ASSERT(status.is_ok());
    asid_ = MMU_RISCV64_UNUSED_ASID;
  }

  FreeTopLevelPage();
  return ZX_OK;
}

zx_status_t Riscv64ArchVmAspace::DestroyIndividual() {
  DEBUG_ASSERT(!IsUnified());
  Guard<Mutex> guard{AssertOrderedLock, &lock_, LockOrder()};
  DEBUG_ASSERT(num_references_ == 0);

  // If this is a shared aspace, its top level page table was statically prepopulated. Therefore,
  // we need to clean up all of the entries manually here.
  if (IsShared()) {
    const uint top_level = RISCV64_MMU_PT_LEVELS - 1;
    const uint start = vaddr_to_index(base_, top_level);
    const uint end = vaddr_to_index(base_ + size_, top_level) - 1;
    for (uint i = start; i <= end; i++) {
      const paddr_t page_table_paddr = riscv64_pte_pa(tt_virt_[i]);
      pmm_free_page(paddr_to_vm_page(page_table_paddr));
      pt_pages_--;
      update_pte(&tt_virt_[i], 0);
    }
  }

  // Check to see if the top level page table is empty. If not the user didn't
  // properly unmap everything before destroying the aspace.
  const zx::result<size_t> index_result = first_used_page_table_entry(tt_virt_);
  DEBUG_ASSERT_MSG(
      index_result.is_error() || *index_result < (1 << (PAGE_SIZE_SHIFT - 2)),
      "Top level page table still in use: aspace %p tt_virt %p index %zu entry %" PRIx64, this,
      tt_virt_, *index_result,
      *index_result < (1 << (PAGE_SIZE_SHIFT - 2)) ? tt_virt_[*index_result] : 0);
  DEBUG_ASSERT_MSG(pt_pages_ == 1, "Too many page table pages: aspace %p pt_pages_ %zu", this,
                   pt_pages_);

  if (riscv_use_asid) {
    // Flush the ASID associated with this aspace
    FlushAsid();

    // Free any ASID.
    auto status = asid_allocator.Free(asid_);
    ASSERT(status.is_ok());
    asid_ = MMU_RISCV64_UNUSED_ASID;
  }

  // Free the top level page table
  FreeTopLevelPage();
  return ZX_OK;
}

// Called during context switches between threads with different address spaces. Swaps the
// mmu context on hardware. Assumes old_aspace != aspace and optimizes as such.
void Riscv64ArchVmAspace::ContextSwitch(Riscv64ArchVmAspace* old_aspace,
                                        Riscv64ArchVmAspace* aspace) {
  uint64_t satp;

  if (likely(aspace)) {
    aspace->canary_.Assert();
    DEBUG_ASSERT(aspace->type_ == Riscv64AspaceType::kUser);

    // Load the user space SATP with the translation table and user space ASID.
    satp = (RISCV64_SATP_MODE_SV39 << RISCV64_SATP_MODE_SHIFT) |
           (static_cast<uint64_t>(aspace->asid_) << RISCV64_SATP_ASID_SHIFT) |
           (aspace->tt_phys_ >> PAGE_SIZE_SHIFT);

    [[maybe_unused]] uint32_t prev =
        aspace->num_active_cpus_.fetch_add(1, ktl::memory_order_relaxed);
    DEBUG_ASSERT(prev < SMP_MAX_CPUS);
    aspace->active_since_last_check_.store(true, ktl::memory_order_relaxed);
    // If the aspace we are context switching to is unified, we need to mark the associated shared
    // and restricted aspaces as active since we may access their mappings indirectly.
    if (aspace->IsUnified()) {
      aspace->get_shared_aspace()->active_since_last_check_.store(true, ktl::memory_order_relaxed);
      aspace->get_restricted_aspace()->active_since_last_check_.store(true,
                                                                      ktl::memory_order_relaxed);
    }
  } else {
    // Switching to the null aspace, which means kernel address space only.
    satp = (RISCV64_SATP_MODE_SV39 << RISCV64_SATP_MODE_SHIFT) |
           (static_cast<uint64_t>(kernel_asid()) << RISCV64_SATP_ASID_SHIFT) |
           (kernel_virt_to_phys(riscv64_kernel_translation_table) >> PAGE_SIZE_SHIFT);
  }
  if (likely(old_aspace != nullptr)) {
    [[maybe_unused]] uint32_t prev =
        old_aspace->num_active_cpus_.fetch_sub(1, ktl::memory_order_relaxed);
    DEBUG_ASSERT(prev > 0);
  }
  if (TRACE_CONTEXT_SWITCH) {
    TRACEF("old aspace %p aspace %p satp %#" PRIx64 "\n", old_aspace, aspace, satp);
  }

  riscv64_csr_write(RISCV64_CSR_SATP, satp);
  mb();

  // If we're not using hardware features, flush all non global TLB entries on context switch.
  if (!riscv_use_asid) {
    riscv64_tlb_flush_asid(MMU_RISCV64_UNUSED_ASID);
  }
}

Riscv64ArchVmAspace::Riscv64ArchVmAspace(vaddr_t base, size_t size, Riscv64AspaceType type,
                                         page_alloc_fn_t paf)
    : test_page_alloc_func_(paf), type_(type), base_(base), size_(size) {}

Riscv64ArchVmAspace::Riscv64ArchVmAspace(vaddr_t base, size_t size, uint mmu_flags,
                                         page_alloc_fn_t paf)
    : Riscv64ArchVmAspace(base, size, AspaceTypeFromFlags(mmu_flags), paf) {}

Riscv64ArchVmAspace::~Riscv64ArchVmAspace() {
  // Destroy() will have freed the final page table if it ran correctly, and further validated that
  // everything else was freed.
  DEBUG_ASSERT(pt_pages_ == 0);
}

vaddr_t Riscv64ArchVmAspace::PickSpot(vaddr_t base, vaddr_t end, vaddr_t align, size_t size,
                                      uint mmu_flags) {
  canary_.Assert();
  return PAGE_ALIGN(base);
}

void Riscv64ArchVmAspace::HandoffPageTablesFromPhysboot(list_node_t* mmu_pages) {
  while (list_node_t* node = list_remove_head(mmu_pages)) {
    vm_page_t* page = reinterpret_cast<vm_page_t*>(node);
    page->set_state(vm_page_state::MMU);

    // TODO(https://fxbug.dev/42164859): Populate vm_page_t::mmu.
  }
}

void riscv64_mmu_early_init() {
  // Figure out the number of supported ASID bits by writing all 1s to
  // the asid field in satp and seeing which ones 'stick'.
  auto satp_orig = riscv64_csr_read(satp);
  auto satp = satp_orig | (RISCV64_SATP_ASID_MASK << RISCV64_SATP_ASID_SHIFT);
  riscv64_csr_write(satp, satp);
  riscv_asid_mask = (riscv64_csr_read(satp) >> RISCV64_SATP_ASID_SHIFT) & RISCV64_SATP_ASID_MASK;
  riscv64_csr_write(satp, satp_orig);

  paddr_t bootstrap_root_paddr = (satp & RISCV64_SATP_PPN_MASK) << PAGE_SIZE_SHIFT;
  pte_t* bootstrap_translation_table =
      reinterpret_cast<pte_t*>(paddr_to_physmap(bootstrap_root_paddr));

  // Fill in all of the unused top level page table pointers for the kernel half of the kernel
  // top level table. These entries will be copied to all new address spaces, thus ensuring the
  // top level entries are synchronized.
  for (size_t i = RISCV64_MMU_PT_KERNEL_BASE_INDEX; i < RISCV64_MMU_PT_ENTRIES; i++) {
    if (!riscv64_pte_is_valid(bootstrap_translation_table[i])) {
      paddr_t pt_paddr = kernel_virt_to_phys(
          riscv64_kernel_top_level_page_tables[i - RISCV64_MMU_PT_KERNEL_BASE_INDEX]);

      LTRACEF("RISCV: MMU allocating top level page table for slot %zu, pa %#lx\n", i, pt_paddr);

      pte_t pte = mmu_non_leaf_pte(pt_paddr, true);
      update_pte(&bootstrap_translation_table[i], pte);
    }
  }

  // Make a copy of our bootstrap table with the identity map present in the user part.
  memcpy(riscv64_kernel_translation_table, bootstrap_translation_table, PAGE_SIZE);

  // Zero the bottom of the kernel page table to remove any left over boot mappings.
  memset(riscv64_kernel_translation_table, 0, PAGE_SIZE / 2);

  // Make sure it's visible to the cpu
  wmb();
}

namespace {

// Load the kernel page tables and set the passed in asid
void riscv64_switch_kernel_asid(uint16_t asid) {
  const uint64_t satp = (RISCV64_SATP_MODE_SV39 << RISCV64_SATP_MODE_SHIFT) |
                        (static_cast<uint64_t>(asid) << RISCV64_SATP_ASID_SHIFT) |
                        (kernel_virt_to_phys(riscv64_kernel_translation_table) >> PAGE_SIZE_SHIFT);
  riscv64_csr_write(RISCV64_CSR_SATP, satp);

  // Globally TLB flush.
  riscv64_tlb_flush_all();
}

}  // anonymous namespace

void riscv64_mmu_early_init_percpu() {
  // Switch to the proper kernel translation table.
  // Note: during early bringup on the boot cpu, we will have not decided to use asids yet, so
  // kernel_asid() will return UNUSED_ASID. This is okay, we will decide later to
  // use asids on the boot cpu in riscv64_mmu_prevm_init and reload the satp.
  // Everything will be sorted out by the time secondary cpus are brought up.
  riscv64_switch_kernel_asid(kernel_asid());

  // Globally TLB flush.
  riscv64_tlb_flush_all();
}

void riscv64_mmu_prevm_init() {
  // Use asids if hardware has full 16 bit support and our command line switches allow.
  // We decide here because before now we have not been able to read gBootOptions.
  riscv_use_asid = gBootOptions->riscv64_enable_asid && riscv_asid_mask == 0xffff;

  // Now that we've decided to use asids, reload the kernel satp with the proper asid
  // on the boot cpu.
  riscv64_switch_kernel_asid(kernel_asid());
}

void riscv64_mmu_init() {
  dprintf(INFO, "RISCV: MMU enabled sv39\n");
  dprintf(INFO, "RISCV: MMU ASID mask %#lx, using asids %u\n", riscv_asid_mask, riscv_use_asid);
}

void Riscv64VmICacheConsistencyManager::SyncAddr(vaddr_t start, size_t len) {
  LTRACEF("start %#lx, len %zu\n", start, len);

  // Validate we are operating on a kernel address range.
  DEBUG_ASSERT(is_kernel_address(start));

  // Track that we'll need to fence.i at the end, the address is not important.
  need_invalidate_ = true;
}

void Riscv64VmICacheConsistencyManager::Finish() {
  LTRACEF("need_invalidate %d\n", need_invalidate_);
  if (!need_invalidate_) {
    return;
  }

  // Sync any address, since fence.i will dump the entire icache (for now).
  arch_sync_cache_range(KERNEL_ASPACE_BASE, PAGE_SIZE);

  need_invalidate_ = false;
}

uint32_t arch_address_tagging_features() { return 0; }

void arch_zero_page(void* _ptr) {
  const uintptr_t end_address = reinterpret_cast<uintptr_t>(_ptr) + PAGE_SIZE;

  if (gRiscvFeatures[arch::RiscvFeature::kZicboz]) {
    __asm__ volatile(
        R"""(
      .balign 4
      0:
        cbo.zero 0(%0)
        add  %0,%0,%2
        bne  %0,%1,0b
        )"""
        : "+r"(_ptr)
        : "r"(end_address), "r"(riscv_cboz_size)
        : "memory");

  } else {
    __asm__ volatile(
        R"""(
      .balign 4
      0:
        sd    zero,0(%0)
        sd    zero,8(%0)
        sd    zero,16(%0)
        sd    zero,24(%0)
        sd    zero,32(%0)
        sd    zero,40(%0)
        sd    zero,48(%0)
        sd    zero,56(%0)
        addi  %0,%0,64
        bne   %0,%1,0b
        )"""
        : "+r"(_ptr)
        : "r"(end_address)
        : "memory");
  }
}
