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
#include <arch/riscv64/mmu.h>
#include <arch/riscv64/sbi.h>
#include <fbl/auto_lock.h>
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
// TODO(fxbug.dev/24762): Choose it randomly.
uint64_t kernel_relocated_base = kArchHandoffVirtualAddress;

// The main translation table for the kernel. Used by the one kernel address space
// when kernel only threads are active.
alignas(PAGE_SIZE) pte_t riscv64_kernel_translation_table[RISCV64_MMU_PT_ENTRIES];

// A copy of the above table with memory identity mapped at 0.
alignas(PAGE_SIZE) pte_t riscv64_kernel_bootstrap_translation_table[RISCV64_MMU_PT_ENTRIES];

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
constexpr pte_t mmu_flags_to_pte_attr(uint flags, bool global) {
  pte_t attr = RISCV64_PTE_V;
  attr |= RISCV64_PTE_A | RISCV64_PTE_D;
  attr |= (flags & ARCH_MMU_FLAG_PERM_USER) ? RISCV64_PTE_U : 0;
  attr |= (flags & ARCH_MMU_FLAG_PERM_READ) ? RISCV64_PTE_R : 0;
  attr |= (flags & ARCH_MMU_FLAG_PERM_WRITE) ? RISCV64_PTE_W : 0;
  attr |= (flags & ARCH_MMU_FLAG_PERM_EXECUTE) ? RISCV64_PTE_X : 0;
  attr |= (global) ? RISCV64_PTE_G : 0;

  return attr;
}

// Construct a non leaf page table entry.
// For all inner page tables for the entire kernel hierarchy, set the global bit.
constexpr pte_t mmu_non_leaf_pte(paddr_t pa, bool global) {
  return riscv64_pte_pa_to_pte(pa) | (global ? RISCV64_PTE_G : 0) | RISCV64_PTE_V;
}

void update_pte(volatile pte_t* pte, pte_t newval) { *pte = newval; }

int first_used_page_table_entry(const volatile pte_t* page_table) {
  const int count = 1U << (PAGE_SIZE_SHIFT - 3);

  for (int i = 0; i < count; i++) {
    pte_t pte = page_table[i];
    if (riscv64_pte_is_valid(pte)) {
      return i;
    }
  }
  return -1;
}

bool page_table_is_clear(const volatile pte_t* page_table) {
  const int index = first_used_page_table_entry(page_table);
  const bool clear = index == -1;
  if (clear) {
    LTRACEF("page table at %p is clear\n", page_table);
  } else {
    LTRACEF("page_table at %p still in use, index %d is %#" PRIx64 "\n", page_table, index,
            page_table[index]);
  }
  return clear;
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
  __UNREACHABLE;
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

}  // namespace

// A consistency manager that tracks TLB updates, walker syncs and free pages in an effort to
// minimize MBs (by delaying and coalescing TLB invalidations) and switching to full ASID
// invalidations if too many TLB invalidations are requested.
class Riscv64ArchVmAspace::ConsistencyManager {
 public:
  ConsistencyManager(Riscv64ArchVmAspace& aspace) TA_REQ(aspace.lock_) : aspace_(aspace) {}
  ~ConsistencyManager() {
    Flush();
    if (!list_is_empty(&to_free_)) {
      pmm_free(&to_free_);
    }
  }

