// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "arch/x86/mmu.h"

#include <align.h>
#include <assert.h>
#include <lib/arch/sysreg.h>
#include <lib/arch/x86/boot-cpuid.h>
#include <lib/arch/x86/system.h>
#include <lib/counters.h>
#include <lib/id_allocator.h>
#include <lib/zircon-internal/macros.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <new>

#include <arch/arch_ops.h>
#include <arch/x86.h>
#include <arch/x86/descriptor.h>
#include <arch/x86/feature.h>
#include <arch/x86/hypervisor/invalidate.h>
#include <arch/x86/hypervisor/vmx_state.h>
#include <arch/x86/mmu_mem_types.h>
#include <kernel/mp.h>
#include <ktl/span.h>
#include <vm/arch_vm_aspace.h>
#include <vm/physmap.h>
#include <vm/pmm.h>
#include <vm/vm.h>

#define LOCAL_TRACE 0

// Count of the number of batches of TLB invalidations initiated on each CPU
KCOUNTER(tlb_invalidations_sent, "mmu.tlb_invalidation_batches_sent")
// Count of the number of batches of TLB invalidation requests received on each CPU
// Includes tlb_invalidations_full_global_received and tlb_invalidations_full_nonglobal_received
KCOUNTER(tlb_invalidations_received, "mmu.tlb_invalidation_batches_received")
// Count of the number of invalidate TLB invalidation requests received on each cpu
KCOUNTER(tlb_invalidations_received_invalid, "mmu.tlb_invalidation_batches_received_invalid")
// Count of the number of TLB invalidation requests for all entries on each CPU
KCOUNTER(tlb_invalidations_full_global_received, "mmu.tlb_invalidation_full_global_received")
// Count of the number of TLB invalidation requests for all non-global entries on each CPU
KCOUNTER(tlb_invalidations_full_nonglobal_received, "mmu.tlb_invalidation_full_nonglobal_received")
// Count of the number of times an EPT TLB invalidation got performed.
KCOUNTER(ept_tlb_invalidations, "mmu.ept_tlb_invalidations")
// Count the total number of context switches on the cpu
KCOUNTER(context_switches, "mmu.context_switches")
// Count the total number of fast context switches on the cpu (using PCID feature)
KCOUNTER(context_switches_pcid, "mmu.context_switches_pcid")

/* Default address width including virtual/physical address.
 * newer versions fetched below */
static uint8_t g_max_vaddr_width = 48;
uint8_t g_max_paddr_width = 32;

/* True if the system supports 1GB pages */
static bool supports_huge_pages = false;

/* a global bitmap to track which PCIDs are in use */
using PCIDAllocator = id_allocator::IdAllocator<uint16_t, 4096, 1>;
static lazy_init::LazyInit<PCIDAllocator> pcid_allocator;

// The physical address of the root page table, initialized in start.S.
paddr_t root_page_table_phys;

static pt_entry_t* root_page_table() {
  return reinterpret_cast<pt_entry_t*>(paddr_to_physmap(root_page_table_phys));
}

/* top level pdp needed to map the -512GB..0 space */
volatile pt_entry_t pdp_high[NO_OF_PT_ENTRIES] __ALIGNED(PAGE_SIZE);

#if __has_feature(address_sanitizer)
volatile pt_entry_t kasan_shadow_pt[NO_OF_PT_ENTRIES] __ALIGNED(PAGE_SIZE);  // Leaf page tables
volatile pt_entry_t kasan_shadow_pd[NO_OF_PT_ENTRIES] __ALIGNED(PAGE_SIZE);  // Page directories
// TODO(https://fxbug.dev/42104852): Share this with the vm::zero_page
volatile uint8_t kasan_zero_page[PAGE_SIZE] __ALIGNED(PAGE_SIZE);
#endif

/* a big pile of page tables needed to map 64GB of memory into kernel space using 2MB pages */
volatile pt_entry_t linear_map_pdp[(64ULL * GB) / (2 * MB)] __ALIGNED(PAGE_SIZE);

static constexpr uint64_t kNumKernelPageTables = (sizeof(linear_map_pdp) / PAGE_SIZE) + 4;

// When this bit is set in the source operand of a MOV CR3, TLB entries and paging structure
// caches for the active PCID may be preserved. If the bit is clear, entries will be cleared.
// See Intel Volume 3A, 4.10.4.1
#define X86_PCID_CR3_SAVE_ENTRIES (63)

// Static relocated base to prepare for KASLR. Used at early boot and by gdb
// script to know the target relocated address.
// TODO(thgarnie): Move to a dynamically generated base address
#if DISABLE_KASLR
uint64_t kernel_relocated_base = KERNEL_BASE - KERNEL_LOAD_OFFSET;
#else
uint64_t kernel_relocated_base = 0xffffffff00000000;
#endif

/**
 * @brief  check if the virtual address is canonical
 */
bool x86_is_vaddr_canonical(vaddr_t vaddr) {
  // If N is the number of address bits in use for a virtual address, then the
  // address is canonical if bits [N - 1, 63] are all either 0 (the low half of
  // the valid addresses) or 1 (the high half).
  return ((vaddr & kX86CanonicalAddressMask) == 0) ||
         ((vaddr & kX86CanonicalAddressMask) == kX86CanonicalAddressMask);
}

/**
 * @brief  check if the virtual address is aligned and canonical
 */
static bool x86_mmu_check_vaddr(vaddr_t vaddr) {
  /* Check to see if the address is PAGE aligned */
  if (!IS_ALIGNED(vaddr, PAGE_SIZE))
    return false;

  return x86_is_vaddr_canonical(vaddr);
}

/**
 * @brief  check if the physical address is valid and aligned
 */
bool x86_mmu_check_paddr(paddr_t paddr) {
  uint64_t max_paddr;

  /* Check to see if the address is PAGE aligned */
  if (!IS_ALIGNED(paddr, PAGE_SIZE))
    return false;

  max_paddr = ((uint64_t)1ull << g_max_paddr_width) - 1;

  return paddr <= max_paddr;
}

