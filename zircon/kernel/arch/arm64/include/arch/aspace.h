// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2015-2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ASPACE_H_
#define ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ASPACE_H_

#include <lib/relaxed_atomic.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <arch/arm64/mmu.h>
#include <fbl/canary.h>
#include <kernel/mutex.h>
#include <vm/arch_vm_aspace.h>
#include <vm/mapping_cursor.h>

enum class ArmAspaceType {
  kUser,        // Userspace address space.
  kKernel,      // Kernel address space.
  kGuest,       // Second-stage address space.
  kHypervisor,  // EL2 hypervisor address space.
};

enum class ArmAspaceRole : uint8_t {
  kIndependent,
  kRestricted,
  kShared,
  kUnified,
};

class ArmArchVmAspace final : public ArchVmAspaceInterface {
 public:
  ArmArchVmAspace(vaddr_t base, size_t size, ArmAspaceType type, page_alloc_fn_t paf = nullptr);
  ArmArchVmAspace(vaddr_t base, size_t size, uint mmu_flags, page_alloc_fn_t paf = nullptr);
  virtual ~ArmArchVmAspace();

  // See comments on `ArchVmAspaceInterface` where these methods are declared.
  zx_status_t Init() override;
  zx_status_t InitShared() override;
  zx_status_t InitRestricted() override;
  zx_status_t InitUnified(ArchVmAspaceInterface& shared,
                          ArchVmAspaceInterface& restricted) override;

  void DisableUpdates() override;

  zx_status_t Destroy() override;

  // main methods
  zx_status_t Map(vaddr_t vaddr, paddr_t* phys, size_t count, uint mmu_flags,
                  ExistingEntryAction existing_action) override;
  zx_status_t MapContiguous(vaddr_t vaddr, paddr_t paddr, size_t count, uint mmu_flags) override;

  using ArchUnmapOptions = ArchVmAspaceInterface::ArchUnmapOptions;

  zx_status_t Unmap(vaddr_t vaddr, size_t count, ArchUnmapOptions enlarge) override;

  // ARM states that we must perform a break-before-make when changing the block size used by the
  // translation system (unless ARMv8.4-TTRem is supported) and so an unmap that needs to split a
  // large page must break-before-make, counting as an enlargement.
  bool UnmapOnlyEnlargeOnOom() const override { return false; }

  zx_status_t Protect(vaddr_t vaddr, size_t count, uint mmu_flags,
                      ArchUnmapOptions enlarge) override;

  zx_status_t Query(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) override;

  vaddr_t PickSpot(vaddr_t base, vaddr_t end, vaddr_t align, size_t size, uint mmu_flags) override;

  zx_status_t MarkAccessed(vaddr_t vaddr, size_t count) override;

  zx_status_t HarvestAccessed(vaddr_t vaddr, size_t count, NonTerminalAction non_terminal_action,
                              TerminalAction terminal_action) override;

  bool AccessedSinceLastCheck(bool clear) override;

  paddr_t arch_table_phys() const override { return tt_phys_; }
  uint16_t arch_asid() const { return asid_; }
  void arch_set_asid(uint16_t asid) { asid_ = asid; }

  static void ContextSwitch(ArmArchVmAspace* from, ArmArchVmAspace* to);

  static void HandoffPageTablesFromPhysboot(list_node_t* mmu_pages);

  // ARM only has accessed flags on terminal page mappings. This means that FreeUnaccessed will
  // only be able to free page tables where terminal accessed flags have been removed using
  // HarvestAccessed.
  static constexpr bool HasNonTerminalAccessedFlag() { return false; }

  static constexpr vaddr_t NextUserPageTableOffset(vaddr_t va) {
    // Work out the virtual address the next page table would start at by first masking the va down
    // to determine its index, then adding 1 and turning it back into a virtual address.
    const uint pt_bits = (MMU_USER_PAGE_SIZE_SHIFT - 3);
    const uint page_pt_shift = MMU_USER_PAGE_SIZE_SHIFT + pt_bits;
    return ((va >> page_pt_shift) + 1) << page_pt_shift;
  }

 private:
  class ConsistencyManager;
  inline bool IsValidVaddr(vaddr_t vaddr) const {
    return (vaddr >= base_ && vaddr <= base_ + size_ - 1);
  }

  zx::result<vm_page_t*> AllocPageTable() TA_REQ(lock_);

  enum class Reclaim : bool { No = false, Yes = true };
  void FreePageTable(void* vaddr, vm_page_t* page, ConsistencyManager& cm, Reclaim reclaim)
      TA_REQ(lock_);

  // Returns both a status, as well as how many new mappings were installed in |page_table|. If the
  // new mapping count is non-zero, regardless of the error value, the num_mappings field in the
  // page must be updated by the caller.
  ktl::pair<zx_status_t, uint> MapPageTable(pte_t attrs, bool ro, uint index_shift,
                                            volatile pte_t* page_table,
                                            ExistingEntryAction existing_action,
                                            MappingCursor& cursor, ConsistencyManager& cm)
      TA_REQ(lock_);