  // Queue a TLB entry for flushing. This may get turned into a complete ASID flush.
  void FlushEntry(vaddr_t va, bool terminal) {
    AssertHeld(aspace_.lock_);

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
  void Flush() TA_REQ(aspace_.lock_) {
    kcounter_add(cm_flush_call, 1);
    if (!full_flush_ && num_pending_tlb_runs_ == 0) {
      return;
    }
    // Need a mb to synchronize any page table updates prior to flushing the TLBs.
    mb();

    // Check if we should just be performing a full ASID invalidation.
    if (full_flush_) {
      aspace_.FlushAsid();
    } else {
      for (size_t i = 0; i < num_pending_tlb_runs_; i++) {
        const vaddr_t va = pending_tlbs_[i].va;
        const size_t count = pending_tlbs_[i].count;

        aspace_.FlushTLBEntryRun(va, count);
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

uint Riscv64ArchVmAspace::MmuFlagsFromPte(pte_t pte) {
  uint mmu_flags = 0;
  mmu_flags |= (pte & RISCV64_PTE_U) ? ARCH_MMU_FLAG_PERM_USER : 0;
  mmu_flags |= (pte & RISCV64_PTE_R) ? ARCH_MMU_FLAG_PERM_READ : 0;
  mmu_flags |= (pte & RISCV64_PTE_W) ? ARCH_MMU_FLAG_PERM_WRITE : 0;
  mmu_flags |= (pte & RISCV64_PTE_X) ? ARCH_MMU_FLAG_PERM_EXECUTE : 0;
  return mmu_flags;
}

zx_status_t Riscv64ArchVmAspace::Query(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) {
  Guard<Mutex> al{&lock_};
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
        *mmu_flags = MmuFlagsFromPte(pte);
      }
      LTRACEF("va 0x%lx, paddr 0x%lx, flags 0x%x\n", vaddr, paddr ? *paddr : ~0UL,
              mmu_flags ? *mmu_flags : ~0U);
      return ZX_OK;
    }

    page_table = static_cast<const volatile pte_t*>(paddr_to_physmap(pte_addr));
    level--;
  }
}

zx_status_t Riscv64ArchVmAspace::AllocPageTable(paddr_t* paddrp) {
  // Allocate a page from the pmm via function pointer passed to us in Init().
  // The default is pmm_alloc_page so test and explicitly call it to avoid any unnecessary
  // virtual functions.
  vm_page_t* page;
  zx_status_t status;
  if (likely(!test_page_alloc_func_)) {
    status = pmm_alloc_page(0, &page, paddrp);
  } else {
    status = test_page_alloc_func_(0, &page, paddrp);
  }
  if (status != ZX_OK) {
    return status;
  }

  page->set_state(vm_page_state::MMU);
  pt_pages_++;

  LOCAL_KTRACE("page table alloc");

  LTRACEF("allocated 0x%lx\n", *paddrp);

  if (!is_physmap_phys_addr(*paddrp)) {
    while (1)
      __asm__("nop");
  }
  return ZX_OK;
}

void Riscv64ArchVmAspace::FreePageTable(void* vaddr, paddr_t paddr, ConsistencyManager& cm) {
  LTRACEF("vaddr %p paddr 0x%lx\n", vaddr, paddr);

  LOCAL_KTRACE("page table free");

  vm_page_t* page = paddr_to_vm_page(paddr);
  if (!page) {
    panic("bad page table paddr 0x%lx\n", paddr);
  }
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

  paddr_t paddr;
  zx_status_t ret = AllocPageTable(&paddr);
  if (ret) {
    TRACEF("failed to allocate page table\n");
    return ret;
  }

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
// in the appropriate ASID or global if kernel.
void Riscv64ArchVmAspace::FlushTLBEntryRun(vaddr_t vaddr, size_t count) const {
  LTRACEF("vaddr %#lx, count %#lx, asid %#hx, kernel %u\n", vaddr, count, asid_, IsKernel());

  kcounter_add(cm_page_run_invalidate, 1);
  kcounter_add(cm_single_page_invalidate, static_cast<int64_t>(count));

  // Future optimization here and FlushAsid() when asids are disabled:
  // Based on which cpu has the aspace active, only send IPIs (either directly
  // or via SBI) to the other cores from that list to shoot down TLBs.
  const cpu_mask_t cpu_mask = mask_all_but_one(arch_curr_cpu_num());
  if (IsKernel()) {
    // Flush all ASIDs with this address
    for (size_t i = 0; i < count; i++) {
      riscv64_tlb_flush_address_all_asids(vaddr);
      vaddr += PAGE_SIZE;
    }
    sbi_remote_sfence_vma(cpu_mask, vaddr, count * PAGE_SIZE);
  } else if (IsUser()) {
    // Flush just the aspace's asid
    const uint16_t asid = asid_;
    for (size_t i = 0; i < count; i++) {
      riscv64_tlb_flush_address_one_asid(vaddr, asid);
      vaddr += PAGE_SIZE;
    }
    sbi_remote_sfence_vma_asid(cpu_mask, vaddr, count * PAGE_SIZE, asid_);
  } else {
    PANIC_UNIMPLEMENTED;
  }
}

// Flush an entire ASID on all cpus.
void Riscv64ArchVmAspace::FlushAsid() const {
  LTRACEF("asid %#hx, kernel %u\n", asid_, IsKernel());

  const cpu_mask_t cpu_mask = mask_all_but_one(arch_curr_cpu_num());
  if (IsKernel()) {
    // Perform a full flush of all cpus across all ASIDs
    riscv64_tlb_flush_all();

    sbi_remote_sfence_vma(cpu_mask, 0, -1);
    kcounter_add(cm_global_invalidate, 1);
  } else {
    // Perform a full flush of all cpus of a single ASID
    riscv64_tlb_flush_asid(asid_);

    sbi_remote_sfence_vma_asid(cpu_mask, 0, -1, asid_);
    kcounter_add(cm_asid_invalidate, 1);
  }
}

ssize_t Riscv64ArchVmAspace::UnmapPageTable(vaddr_t vaddr, vaddr_t vaddr_rel, size_t size,
                                            EnlargeOperation enlarge, uint level,
                                            volatile pte_t* page_table, ConsistencyManager& cm) {
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
      zx_status_t s = SplitLargePage(vaddr, level, index, page_table, cm);
      // If the split failed then we just fall through and unmap the entire large page.
      if (likely(s == ZX_OK)) {
        pte = page_table[index];
      } else if (enlarge == EnlargeOperation::No) {
        return s;
      }
    }

    // Check for an inner page table pointer.
    if (level > 0 && riscv64_pte_is_valid(pte) && !riscv64_pte_is_leaf(pte)) {
      const paddr_t page_table_paddr = riscv64_pte_pa(pte);
      volatile pte_t* next_page_table =
          static_cast<volatile pte_t*>(paddr_to_physmap(page_table_paddr));

      // Recurse a level.
      ssize_t result =
          UnmapPageTable(vaddr, vaddr_rem, chunk_size, enlarge, level - 1, next_page_table, cm);
      if (result < 0) {
        return result;
      }

      LTRACEF_LEVEL(2, "exited recursion: back at level %u\n", level);

      // If this is an entry corresponding to a top level kernel page table (MMU_PT_LEVELS - 1),
      // skip freeing it so that we always keep these kernel page tables populated in all address
      // spaces.
      const bool kernel_top_level_pt = (type_ == Riscv64AspaceType::kKernel) &&
                                       (index >= RISCV64_MMU_PT_KERNEL_BASE_INDEX) &&
                                       level == (RISCV64_MMU_PT_LEVELS - 1);
      if (!kernel_top_level_pt &&
          (chunk_size == block_size || page_table_is_clear(next_page_table))) {
        // If we unmapped an entire page table leaf and/or the unmap made the level below us empty,
        // free the page table.
        LTRACEF("pte %p[0x%lx] = 0 (was page table phys %#lx virt %p)\n", page_table, index,
                page_table_paddr, next_page_table);
        update_pte(&page_table[index], 0);

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

  return unmap_size;
}

ssize_t Riscv64ArchVmAspace::MapPageTable(vaddr_t vaddr_in, vaddr_t vaddr_rel_in, paddr_t paddr_in,
                                          size_t size_in, pte_t attrs, uint level,
                                          volatile pte_t* page_table, ConsistencyManager& cm) {
  vaddr_t vaddr = vaddr_in;
  vaddr_t vaddr_rel = vaddr_rel_in;
  paddr_t paddr = paddr_in;
  size_t size = size_in;

  const vaddr_t block_size = page_size_per_level(level);
  const vaddr_t block_mask = block_size - 1;

  LTRACEF("vaddr %#" PRIxPTR ", vaddr_rel %#" PRIxPTR ", paddr %#" PRIxPTR
          ", size %#zx, attrs %#" PRIx64 ", level %u, page_table %p\n",
          vaddr, vaddr_rel, paddr, size, attrs, level, page_table);

  if ((vaddr_rel | paddr | size) & (PAGE_MASK)) {
    TRACEF("not page aligned\n");
    return ZX_ERR_INVALID_ARGS;
  }

  auto cleanup = fit::defer([&]() {
    AssertHeld(lock_);
    UnmapPageTable(vaddr_in, vaddr_rel_in, size_in - size, EnlargeOperation::No, level, page_table,
                   cm);
  });

  size_t mapped_size = 0;
  while (size) {
    const vaddr_t vaddr_rem = vaddr_rel & block_mask;
    const size_t chunk_size = ktl::min(size, block_size - vaddr_rem);
    const vaddr_t index = vaddr_to_index(vaddr_rel, level);
    pte_t pte = page_table[index];

    // if we're at an unaligned address, and not trying to map a block larger than 1GB,
    // recurse one more level of the page table tree
    if (((vaddr_rel | paddr) & block_mask) || (chunk_size != block_size) || (level > 2)) {
      bool allocated_page_table = false;
      paddr_t page_table_paddr = 0;
      volatile pte_t* next_page_table = nullptr;

      if (!riscv64_pte_is_valid(pte)) {
        zx_status_t ret = AllocPageTable(&page_table_paddr);
        if (ret) {
          TRACEF("failed to allocate page table\n");
          return NULL;
        }
        allocated_page_table = true;
        void* pt_vaddr = paddr_to_physmap(page_table_paddr);

        LTRACEF("allocated page table, vaddr %p, paddr 0x%lx\n", pt_vaddr, page_table_paddr);
        arch_zero_page(pt_vaddr);

        // ensure that the zeroing is observable from hardware page table walkers, as we need to
        // do this prior to writing the pte we cannot defer it using the consistency manager.
        mb();

        pte = mmu_non_leaf_pte(page_table_paddr, IsKernel());
        update_pte(&page_table[index], pte);
        // We do not need to sync the walker, despite writing a new entry, as this is a
        // non-terminal entry and so is irrelevant to the walker anyway.
        LTRACEF("pte %p[%#" PRIxPTR "] = %#" PRIx64 " (paddr %#lx)\n", page_table, index, pte,
                paddr);
        next_page_table = static_cast<volatile pte_t*>(pt_vaddr);
      } else if (!riscv64_pte_is_leaf(pte)) {
        page_table_paddr = riscv64_pte_pa(pte);
        LTRACEF("found page table %#" PRIxPTR "\n", page_table_paddr);
        next_page_table = static_cast<volatile pte_t*>(paddr_to_physmap(page_table_paddr));
      } else {
        return ZX_ERR_ALREADY_EXISTS;
      }
      DEBUG_ASSERT(next_page_table);

      ssize_t ret =
          MapPageTable(vaddr, vaddr_rem, paddr, chunk_size, attrs, level - 1, next_page_table, cm);
      if (ret < 0) {
        if (allocated_page_table) {
          // We just allocated this page table. The unmap in err will not clean it up as the size
          // we pass in will not cause us to look at this page table. This is reasonable as if we
          // didn't allocate the page table then we shouldn't look into and potentially unmap
          // anything from that page table.
          // Since we just allocated it there should be nothing in it, otherwise the MapPageTable
          // call would not have failed.
          DEBUG_ASSERT(page_table_is_clear(next_page_table));
          page_table[index] = 0;

          // We can safely defer TLB flushing as the consistency manager will not return the backing
          // page to the PMM until after the tlb is flushed.
          cm.FlushEntry(vaddr, false);
          FreePageTable(const_cast<pte_t*>(next_page_table), page_table_paddr, cm);
        }
        return ret;
      }
      DEBUG_ASSERT(static_cast<size_t>(ret) == chunk_size);
    } else {
      if (riscv64_pte_is_valid(pte)) {
        LTRACEF("page table entry already in use, index %#" PRIxPTR ", %#" PRIx64 "\n", index, pte);
        return ZX_ERR_ALREADY_EXISTS;
      }

      pte = riscv64_pte_pa_to_pte(paddr) | attrs;
      LTRACEF("pte %p[%#" PRIxPTR "] = %#" PRIx64 "\n", page_table, index, pte);
      page_table[index] = pte;
    }
    vaddr += chunk_size;
    vaddr_rel += chunk_size;
    paddr += chunk_size;
    size -= chunk_size;
    mapped_size += chunk_size;
  }

  cleanup.cancel();
  return mapped_size;
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

ssize_t Riscv64ArchVmAspace::MapPages(vaddr_t vaddr, paddr_t paddr, size_t size, pte_t attrs,
                                      ConsistencyManager& cm) {
  LOCAL_KTRACE("mmu map", (vaddr & ~PAGE_MASK) | ((size >> PAGE_SIZE_SHIFT) & PAGE_MASK));
  uint level = RISCV64_MMU_PT_LEVELS - 1;
  ssize_t ret = MapPageTable(vaddr, vaddr, paddr, size, attrs, level, tt_virt_, cm);
  mb();
  return ret;
}

ssize_t Riscv64ArchVmAspace::UnmapPages(vaddr_t vaddr, size_t size, EnlargeOperation enlarge,
                                        ConsistencyManager& cm) {
  LOCAL_KTRACE("mmu unmap", (vaddr & ~PAGE_MASK) | ((size >> PAGE_SIZE_SHIFT) & PAGE_MASK));
  uint level = RISCV64_MMU_PT_LEVELS - 1;
  ssize_t ret = UnmapPageTable(vaddr, vaddr, size, enlarge, level, tt_virt_, cm);
  return ret;
}

zx_status_t Riscv64ArchVmAspace::ProtectPages(vaddr_t vaddr, size_t size, pte_t attrs) {
  LOCAL_KTRACE("mmu protect", (vaddr & ~PAGE_MASK) | ((size >> PAGE_SIZE_SHIFT) & PAGE_MASK));
  uint level = RISCV64_MMU_PT_LEVELS - 1;
  ConsistencyManager cm(*this);
  zx_status_t ret = ProtectPageTable(vaddr, vaddr, size, attrs, level, tt_virt_, cm);
  return ret;
}

void Riscv64ArchVmAspace::MmuParamsFromFlags(uint mmu_flags, pte_t* attrs) {
  if (attrs) {
    *attrs = mmu_flags_to_pte_attr(mmu_flags, IsKernel());
  }
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

  ssize_t ret;
  {
    Guard<Mutex> a{&lock_};
    if (mmu_flags & ARCH_MMU_FLAG_PERM_EXECUTE) {
      Riscv64VmICacheConsistencyManager cache_cm;
      cache_cm.SyncAddr(reinterpret_cast<vaddr_t>(paddr_to_physmap(paddr)), count * PAGE_SIZE);
    }
    pte_t attrs;
    MmuParamsFromFlags(mmu_flags, &attrs);
    ConsistencyManager cm(*this);
    ret = MapPages(vaddr, paddr, count * PAGE_SIZE, attrs, cm);
  }

  if (mapped) {
    *mapped = (ret > 0) ? (ret / PAGE_SIZE) : 0u;
    DEBUG_ASSERT(*mapped <= count);
  }

  return (ret < 0) ? (zx_status_t)ret : ZX_OK;
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

  size_t total_mapped = 0;
  {
    Guard<Mutex> a{&lock_};
    if (mmu_flags & ARCH_MMU_FLAG_PERM_EXECUTE) {
      Riscv64VmICacheConsistencyManager cache_cm;
      for (size_t idx = 0; idx < count; ++idx) {
        cache_cm.SyncAddr(reinterpret_cast<vaddr_t>(paddr_to_physmap(phys[idx])), PAGE_SIZE);
      }
    }
    pte_t attrs;
    MmuParamsFromFlags(mmu_flags, &attrs);

    ssize_t ret;
    size_t idx = 0;
    ConsistencyManager cm(*this);
    auto undo = fit::defer([&]() TA_NO_THREAD_SAFETY_ANALYSIS {
      if (idx > 0) {
        UnmapPages(vaddr, idx * PAGE_SIZE, EnlargeOperation::No, cm);
      }
    });

    vaddr_t v = vaddr;
    for (; idx < count; ++idx) {
      paddr_t paddr = phys[idx];
      DEBUG_ASSERT(IS_PAGE_ALIGNED(paddr));
      ret = MapPages(v, paddr, PAGE_SIZE, attrs, cm);
      if (ret < 0) {
        zx_status_t status = static_cast<zx_status_t>(ret);
        if (status != ZX_ERR_ALREADY_EXISTS || existing_action == ExistingEntryAction::Error) {
          return status;
        }
      } else {
        total_mapped += ret / PAGE_SIZE;
      }

      v += PAGE_SIZE;
    }
    undo.cancel();
  }
  DEBUG_ASSERT(total_mapped <= count);

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

  Guard<Mutex> a{&lock_};

  ConsistencyManager cm(*this);
  ssize_t ret = UnmapPages(vaddr, count * PAGE_SIZE, enlarge, cm);

  if (unmapped) {
    *unmapped = (ret > 0) ? (ret / PAGE_SIZE) : 0u;
    DEBUG_ASSERT(*unmapped <= count);
  }

  return (ret < 0) ? (zx_status_t)ret : 0;
}

zx_status_t Riscv64ArchVmAspace::Protect(vaddr_t vaddr, size_t count, uint mmu_flags) {
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

  Guard<Mutex> a{&lock_};
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

  int ret;
  {
    pte_t attrs;
    MmuParamsFromFlags(mmu_flags, &attrs);

    ret = ProtectPages(vaddr, count * PAGE_SIZE, attrs);
  }

  return ret;
}

zx_status_t Riscv64ArchVmAspace::HarvestAccessed(vaddr_t vaddr, size_t count,
                                                 NonTerminalAction non_terminal,
                                                 TerminalAction terminal) {
  canary_.Assert();
  LTRACEF("vaddr %#" PRIxPTR " count %zu\n", vaddr, count);

  if (!IS_PAGE_ALIGNED(vaddr) || !IsValidVaddr(vaddr)) {
    return ZX_ERR_INVALID_ARGS;
  }

  Guard<Mutex> guard{&lock_};

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

  Guard<Mutex> a{&lock_};

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

  Guard<Mutex> a{&lock_};

  // Validate that the base + size is valid and doesn't wrap.
  DEBUG_ASSERT(size_ > 0);
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
      DEBUG_ASSERT_MSG(IsUserBaseSizeValid(base_, size_), "base %#" PRIxPTR " size 0x%zx", base_,
                       size_);
      if (!IsUserBaseSizeValid(base_, size_)) {
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
    paddr_t pa;
    zx_status_t status = AllocPageTable(&pa);
    if (status != ZX_OK) {
      return status;
    }

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

void Riscv64ArchVmAspace::DisableUpdates() {
  // TODO-rvbringup: add machinery for this and the update checker logic
}

zx_status_t Riscv64ArchVmAspace::Destroy() {
  canary_.Assert();
  LTRACEF("aspace %p\n", this);

  Guard<Mutex> a{&lock_};

  // Not okay to destroy the kernel address space
  DEBUG_ASSERT(type_ != Riscv64AspaceType::kKernel);

  // Check to see if the top level page table is empty. If not the user didn't
  // properly unmap everything before destroying the aspace
  const int index = first_used_page_table_entry(tt_virt_);
  if (index != -1 && index >= (1 << (PAGE_SIZE_SHIFT - 2))) {
    panic("top level page table still in use! aspace %p tt_virt %p index %d entry %" PRIx64 "\n",
          this, tt_virt_, index, tt_virt_[index]);
  }

  if (pt_pages_ != 1) {
    panic("allocated page table count is wrong, aspace %p count %zu (should be 1)\n", this,
          pt_pages_);
  }

  if (riscv_use_asid) {
    // Flush the ASID associated with this aspace
    FlushAsid();

    // Free any ASID.
    auto status = asid_allocator.Free(asid_);
    ASSERT(status.is_ok());
    asid_ = MMU_RISCV64_UNUSED_ASID;
  }

  // Free the top level page table
  vm_page_t* page = paddr_to_vm_page(tt_phys_);
  DEBUG_ASSERT(page);
  pmm_free_page(page);
  pt_pages_--;

  tt_phys_ = 0;
  tt_virt_ = nullptr;

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
    satp = ((uint64_t)RISCV64_SATP_MODE_SV39 << RISCV64_SATP_MODE_SHIFT) |
           ((uint64_t)aspace->asid_ << RISCV64_SATP_ASID_SHIFT) |
           (aspace->tt_phys_ >> PAGE_SIZE_SHIFT);

    [[maybe_unused]] uint32_t prev =
        aspace->num_active_cpus_.fetch_add(1, ktl::memory_order_relaxed);
    DEBUG_ASSERT(prev < SMP_MAX_CPUS);
    aspace->active_since_last_check_.store(true, ktl::memory_order_relaxed);
  } else {
    // Switching to the null aspace, which means kernel address space only.
    satp = ((uint64_t)RISCV64_SATP_MODE_SV39 << RISCV64_SATP_MODE_SHIFT) |
           ((uint64_t)kernel_asid() << RISCV64_SATP_ASID_SHIFT) |
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

void riscv64_mmu_early_init() {
  // Figure out the number of supported ASID bits by writing all 1s to
  // the asid field in satp and seeing which ones 'stick'.
  auto satp_orig = riscv64_csr_read(satp);
  auto satp = satp_orig | (RISCV64_SATP_ASID_MASK << RISCV64_SATP_ASID_SHIFT);
  riscv64_csr_write(satp, satp);
  riscv_asid_mask = (riscv64_csr_read(satp) >> RISCV64_SATP_ASID_SHIFT) & RISCV64_SATP_ASID_MASK;
  riscv64_csr_write(satp, satp_orig);

  // Fill in all of the unused top level page table pointers for the kernel half of the kernel
  // top level table. These entries will be copied to all new address spaces, thus ensuring the
  // top level entries are synchronized.
  for (size_t i = RISCV64_MMU_PT_KERNEL_BASE_INDEX; i < RISCV64_MMU_PT_ENTRIES; i++) {
    if (!riscv64_pte_is_valid(riscv64_kernel_bootstrap_translation_table[i])) {
      paddr_t pt_paddr = kernel_virt_to_phys(
          riscv64_kernel_top_level_page_tables[i - RISCV64_MMU_PT_KERNEL_BASE_INDEX]);

      LTRACEF("RISCV: MMU allocating top level page table for slot %zu, pa %#lx\n", i, pt_paddr);

      pte_t pte = mmu_non_leaf_pte(pt_paddr, true);
      update_pte(&riscv64_kernel_bootstrap_translation_table[i], pte);
    }
  }

  // Make a copy of our bootstrap table with the identity map present in the user part.
  memcpy(riscv64_kernel_translation_table, riscv64_kernel_bootstrap_translation_table, PAGE_SIZE);

  // Zero the bottom of the kernel page table to remove any left over boot mappings.
  memset(riscv64_kernel_translation_table, 0, PAGE_SIZE / 2);

  // Make sure it's visible to the cpu
  wmb();
}

namespace {

// Load the kernel page tables and set the passed in asid
void riscv64_switch_kernel_asid(uint16_t asid) {
  const uint64_t satp = (RISCV64_SATP_MODE_SV39 << RISCV64_SATP_MODE_SHIFT) |
                        ((uint64_t)asid << RISCV64_SATP_ASID_SHIFT) |
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
  // Validate we are operating on a kernel address range.
  DEBUG_ASSERT(is_kernel_address(start));
  // use the physmap to clean the range to PoU, which is the point of where the instruction cache
  // pulls from. Cleaning to PoU is potentially cheaper than cleaning to PoC, which is the default
  // of arch_clean_cache_range.
  // TODO(revest): Flush
  // We can batch the icache invalidate and just perform it once at the end.
  need_invalidate_ = true;
}

void Riscv64VmICacheConsistencyManager::Finish() {
  if (!need_invalidate_) {
    return;
  }
  // TODO(revest): Flush
  need_invalidate_ = false;
}

uint32_t arch_address_tagging_features() { return 0; }

void arch_zero_page(void* _ptr) {
  const uintptr_t end_address = reinterpret_cast<uintptr_t>(_ptr) + PAGE_SIZE;
  asm volatile(
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