static void invlpg(vaddr_t addr) {
  __asm__ volatile("invlpg %0" ::"m"(*reinterpret_cast<uint8_t*>(addr)));
}

struct InvpcidDescriptor {
  uint64_t pcid{};
  uint64_t address{};
};

static void invpcid(InvpcidDescriptor desc, uint64_t mode) {
  __asm__ volatile("invpcid %0, %1" ::"m"(desc), "r"(mode));
}

static void invpcid_va_pcid(vaddr_t addr, uint16_t pcid) {
  // Mode 0 of INVPCID takes both the virtual address + pcid and locally shoots
  // down non global pages with it on the current cpu.
  uint64_t mode = 0;
  InvpcidDescriptor desc = {
      .pcid = pcid,
      .address = addr,
  };

  invpcid(desc, mode);
}

static void invpcid_pcid_all(uint16_t pcid) {
  // Mode 1 of INVPCID takes only the pcid and locally shoots down all non global
  // pages tagged with it on the current cpu.
  uint64_t mode = 1;
  InvpcidDescriptor desc = {
      .pcid = pcid,
      .address = 0,
  };

  invpcid(desc, mode);
}

static void invpcid_all_including_global() {
  // Mode 2 of INVPCID shoots down all tlb entries in all pcids including global pages
  // on the current cpu.
  uint64_t mode = 2;
  InvpcidDescriptor desc = {
      .pcid = 0,
      .address = 0,
  };

  invpcid(desc, mode);
}

static void invpcid_all_excluding_global() {
  // Mode 3 of INVPCID shoots down all tlb entries in all pcids excluding global pages
  // on the current cpu.
  uint64_t mode = 3;
  InvpcidDescriptor desc = {
      .pcid = 0,
      .address = 0,
  };

  invpcid(desc, mode);
}

/**
 * @brief  invalidate all TLB entries for the given PCID, excluding global entries
 */
static void x86_tlb_nonglobal_invalidate(uint16_t pcid) {
  if (g_x86_feature_invpcid) {
    // If using PCID, make sure we invalidate all entries in all PCIDs.
    // If just using INVPCID, take advantage of the fancier instruction.
    if (pcid != MMU_X86_UNUSED_PCID) {
      invpcid_pcid_all(pcid);
    } else {
      invpcid_all_excluding_global();
    }
  } else {
    // Read CR3 and immediately write it back.
    arch::X86Cr3::Read().Write();
  }
}

/**
 * @brief  invalidate all TLB entries for all contexts, including global entries
 */
static void x86_tlb_global_invalidate() {
  if (g_x86_feature_invpcid) {
    // If using PCID, make sure we invalidate all entries in all PCIDs.
    // If just using INVPCID, take advantage of the fancier instruction.
    invpcid_all_including_global();
  } else {
    // See Intel 3A section 4.10.4.1
    auto cr4 = arch::X86Cr4::Read();
    DEBUG_ASSERT(cr4.pge());  // Global pages *must* be enabled.
    cr4.set_pge(false).Write();
    cr4.set_pge(true).Write();
  }
}

// X86PageTableMmu

bool X86PageTableMmu::check_paddr(paddr_t paddr) { return x86_mmu_check_paddr(paddr); }

bool X86PageTableMmu::check_vaddr(vaddr_t vaddr) { return x86_mmu_check_vaddr(vaddr); }

bool X86PageTableMmu::supports_page_size(PageTableLevel level) {
  DEBUG_ASSERT(level != PageTableLevel::PT_L);
  switch (level) {
    case PageTableLevel::PD_L:
      return true;
    case PageTableLevel::PDP_L:
      return supports_huge_pages;
    case PageTableLevel::PML4_L:
      return false;
    default:
      panic("Unreachable case in supports_page_size\n");
  }
}

IntermediatePtFlags X86PageTableMmu::intermediate_flags() { return X86_MMU_PG_RW | X86_MMU_PG_U; }

PtFlags X86PageTableMmu::terminal_flags(PageTableLevel level, uint flags) {
  PtFlags terminal_flags = 0;

  if (flags & ARCH_MMU_FLAG_PERM_WRITE) {
    terminal_flags |= X86_MMU_PG_RW;
  }
  if (flags & ARCH_MMU_FLAG_PERM_USER) {
    terminal_flags |= X86_MMU_PG_U;
  }
  if (use_global_mappings_) {
    terminal_flags |= X86_MMU_PG_G;
  }
  if (!(flags & ARCH_MMU_FLAG_PERM_EXECUTE)) {
    terminal_flags |= X86_MMU_PG_NX;
  }

  if (level != PageTableLevel::PT_L) {
    switch (flags & ARCH_MMU_FLAG_CACHE_MASK) {
      case ARCH_MMU_FLAG_CACHED:
        terminal_flags |= X86_MMU_LARGE_PAT_WRITEBACK;
        break;
      case ARCH_MMU_FLAG_UNCACHED_DEVICE:
      case ARCH_MMU_FLAG_UNCACHED:
        terminal_flags |= X86_MMU_LARGE_PAT_UNCACHABLE;
        break;
      case ARCH_MMU_FLAG_WRITE_COMBINING:
        terminal_flags |= X86_MMU_LARGE_PAT_WRITE_COMBINING;
        break;
      default:
        panic("Unexpected flags 0x%x\n", flags);
    }
  } else {
    switch (flags & ARCH_MMU_FLAG_CACHE_MASK) {
      case ARCH_MMU_FLAG_CACHED:
        terminal_flags |= X86_MMU_PTE_PAT_WRITEBACK;
        break;
      case ARCH_MMU_FLAG_UNCACHED_DEVICE:
      case ARCH_MMU_FLAG_UNCACHED:
        terminal_flags |= X86_MMU_PTE_PAT_UNCACHABLE;
        break;
      case ARCH_MMU_FLAG_WRITE_COMBINING:
        terminal_flags |= X86_MMU_PTE_PAT_WRITE_COMBINING;
        break;
      default:
        panic("Unexpected flags 0x%x\n", flags);
    }
  }

  return terminal_flags;
}