  // Used by callers of UnmapPageTable to indicate whether there might be empty page tables in the
  // tree that need to be checked for. Empty page tables are normally not allowed, but might be
  // there due to cleaning up a failed attempt at mapping.
  enum class CheckForEmptyPt : bool { No, Yes };

  // Unmaps the range of |cursor| from the |page_table| and returns both a status, as well as how
  // many mappings were removed in |page_table|. If the removed mapping count is non-zero,
  // regardless of the error value, the num_mappings field in the page must be updated by the
  // caller.
  ktl::pair<zx_status_t, uint> UnmapPageTable(VirtualAddressCursor& cursor,
                                              ArchUnmapOptions enlarge, CheckForEmptyPt pt_check,
                                              uint index_shift, volatile pte_t* page_table,
                                              ConsistencyManager& cm, Reclaim reclaim)
      TA_REQ(lock_);

  zx_status_t ProtectPageTable(vaddr_t vaddr_in, vaddr_t vaddr_rel_in, size_t size_in, pte_t attrs,
                               ArchUnmapOptions enlarge, uint index_shift,
                               volatile pte_t* page_table, ConsistencyManager& cm) TA_REQ(lock_);

  size_t HarvestAccessedPageTable(size_t* entry_limit, vaddr_t vaddr_in, vaddr_t vaddr_rel_in,
                                  size_t size_in, uint index_shift,
                                  NonTerminalAction non_terminal_action,
                                  TerminalAction terminal_action, volatile pte_t* page_table,
                                  ConsistencyManager& cm) TA_REQ(lock_);

  void MarkAccessedPageTable(vaddr_t vaddr, vaddr_t vaddr_rel_in, size_t size, uint index_shift,
                             volatile pte_t* page_table) TA_REQ(lock_);

  // Splits a descriptor block into a set of next-level-down page blocks/pages.
  //
  // |vaddr| is the virtual address of the start of the block being split. |index_shift| is
  // the index shift of the page table entry of the descriptor blocking being split.
  // |page_table| is the page table that contains the descriptor block being split,
  // and |pt_index| is the index into that table.
  zx_status_t SplitLargePage(vaddr_t vaddr, uint index_shift, vaddr_t pt_index,
                             volatile pte_t* page_table, ConsistencyManager& cm) TA_REQ(lock_);

  pte_t MmuParamsFromFlags(uint mmu_flags);

  zx_status_t ProtectPages(vaddr_t vaddr, size_t size, pte_t attrs, ArchUnmapOptions enlarge,
                           vaddr_t vaddr_base, ConsistencyManager& cm) TA_REQ(lock_);
  zx_status_t QueryLocked(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags) TA_REQ(lock_);

  // Walks the aspace and finds the first leaf mapping that it can, return the virtual address (in
  // the kernel physmap) of the containing page table, the virtual address of the entry, and the
  // raw pte value.
  //
  // TODO(https://fxbug.dev/42159319): Once https://fxbug.dev/42159319 is resolved this method can
  // be removed.
  zx_status_t DebugFindFirstLeafMapping(vaddr_t* out_pt, vaddr_t* out_vaddr, pte_t* out_pte) const;

  void FlushTLBEntry(vaddr_t vaddr, bool terminal) const TA_REQ(lock_);
  void FlushTLBEntryForAllAsids(vaddr_t vaddr, bool terminal) const TA_REQ(lock_);

  void FlushAsid() const TA_REQ(lock_);
  void FlushAllAsids() const TA_REQ(lock_);

  uint MmuFlagsFromPte(pte_t pte);

  // Panic if the page table is not empty.
  //
  // The caller must be holding |lock_|.
  void AssertEmptyLocked() const TA_REQ(lock_);

  // Return if an aspace is shared.
  bool IsShared() const { return role_ == ArmAspaceRole::kShared; }

  // Return if an aspace is restricted.
  bool IsRestricted() const { return role_ == ArmAspaceRole::kRestricted; }

  // Return if an aspace is unified.
  bool IsUnified() const { return role_ == ArmAspaceRole::kUnified; }

  // The `DestroyIndividual` and `DestroyUnified` functions are called by the public `Destroy`
  // function depending on whether this address space is unified or not. Note that unified aspaces
  // must be destroyed prior to destroying their constituent aspaces. In other words, the shared
  // and restricted aspaces must have a longer lifetime than the unified aspaces they are part of.
  zx_status_t DestroyIndividual();
  zx_status_t DestroyUnified();

  // Returns the value to set the TCR register to during a context switch.
  uint64_t Tcr() const;

  // data fields
  fbl::Canary<fbl::magic("VAAS")> canary_;

  mutable DECLARE_CRITICAL_MUTEX(ArmArchVmAspace) lock_;

  // Tracks the number of pending access faults. A non-zero value informs the access harvester to
  // back off to avoid contention with access faults.
  RelaxedAtomic<uint64_t> pending_access_faults_{0};