PtFlags X86PageTableMmu::split_flags(PageTableLevel level, PtFlags flags) {
  DEBUG_ASSERT(level != PageTableLevel::PML4_L && level != PageTableLevel::PT_L);
  DEBUG_ASSERT(flags & X86_MMU_PG_PS);
  if (level == PageTableLevel::PD_L) {
    // Note: Clear PS before the check below; the PAT bit for a PTE is the
    // the same as the PS bit for a higher table entry.
    flags &= ~X86_MMU_PG_PS;

    /* If the larger page had the PAT flag set, make sure it's
     * transferred to the different index for a PTE */
    if (flags & X86_MMU_PG_LARGE_PAT) {
      flags &= ~X86_MMU_PG_LARGE_PAT;
      flags |= X86_MMU_PG_PTE_PAT;
    }
  }
  return flags;
}

void X86PageTableMmu::TlbInvalidate(const PendingTlbInvalidation* pending) {
  AssertHeld(lock_);
  if (pending->count == 0 && !pending->full_shootdown) {
    return;
  }

  kcounter_add(tlb_invalidations_sent, 1);

  const auto aspace = static_cast<X86ArchVmAspace*>(ctx());
  const ulong root_ptable_phys = phys();
  const uint16_t pcid = aspace->pcid();

  struct TlbInvalidatePage_context {
    paddr_t target_root_ptable;
    const PendingTlbInvalidation* pending;
    uint16_t pcid;
    bool is_shared;
  };
  TlbInvalidatePage_context task_context = {
      .target_root_ptable = root_ptable_phys,
      .pending = pending,
      .pcid = pcid,
      .is_shared = IsShared(),
  };

  mp_ipi_target_t target;
  cpu_mask_t target_mask = 0;
  // We need to send the TLB invalidate to all CPUs if this aspace is shared because active_cpus
  // is inaccurate in that case (another CPU may be running a unified aspace with these shared
  // mappings).
  // TODO(https://fxbug.dev/42083004): Replace this global broadcast for shared aspaces with a more
  // targeted one once shared aspaces keep track of all the CPUs they are on.
  if (IsShared() || pending->contains_global) {
    target = MP_IPI_TARGET_ALL;
  } else {
    target = MP_IPI_TARGET_MASK;
    // Target only CPUs this aspace is active on. It may be the case that some other CPU will
    // become active in it after this load, or will have left it  just before this load.
    // In the absence of PCIDs there are two cases:
    //  1. It is becoming active after the write to the page table, so it will see the change.
    //  2. It will get a potentially spurious request to flush.
    // With PCIDs we need additional handling for case (1), since an inactive CPU might have old
    // entries cached and so may not see the change, and case (2) is no longer spurious. See
    // additional comments in next if block.
    target_mask = aspace->active_cpus();

    if (g_x86_feature_pcid_enabled) {
      // Only the kernel aspace uses the 0 pcid, and all its mappings are global and so would have
      // forced an IPI_TARGET_ALL above.
      DEBUG_ASSERT(pcid != MMU_X86_UNUSED_PCID);
      // Mark all cpus as being dirty that aren't in this mask. This will force a TLB flush on the
      // next context switch on that cpu.
      aspace->MarkPcidDirtyCpus(~target_mask);

      // At this point we have CPUs in our target_mask that we're going to IPI, and any CPUS not in
      // target_mask that will at some point in the future become active and see the dirty bit.
      //
      // This is, however, not all CPUs, as there might be CPUs that are not in target_mask, but
      // became active before we could set the dirty bit. To account for these CPUs we read
      // active_cpus again and OR with the previous target_mask. It is possible that we might now
      // both IPI a core and have it flush on load due to the dirty bit, however this is a very
      // unlikely race condition and so will not be expensive in practice.
      //
      // Note that any CPU that manages to become active after we read target_mask and stop being
      // active before we read it again below does not matter, since the dirty bit is still set and
      // so when it eventually runs again it will still clear the PCID.
      target_mask |= aspace->active_cpus();
    }
  }

  /* Task used for invalidating a TLB entry on each CPU */
  auto tlb_invalidate_page_task = [](void* raw_context) -> void {
    DEBUG_ASSERT(arch_ints_disabled());
    const TlbInvalidatePage_context* context = static_cast<TlbInvalidatePage_context*>(raw_context);

    const paddr_t current_root_ptable = arch::X86Cr3::Read().base();

    kcounter_add(tlb_invalidations_received, 1);

    /* This invalidation doesn't apply to this CPU if:
     * - PCID feature is not being used
     * - It doesn't contain any global pages (ie, isn't the kernel)
     * - The target aspace is different (different root page table in cr3)
     * - This is not a shared mapping invalidation.
     */
    if (!g_x86_feature_pcid_enabled && !context->pending->contains_global &&
        context->target_root_ptable != current_root_ptable && !context->is_shared) {
      tlb_invalidations_received_invalid.Add(1);
      return;
    }

    // Handle full shootdowns of the TLB. This happens anytime full_shootdown is set and whenever
    // this is a TLB invalidation of a shared entry.
    if (context->pending->full_shootdown || context->is_shared) {
      if (context->pending->contains_global) {
        kcounter_add(tlb_invalidations_full_global_received, 1);
        x86_tlb_global_invalidate();
      } else {
        kcounter_add(tlb_invalidations_full_nonglobal_received, 1);
        if (context->is_shared) {
          // The shared region runs under many different PCIDs, so instead of tracking those PCIDs
          // we just invalidated all of them.
          x86_tlb_nonglobal_invalidate(MMU_X86_UNUSED_PCID);
        } else {
          x86_tlb_nonglobal_invalidate(context->pcid);
        }
      }
      return;
    }

    /* If not a full shootdown, then iterate through a list of pages and handle
     * them individually.
     */
    for (uint i = 0; i < context->pending->count; ++i) {
      const auto& item = context->pending->item[i];
      switch (static_cast<PageTableLevel>(item.page_level())) {
        case PageTableLevel::PML4_L:
          panic("PML4_L invld found; should not be here\n");
        case PageTableLevel::PDP_L:
        case PageTableLevel::PD_L:
        case PageTableLevel::PT_L:
          // Terminal entry is being asked to be flushed. If it's a global page or does not belong
          // to a special PCID, use the invlpg instruction.
          if (context->target_root_ptable == current_root_ptable || item.is_global() ||
              context->pcid == MMU_X86_UNUSED_PCID) {
            invlpg(item.addr());
          } else {
            /* This is a user page with a tagged PCID.
             */
            invpcid_va_pcid(item.addr(), context->pcid);
          }
          break;
      }
    }
  };

  mp_sync_exec(target, target_mask, tlb_invalidate_page_task, &task_context);
}

uint X86PageTableMmu::pt_flags_to_mmu_flags(PtFlags flags, PageTableLevel level) {
  uint mmu_flags = ARCH_MMU_FLAG_PERM_READ;

  if (flags & X86_MMU_PG_RW) {
    mmu_flags |= ARCH_MMU_FLAG_PERM_WRITE;
  }
  if (flags & X86_MMU_PG_U) {
    mmu_flags |= ARCH_MMU_FLAG_PERM_USER;
  }
  if (!(flags & X86_MMU_PG_NX)) {
    mmu_flags |= ARCH_MMU_FLAG_PERM_EXECUTE;
  }

  if (level != PageTableLevel::PT_L) {
    switch (flags & X86_MMU_LARGE_PAT_MASK) {
      case X86_MMU_LARGE_PAT_WRITEBACK:
        mmu_flags |= ARCH_MMU_FLAG_CACHED;
        break;
      case X86_MMU_LARGE_PAT_UNCACHABLE:
        mmu_flags |= ARCH_MMU_FLAG_UNCACHED;
        break;
      case X86_MMU_LARGE_PAT_WRITE_COMBINING:
        mmu_flags |= ARCH_MMU_FLAG_WRITE_COMBINING;
        break;
      default:
        panic("Unexpected flags %" PRIx64, flags);
    }
  } else {
    switch (flags & X86_MMU_PTE_PAT_MASK) {
      case X86_MMU_PTE_PAT_WRITEBACK:
        mmu_flags |= ARCH_MMU_FLAG_CACHED;
        break;
      case X86_MMU_PTE_PAT_UNCACHABLE:
        mmu_flags |= ARCH_MMU_FLAG_UNCACHED;
        break;
      case X86_MMU_PTE_PAT_WRITE_COMBINING:
        mmu_flags |= ARCH_MMU_FLAG_WRITE_COMBINING;
        break;
      default:
        panic("Unexpected flags %" PRIx64, flags);
    }
  }
  return mmu_flags;
}

// X86PageTableEpt

bool X86PageTableEpt::allowed_flags(uint flags) {
  if (!(flags & ARCH_MMU_FLAG_PERM_READ)) {
    return false;
  }
  return true;
}

bool X86PageTableEpt::check_paddr(paddr_t paddr) { return x86_mmu_check_paddr(paddr); }

bool X86PageTableEpt::check_vaddr(vaddr_t vaddr) { return x86_mmu_check_vaddr(vaddr); }

bool X86PageTableEpt::supports_page_size(PageTableLevel level) {
  DEBUG_ASSERT(level != PageTableLevel::PT_L);
  switch (level) {
    case PageTableLevel::PD_L:
      return vmx_ept_supports_large_pages();
    case PageTableLevel::PDP_L:
      return false;
    case PageTableLevel::PML4_L:
      return false;
    default:
      panic("Unreachable case in supports_page_size\n");
  }
}

PtFlags X86PageTableEpt::intermediate_flags() { return X86_EPT_R | X86_EPT_W | X86_EPT_X; }

PtFlags X86PageTableEpt::terminal_flags(PageTableLevel level, uint flags) {
  PtFlags terminal_flags = 0;

  if (flags & ARCH_MMU_FLAG_PERM_READ) {
    terminal_flags |= X86_EPT_R;
  }
  if (flags & ARCH_MMU_FLAG_PERM_WRITE) {
    terminal_flags |= X86_EPT_W;
  }
  if (flags & ARCH_MMU_FLAG_PERM_EXECUTE) {
    terminal_flags |= X86_EPT_X;
  }

  switch (flags & ARCH_MMU_FLAG_CACHE_MASK) {
    case ARCH_MMU_FLAG_CACHED:
      terminal_flags |= X86_EPT_WB;
      break;
    case ARCH_MMU_FLAG_UNCACHED_DEVICE:
    case ARCH_MMU_FLAG_UNCACHED:
      terminal_flags |= X86_EPT_UC;
      break;
    case ARCH_MMU_FLAG_WRITE_COMBINING:
      terminal_flags |= X86_EPT_WC;
      break;
    default:
      panic("Unexpected flags 0x%x", flags);
  }

  return terminal_flags;
}

PtFlags X86PageTableEpt::split_flags(PageTableLevel level, PtFlags flags) {
  DEBUG_ASSERT(level != PageTableLevel::PML4_L && level != PageTableLevel::PT_L);
  // We don't need to relocate any flags on split for EPT.
  return flags;
}