  // Private RAII type to manage scoped inc/dec of pending_access_faults_ and preemption disabling.
  class AutoPendingAccessFault {
   public:
    explicit AutoPendingAccessFault(ArmArchVmAspace* aspace);
    ~AutoPendingAccessFault();

    AutoPendingAccessFault(const AutoPendingAccessFault&) = delete;
    AutoPendingAccessFault& operator=(const AutoPendingAccessFault&) = delete;

   private:
    // Include a preempt disable block so that during the state where we do not have the lock, but
    // have incremented the pending_access_faults_, we cannot be preempted.
    AutoPreemptDisabler preempt_disable_;
    ArmArchVmAspace* aspace_;
  };

  // Whether or not changes to this instance are allowed.
  bool updates_enabled_ = true;

  // Page allocate function, if set will be used instead of the default allocator
  const page_alloc_fn_t test_page_alloc_func_ = nullptr;

  uint16_t asid_ = MMU_ARM64_UNUSED_ASID;

  // Pointer to the translation table.
  paddr_t tt_phys_ = 0;
  vm_page_t* tt_page_ = nullptr;
  volatile pte_t* tt_virt_ = nullptr;

  // Upper bound of the number of pages allocated to back the translation
  // table.
  size_t pt_pages_ = 0;

  // Type of address space.
  const ArmAspaceType type_;

  // Range of address space.
  const vaddr_t base_ = 0;
  const size_t size_ = 0;

  // Once-computed page shift constants
  vaddr_t vaddr_base_ = 0;       // Offset that should be applied to address to compute PTE indices.
  uint32_t top_size_shift_ = 0;  // Log2 of aspace size.
  uint32_t top_index_shift_ = 0;  // Log2 top level shift.
  uint32_t page_size_shift_ = 0;  // Log2 of page size.

  // Number of CPUs this aspace is currently active on.
  ktl::atomic<uint32_t> num_active_cpus_ = 0;

  // Whether not this has been accessed since |AccessedSinceLastCheck| was called. This is updated
  // by MarkAccessed, and anywhere a mapping is installed with the AF flag set.
  bool accessed_since_last_check_ TA_GUARDED(lock_) = false;

  // The number of times entries in the top level page are referenced by other page tables.
  // Unified page tables increment and decrement this value on their associated shared and
  // restricted page tables, so we must hold the lock_ when doing so.
  uint32_t num_references_ TA_GUARDED(lock_) = 0;

  // The role this aspace plays in unified aspaces, if any. This should only set by the `Init*`
  // functions and should not be modified anywhere else.
  ArmAspaceRole role_ = ArmAspaceRole::kIndependent;

  // Pointers to the shared and restricted aspaces referenced by this aspace if it is unified.
  // These fields are set by InitUnified and should not be modified anywhere else.
  ArmArchVmAspace* shared_aspace_ = nullptr;
  ArmArchVmAspace* restricted_aspace_ = nullptr;
};

inline ArmArchVmAspace::AutoPendingAccessFault::AutoPendingAccessFault(ArmArchVmAspace* aspace)
    : aspace_{aspace} {
  aspace_->pending_access_faults_.fetch_add(1);
}

inline ArmArchVmAspace::AutoPendingAccessFault::~AutoPendingAccessFault() {
  [[maybe_unused]] const uint64_t previous_value = aspace_->pending_access_faults_.fetch_sub(1);
  DEBUG_ASSERT(previous_value >= 1);
}

// TODO: Take advantage of information in the CTR to determine if icache is PIPT and whether
// cleaning is required.
class ArmVmICacheConsistencyManager final : public ArchVmICacheConsistencyManagerInterface {
 public:
  ArmVmICacheConsistencyManager() = default;
  ~ArmVmICacheConsistencyManager() override { Finish(); }

  void SyncAddr(vaddr_t start, size_t len) override;
  void Finish() override;

  // If a cache operation is going to be performed anyone on the addresses provided, then can avoid
  // doing duplicate operations by insisting SyncAddr cleans to the PoC. If SyncAddr did not need
  // to do any cache operation, due to CTR.IDC[28]=1, then this still forces it to do a clean to
  // PoC. Note that the default clean, when CTR.IDC[28] = 0, is only to PoU.
  void ForceCleanToPoC() { clean_poc_ = true; }

 private:
  // When set SyncAddr will always clean to PoC, even if cleaning is not required by the
  // implementation.
  bool clean_poc_ = false;
  bool need_invalidate_ = false;
};

static inline uint64_t arm64_vttbr(uint16_t vmid, paddr_t baddr) {
  return static_cast<paddr_t>(vmid) << 48 | baddr;
}

using ArchVmAspace = ArmArchVmAspace;
using ArchVmICacheConsistencyManager = ArmVmICacheConsistencyManager;

#endif  // ZIRCON_KERNEL_ARCH_ARM64_INCLUDE_ARCH_ASPACE_H_