void X86PageTableEpt::TlbInvalidate(const PendingTlbInvalidation* pending) {
  if (pending->count == 0 && !pending->full_shootdown) {
    return;
  }

  kcounter_add(ept_tlb_invalidations, 1);

  // Target all CPUs with a context invalidation since we do not know what CPUs have this EPT
  // active. We cannot use active_cpus() is only updated by ContextSwitch, which does not get called
  // for guests, and also EPT mappings persist even if a guest is not presently executing. In
  // general unmap operations on EPTs should be extremely rare and not in any common path, so this
  // inefficiency is not disastrous in the short term. Similarly, since this is an infrequent
  // operation, we do not attempt to invalidate any individual entries, but just blow away the whole
  // context.
  // TODO: Track what CPUs the VCPUs using this EPT are migrated to and only IPI that subset.
  uint64_t eptp = ept_pointer_from_pml4(static_cast<X86ArchVmAspace*>(ctx())->arch_table_phys());
  broadcast_invept(eptp);
}

uint X86PageTableEpt::pt_flags_to_mmu_flags(PtFlags flags, PageTableLevel level) {
  uint mmu_flags = 0;

  if (flags & X86_EPT_R) {
    mmu_flags |= ARCH_MMU_FLAG_PERM_READ;
  }
  if (flags & X86_EPT_W) {
    mmu_flags |= ARCH_MMU_FLAG_PERM_WRITE;
  }
  if (flags & X86_EPT_X) {
    mmu_flags |= ARCH_MMU_FLAG_PERM_EXECUTE;
  }

  switch (flags & X86_EPT_MEMORY_TYPE_MASK) {
    case X86_EPT_WB:
      mmu_flags |= ARCH_MMU_FLAG_CACHED;
      break;
    case X86_EPT_UC:
      mmu_flags |= ARCH_MMU_FLAG_UNCACHED;
      break;
    case X86_EPT_WC:
      mmu_flags |= ARCH_MMU_FLAG_WRITE_COMBINING;
      break;
    default:
      panic("Unexpected flags %" PRIx64, flags);
  }

  return mmu_flags;
}

// We disable analysis due to the write to |pages_| tripping it up.  It is safe
// to write to |pages_| since this is part of object construction.
zx_status_t X86PageTableMmu::InitKernel(void* ctx,
                                        page_alloc_fn_t test_paf) TA_NO_THREAD_SAFETY_ANALYSIS {
  test_page_alloc_func_ = test_paf;

  phys_ = root_page_table_phys;
  virt_ = (pt_entry_t*)X86_PHYS_TO_VIRT(root_page_table_phys);
  page_ = Pmm::Node().PaddrToPage(root_page_table_phys);
  ctx_ = ctx;
  // These are all the page tables mapped in by start.S into the kernel aspace.
  pages_ = kNumKernelPageTables;
  use_global_mappings_ = true;
  return ZX_OK;
}

zx_status_t X86PageTableMmu::AliasKernelMappings() {
  auto upper_half = &(root_page_table()[NO_OF_PT_ENTRIES / 2]);
  // Copy the kernel portion of it from the master kernel pt.
  memcpy(virt_ + NO_OF_PT_ENTRIES / 2, reinterpret_cast<const void*>(upper_half),
         sizeof(pt_entry_t) * NO_OF_PT_ENTRIES / 2);
  return ZX_OK;
}

X86ArchVmAspace::X86ArchVmAspace(vaddr_t base, size_t size, uint mmu_flags,
                                 page_alloc_fn_t test_paf)
    : test_page_alloc_func_(test_paf), flags_(mmu_flags), base_(base), size_(size) {}

/*
 * Fill in the high level x86 arch aspace structure and allocating a top level page table.
 */
zx_status_t X86ArchVmAspace::Init() {
  canary_.Assert();

  LTRACEF("aspace %p, base %#" PRIxPTR ", size 0x%zx, mmu_flags 0x%x\n", this, base_, size_,
          flags_);

  if (flags_ & ARCH_ASPACE_FLAG_KERNEL) {
    X86PageTableMmu* mmu = new (&page_table_storage_.mmu) X86PageTableMmu();
    pt_ = mmu;

    zx_status_t status = mmu->InitKernel(this, test_page_alloc_func_);
    if (status != ZX_OK) {
      return status;
    }
    LTRACEF("kernel aspace: pt phys %#" PRIxPTR ", virt %p\n", pt_->phys(), pt_->virt());
  } else if (flags_ & ARCH_ASPACE_FLAG_GUEST) {
    X86PageTableEpt* ept = new (&page_table_storage_.ept) X86PageTableEpt();
    pt_ = ept;

    zx_status_t status = ept->Init(this, test_page_alloc_func_);
    if (status != ZX_OK) {
      return status;
    }
    LTRACEF("guest paspace: pt phys %#" PRIxPTR ", virt %p\n", pt_->phys(), pt_->virt());
  } else {
    X86PageTableMmu* mmu = new (&page_table_storage_.mmu) X86PageTableMmu();
    pt_ = mmu;

    if (g_x86_feature_pcid_enabled) {
      zx_status_t status = AllocatePCID();
      if (status != ZX_OK) {
        return status;
      }
    }

    zx_status_t status = mmu->Init(this, test_page_alloc_func_);
    if (status != ZX_OK) {
      return status;
    }

    status = mmu->AliasKernelMappings();
    if (status != ZX_OK) {
      return status;
    }

    LTRACEF("user aspace: pt phys %#" PRIxPTR ", virt %p, pcid %#hx\n", pt_->phys(), pt_->virt(),
            pcid_);
  }

  return ZX_OK;
}

zx_status_t X86ArchVmAspace::InitRestricted() {
  canary_.Assert();
  // Restricted ArchVmAspaces are only allowed with user address spaces.
  DEBUG_ASSERT(flags_ == 0);

  X86PageTableMmu* mmu = new (&page_table_storage_.mmu) X86PageTableMmu();
  pt_ = mmu;

  if (g_x86_feature_pcid_enabled) {
    zx_status_t status = AllocatePCID();
    if (status != ZX_OK) {
      return status;
    }
  }

  zx_status_t status = mmu->InitRestricted(this, test_page_alloc_func_);
  if (status != ZX_OK) {
    return status;
  }

  status = mmu->AliasKernelMappings();
  if (status != ZX_OK) {
    return status;
  }

  LTRACEF("user restricted aspace: pt phys %#" PRIxPTR ", virt %p, pcid %#hx\n", pt_->phys(),
          pt_->virt(), pcid_);
  return ZX_OK;
}

zx_status_t X86ArchVmAspace::InitShared() {
  canary_.Assert();
  // Shared ArchVmAspaces are only allowed with user address spaces.
  DEBUG_ASSERT(flags_ == 0);

  X86PageTableMmu* mmu = new (&page_table_storage_.mmu) X86PageTableMmu();
  pt_ = mmu;

  if (g_x86_feature_pcid_enabled) {
    zx_status_t status = AllocatePCID();
    if (status != ZX_OK) {
      return status;
    }
  }

  zx_status_t status = mmu->InitShared(this, base_, size_, test_page_alloc_func_);
  if (status != ZX_OK) {
    return status;
  }

  status = mmu->AliasKernelMappings();
  if (status != ZX_OK) {
    return status;
  }

  LTRACEF("user shared aspace: pt phys %#" PRIxPTR ", virt %p, pcid %#hx\n", pt_->phys(),
          pt_->virt(), pcid_);
  return ZX_OK;
}

zx_status_t X86ArchVmAspace::InitUnified(ArchVmAspaceInterface& shared,
                                         ArchVmAspaceInterface& restricted) {
  canary_.Assert();
  // Unified ArchVmAspaces are only allowed with user address spaces.
  DEBUG_ASSERT(flags_ == 0);

  X86PageTableMmu* mmu = new (&page_table_storage_.mmu) X86PageTableMmu();
  pt_ = mmu;

  if (g_x86_feature_pcid_enabled) {
    zx_status_t status = AllocatePCID();
    if (status != ZX_OK) {
      return status;
    }
  }

  X86ArchVmAspace& sharedX86 = static_cast<X86ArchVmAspace&>(shared);
  X86ArchVmAspace& restrictedX86 = static_cast<X86ArchVmAspace&>(restricted);
  // Validate that the shared and restricted aspaces are correctly initialized, as this can only be
  // done on MMU aspaces this tells us it is safe to case.
  ASSERT(sharedX86.pt_->IsShared());
  ASSERT(restrictedX86.pt_->IsRestricted());
  zx_status_t status =
      mmu->InitUnified(this, static_cast<X86PageTableMmu*>(sharedX86.pt_), sharedX86.base_,
                       sharedX86.size_, static_cast<X86PageTableMmu*>(restrictedX86.pt_),
                       restrictedX86.base_, restrictedX86.size_, test_page_alloc_func_);
  if (status != ZX_OK) {
    return status;
  }

  status = mmu->AliasKernelMappings();
  if (status != ZX_OK) {
    return status;
  }

  LTRACEF("user aspace: pt phys %#" PRIxPTR ", virt %p, pcid %#hx\n", pt_->phys(), pt_->virt(),
          pcid_);
  return ZX_OK;
}

zx_status_t X86ArchVmAspace::AllocatePCID() {
  DEBUG_ASSERT(g_x86_feature_pcid_enabled);
  zx::result<uint16_t> result = pcid_allocator->TryAlloc();
  if (result.is_error()) {
    // TODO(https://fxbug.dev/42075323): Implement some kind of PCID recycling.
    LTRACEF("X86: ran out of PCIDs when assigning new aspace\n");
    return ZX_ERR_NO_RESOURCES;
  }
  pcid_ = result.value();
  DEBUG_ASSERT(pcid_ != MMU_X86_UNUSED_PCID && pcid_ < 4096);

  // Start off with all cpus marked as dirty so the first context switch on any cpu
  // invalidates the entire PCID when it's loaded.
  MarkPcidDirtyCpus(CPU_MASK_ALL);
  return ZX_OK;
}

X86ArchVmAspace::~X86ArchVmAspace() {
  if (pt_) {
    pt_->~X86PageTableBase();
  }
  // TODO(https://fxbug.dev/42105844): check that we've destroyed the aspace.
}

zx_status_t X86ArchVmAspace::Destroy() {
  canary_.Assert();
  DEBUG_ASSERT(active_cpus_.load() == 0);

  if (flags_ & ARCH_ASPACE_FLAG_GUEST) {
    static_cast<X86PageTableEpt*>(pt_)->Destroy(base_, size_);
  } else {
    static_cast<X86PageTableMmu*>(pt_)->Destroy(base_, size_);
    if (pcid_ != MMU_X86_UNUSED_PCID) {
      auto result = pcid_allocator->Free(pcid_);
      DEBUG_ASSERT(result.is_ok());
    }
  }
  return ZX_OK;
}

zx_status_t X86ArchVmAspace::Unmap(vaddr_t vaddr, size_t count, EnlargeOperation enlarge,
                                   size_t* unmapped) {
  DEBUG_ASSERT(!pt_->IsUnified());
  if (!IsValidVaddr(vaddr))
    return ZX_ERR_INVALID_ARGS;

  zx_status_t result = pt_->UnmapPages(vaddr, count, enlarge, unmapped);
  MarkAspaceModified();
  return result;
}

zx_status_t X86ArchVmAspace::MapContiguous(vaddr_t vaddr, paddr_t paddr, size_t count,
                                           uint mmu_flags, size_t* mapped) {
  DEBUG_ASSERT(!pt_->IsUnified());
  if (!IsValidVaddr(vaddr))
    return ZX_ERR_INVALID_ARGS;

  zx_status_t result = pt_->MapPagesContiguous(vaddr, paddr, count, mmu_flags, mapped);
  MarkAspaceModified();
  return result;
}

zx_status_t X86ArchVmAspace::Map(vaddr_t vaddr, paddr_t* phys, size_t count, uint mmu_flags,
                                 ExistingEntryAction existing_action, size_t* mapped) {
  DEBUG_ASSERT(!pt_->IsUnified());
  if (!IsValidVaddr(vaddr))
    return ZX_ERR_INVALID_ARGS;

  zx_status_t result = pt_->MapPages(vaddr, phys, count, mmu_flags, existing_action, mapped);
  MarkAspaceModified();
  return result;
}

zx_status_t X86ArchVmAspace::Protect(vaddr_t vaddr, size_t count, uint mmu_flags,
                                     EnlargeOperation enlarge) {
  DEBUG_ASSERT(!pt_->IsUnified());
  if (!IsValidVaddr(vaddr))
    return ZX_ERR_INVALID_ARGS;

  zx_status_t result = pt_->ProtectPages(vaddr, count, mmu_flags);
  MarkAspaceModified();
  return result;
}

void X86ArchVmAspace::ContextSwitch(X86ArchVmAspace* old_aspace, X86ArchVmAspace* aspace) {
  DEBUG_ASSERT(arch_ints_disabled());

  cpu_mask_t cpu_bit = cpu_num_to_mask(arch_curr_cpu_num());

  context_switches.Add(1);

  if (aspace != nullptr) {
    // Switching to another user aspace
    aspace->canary_.Assert();

    paddr_t phys = aspace->pt_phys();
    LTRACEF_LEVEL(3, "switching to aspace %p, pt %#" PRIXPTR "\n", aspace, phys);

    if (old_aspace != nullptr) {
      [[maybe_unused]] uint32_t prev = old_aspace->active_cpus_.fetch_and(~cpu_bit);
      // Make sure we were actually previously running on this CPU.
      DEBUG_ASSERT(prev & cpu_bit);
    }
    // Set ourselves as active on this CPU prior to clearing the dirty bit. This ensures that TLB
    // invalidation code will either see us as active, and know to IPI us, or we will see the dirty
    // bit and clear the tlb here. See comment in X86PageTableMmu::TlbInvalidate for more details.
    [[maybe_unused]] uint32_t prev = aspace->active_cpus_.fetch_or(cpu_bit);
    // Should not already be running on this CPU.
    DEBUG_ASSERT(!(prev & cpu_bit));

    // Load the new cr3, add in the pcid if it's supported
    if (aspace->pcid_ != MMU_X86_UNUSED_PCID) {
      DEBUG_ASSERT(g_x86_feature_pcid_enabled);
      DEBUG_ASSERT(aspace->pcid_ < 4096);
      arch::X86Cr3PCID cr3;

      // If the new aspace is marked as dirty for this cpu, force a TLB invalidate
      // when loading the new cr3. Clear the dirty flag while we're at it. If
      // another cpu sets the dirty flag after this point but before we load the cr3
      // and invalidate it, we'll at most end up with an extraneous dirty flag set.
      const cpu_mask_t dirty_mask = aspace->pcid_dirty_cpus_.fetch_and(~cpu_bit);
      if (dirty_mask & cpu_bit) {
        // This is a double negative, and noflush=0 -> flush.
        cr3.set_noflush(0);
      } else {
        cr3.set_noflush(1);
        context_switches_pcid.Add(1);
      }
      cr3.set_base(phys);
      cr3.set_pcid(aspace->pcid_ & 0xfff);
      cr3.Write();
    } else {
      arch::X86Cr3::Write(phys);
    }

    aspace->active_since_last_check_.store(true, ktl::memory_order_relaxed);
    // If we are switching to a unified aspace, we need to mark the associated shared and
    // restricted aspaces as active since the last check as well.
    if (aspace->IsUnified()) {
      // Being a unified aspace implies it is an MMU type.
      X86PageTableMmu* aspace_pt = static_cast<X86PageTableMmu*>(aspace->pt_);
      X86ArchVmAspace* shared = static_cast<X86ArchVmAspace*>(aspace_pt->get_shared_pt()->ctx());
      X86ArchVmAspace* restricted =
          static_cast<X86ArchVmAspace*>(aspace_pt->get_restricted_pt()->ctx());
      shared->active_since_last_check_.store(true, ktl::memory_order_relaxed);
      restricted->active_since_last_check_.store(true, ktl::memory_order_relaxed);
    }
  } else {
    // Switching to the kernel aspace
    LTRACEF_LEVEL(3, "switching to kernel aspace, pt %#" PRIxPTR "\n", root_page_table_phys);

    // Write the kernel top level page table. Note: even when using PCID we do not
    // need to do anything special here since we are intrinsically loading PCID 0 with
    // the noflush bit clear which is fine since the kernel uses global pages.
    arch::X86Cr3::Write(root_page_table_phys);
    if (old_aspace != nullptr) {
      [[maybe_unused]] uint32_t prev = old_aspace->active_cpus_.fetch_and(~cpu_bit);
      // Make sure we were actually previously running on this CPU
      DEBUG_ASSERT(prev & cpu_bit);
    }
  }

  // Cleanup io bitmap entries from previous thread.
  if (old_aspace)
    x86_clear_tss_io_bitmap(old_aspace->io_bitmap());

  // Set the io bitmap for this thread.
  if (aspace)
    x86_set_tss_io_bitmap(aspace->io_bitmap());
}

zx_status_t X86ArchVmAspace::Query(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) {
  DEBUG_ASSERT(!pt_->IsUnified());
  if (!IsValidVaddr(vaddr))
    return ZX_ERR_INVALID_ARGS;

  return pt_->QueryVaddr(vaddr, paddr, mmu_flags);
}

zx_status_t X86ArchVmAspace::HarvestAccessed(vaddr_t vaddr, size_t count,
                                             NonTerminalAction non_terminal_action,
                                             TerminalAction terminal_action) {
  DEBUG_ASSERT(!pt_->IsUnified());
  if (!IsValidVaddr(vaddr)) {
    return ZX_ERR_INVALID_ARGS;
  }
  return pt_->HarvestAccessed(vaddr, count, non_terminal_action, terminal_action);
}

bool X86ArchVmAspace::ActiveSinceLastCheck(bool clear) {
  // Read whether any CPUs are presently executing.
  bool currently_active = active_cpus_.load(ktl::memory_order_relaxed) != 0;
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

vaddr_t X86ArchVmAspace::PickSpot(vaddr_t base, vaddr_t end, vaddr_t align, size_t size,
                                  uint mmu_flags) {
  canary_.Assert();
  return PAGE_ALIGN(base);
}

void X86ArchVmAspace::HandoffPageTablesFromPhysboot(list_node_t* mmu_pages) {
  while (list_node_t* node = list_remove_head(mmu_pages)) {
    vm_page_t* page = reinterpret_cast<vm_page_t*>(node);
    page->set_state(vm_page_state::MMU);

    ktl::span entries{
        reinterpret_cast<pt_entry_t*>(paddr_to_physmap(page->paddr())),
        PAGE_SIZE / sizeof(pt_entry_t),
    };
    page->mmu.num_mappings = 0;
    for (pt_entry_t entry : entries) {
      if ((entry & X86_MMU_PG_P) != 0) {
        page->mmu.num_mappings++;
      }
    }
    page->set_state(vm_page_state::MMU);
  }
}

uint32_t arch_address_tagging_features() { return 0; }

void x86_mmu_early_init() {
  root_page_table_phys = arch::X86Cr3::Read().base();

  x86_mmu_percpu_init();

  x86_mmu_mem_type_init();

  // Unmap the lower identity mapping.
  root_page_table()[0] = 0;

  // As we are still in early init code we cannot use the general page invalidation mechanisms,
  // specifically ones that might use mp_sync_exec or kcounters, so just drop the entire tlb.
  x86_tlb_global_invalidate();

  /* get the address width from the CPU */
  auto vaddr_width =
      static_cast<uint8_t>(arch::BootCpuid<arch::CpuidAddressSizeInfo>().linear_addr_bits());
  auto paddr_width =
      static_cast<uint8_t>(arch::BootCpuid<arch::CpuidAddressSizeInfo>().phys_addr_bits());

  supports_huge_pages = x86_feature_test(X86_FEATURE_HUGE_PAGE);

  /* if we got something meaningful, override the defaults.
   * some combinations of cpu on certain emulators seems to return
   * nonsense paddr widths (1), so trim it. */
  if (paddr_width > g_max_paddr_width) {
    g_max_paddr_width = paddr_width;
  }

  if (vaddr_width > g_max_vaddr_width) {
    g_max_vaddr_width = vaddr_width;
  }

  LTRACEF("paddr_width %u vaddr_width %u\n", g_max_paddr_width, g_max_vaddr_width);

  pcid_allocator.Initialize();
}

void x86_mmu_init() {
  printf("MMU: max physical address bits %u max virtual address bits %u\n", g_max_paddr_width,
         g_max_vaddr_width);
  if (g_x86_feature_pcid_enabled) {
    printf("MMU: Using PCID + INVPCID\n");
  } else if (g_x86_feature_invpcid) {
    printf("MMU: Using INVPCID\n");
  }

  ASSERT_MSG(g_max_vaddr_width >= kX86VAddrBits,
             "Maximum number of virtual address bits (%u) is less than the assumed number of bits"
             " being used (%u)\n",
             g_max_vaddr_width, kX86VAddrBits);
}

// Updates the page backing a given page table created in the context of
// start.S, in particular moving it from the WIRED to MMU state.
//
// TODO(https://fxbug.dev/42164859): This happens automatically for page tables
// handed off to the kernel from physboot, so this function will go away once
// those account for kernel aspace mappings.
static void unwire_boot_mmu_page(const volatile pt_entry_t* table) {
  // Convert to a phys address.
  paddr_t paddr = kernel_base_phys + reinterpret_cast<vaddr_t>(table) -
                  reinterpret_cast<vaddr_t>(__executable_start);

  vm_page_t* page = paddr_to_vm_page(paddr);
  ASSERT(page);

  // Unwire and mark it as an MMU page.
  pmm_unwire_page(page);
  page->set_state(vm_page_state::MMU);

  page->mmu.num_mappings = X86PageTableMmu::CountPresentEntries(table);
}

void x86_mmu_prevm_init() {
  // Unwire and mark as in use by the MMU all the page tables that might be part of the kernel
  // aspace as created by start.S.
  unwire_boot_mmu_page(pdp_high);
  for (size_t i = 0; i < sizeof(linear_map_pdp) / sizeof(pt_entry_t); i += NO_OF_PT_ENTRIES) {
    unwire_boot_mmu_page(&linear_map_pdp[i]);
  }

  // Use of PCID is detected late and on the boot cpu is this happens after x86_mmu_percpu_init
  // and so we enable it again here. For other CPUs, and when coming in and out of suspend, it
  // will happen correctly in x86_mmu_percpu_init.
  arch::X86Cr4 cr4 = arch::X86Cr4::Read();
  cr4.set_pcide(g_x86_feature_pcid_enabled);
  cr4.Write();
}

void x86_mmu_percpu_init() {
  arch::X86Cr0::Read()
      .set_wp(true)   // Set write protect.
      .set_nw(false)  // Clear not-write-through.
      .set_cd(false)  // Clear cache-disable.
      .Write();

  // Set or clear the SMEP & SMAP & PCIDE bits in CR4 based on features we've detected.
  // Make sure global pages are enabled.
  arch::X86Cr4 cr4 = arch::X86Cr4::Read();
  cr4.set_smep(x86_feature_test(X86_FEATURE_SMEP));
  cr4.set_smap(g_x86_feature_has_smap);
  cr4.set_pcide(g_x86_feature_pcid_enabled);
  cr4.set_pge(true);
  cr4.Write();

  // Set NXE bit in X86_MSR_IA32_EFER.
  uint64_t efer_msr = read_msr(X86_MSR_IA32_EFER);
  efer_msr |= X86_EFER_NXE;
  write_msr(X86_MSR_IA32_EFER, efer_msr);
}
