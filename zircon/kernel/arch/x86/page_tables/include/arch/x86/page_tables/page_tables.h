// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_X86_PAGE_TABLES_INCLUDE_ARCH_X86_PAGE_TABLES_PAGE_TABLES_H_
#define ZIRCON_KERNEL_ARCH_X86_PAGE_TABLES_INCLUDE_ARCH_X86_PAGE_TABLES_PAGE_TABLES_H_

#include <lib/zx/result.h>

#include <fbl/canary.h>
#include <hwreg/bitfields.h>
#include <kernel/mutex.h>
// Needed for ARCH_MMU_FLAG_*
#include <vm/arch_vm_aspace.h>
#include <vm/mapping_cursor.h>
#include <vm/pmm.h>

typedef uint64_t pt_entry_t;
#define PRIxPTE PRIx64

// Different page table levels in the page table mgmt hirerachy
enum class PageTableLevel {
  PT_L = 0,
  PD_L = 1,
  PDP_L = 2,
  PML4_L = 3,
};

// Different roles a page table can fulfill when running with unified aspaces.
enum class PageTableRole : uint8_t {
  kIndependent,
  kRestricted,
  kShared,
  kUnified,
};

// Structure for tracking an upcoming TLB invalidation
struct PendingTlbInvalidation {
  struct Item {
    uint64_t raw;
    DEF_SUBFIELD(raw, 2, 0, page_level);
    DEF_SUBBIT(raw, 3, is_global);
    DEF_SUBBIT(raw, 4, is_terminal);
    DEF_SUBFIELD(raw, 63, 12, encoded_addr);

    vaddr_t addr() const { return encoded_addr() << PAGE_SIZE_SHIFT; }
  };
  static_assert(sizeof(Item) == 8, "");

  // If true, ignore |vaddr| and perform a full invalidation for this context.
  bool full_shootdown = false;
  // If true, at least one enqueued entry was for a global page.
  bool contains_global = false;
  // Number of valid elements in |item|
  uint count = 0;
  // List of addresses queued for invalidation.
  // Explicitly uninitialized since the size is fairly large.
  Item item[32];

  // Add address |v|, translated at depth |level|, to the set of addresses to be invalidated.
  // |is_terminal| should be true iff this invalidation is targeting the final step of the
  // translation rather than a higher page table entry. |is_global_page| should be true iff this
  // page was mapped with the global bit set.
  void enqueue(vaddr_t v, PageTableLevel level, bool is_global_page, bool is_terminal);

  // Clear the list of pending invalidations
  void clear();

  ~PendingTlbInvalidation();
};

// Type for flags used in the hardware page tables, for terminal entries.
// Note that some flags here may have meanings that depend on the level
// at which they occur (e.g. page size and PAT).
using PtFlags = uint64_t;

// Type for flags used in the hardware page tables, for non-terminal
// entries.
using IntermediatePtFlags = uint64_t;

class X86PageTableBase {
 public:
  X86PageTableBase();
  virtual ~X86PageTableBase();

  paddr_t phys() const { return phys_; }
  void* virt() const { return virt_; }

  size_t pages() {
    Guard<Mutex> al{AssertOrderedLock, &lock_, LockOrder()};
    return pages_;
  }
  void* ctx() const { return ctx_; }

  using ExistingEntryAction = ArchVmAspaceInterface::ExistingEntryAction;
  using EnlargeOperation = ArchVmAspaceInterface::EnlargeOperation;

  // Returns whether this page table is restricted.
  // We do so by verifying that it was created with `InitRestricted` and has been linked to a
  // unified page table.
  bool IsRestricted() const { return role_ == PageTableRole::kRestricted; }

  // Returns whether this page table is shared.
  bool IsShared() const { return role_ == PageTableRole::kShared; }

  // Returns whether this page table is unified.
  bool IsUnified() const { return role_ == PageTableRole::kUnified; }

  // Accessors for the shared and restricted page tables on a unified page table.
  // We can turn off thread safety analysis as these accessors should only be used on unified page
  // tables, for which both the shared and restricted page table pointers are notionally const.
  X86PageTableBase* get_shared_pt() TA_NO_THREAD_SAFETY_ANALYSIS {
    DEBUG_ASSERT(IsUnified());
    return shared_pt_;
  }
  X86PageTableBase* get_restricted_pt() TA_NO_THREAD_SAFETY_ANALYSIS {
    DEBUG_ASSERT(IsUnified());
    return referenced_pt_;
  }

  zx_status_t MapPages(vaddr_t vaddr, paddr_t* phys, size_t count, uint flags,
                       ExistingEntryAction existing_action, size_t* mapped);
  zx_status_t MapPagesContiguous(vaddr_t vaddr, paddr_t paddr, const size_t count, uint flags,
                                 size_t* mapped);
  zx_status_t UnmapPages(vaddr_t vaddr, const size_t count, EnlargeOperation enlarge,
                         size_t* unmapped);
  zx_status_t ProtectPages(vaddr_t vaddr, size_t count, uint flags);

  zx_status_t QueryVaddr(vaddr_t vaddr, paddr_t* paddr, uint* mmu_flags);

  using NonTerminalAction = ArchVmAspaceInterface::NonTerminalAction;
  using TerminalAction = ArchVmAspaceInterface::TerminalAction;
  zx_status_t HarvestAccessed(vaddr_t vaddr, size_t count, NonTerminalAction non_terminal_action,
                              TerminalAction terminal_action);

  // Returns 1 for unified page tables and 0 for all other page tables. This establishes an
  // ordering that is used when the lock_ is acquired. The restricted page table lock is acquired
  // first, and the unified page table lock is acquired afterwards.
  uint32_t LockOrder() const { return IsUnified() ? 1 : 0; }

 protected:
  using page_alloc_fn_t = ArchVmAspaceInterface::page_alloc_fn_t;

  // Initialize an empty page table, assigning this given context to it.
  zx_status_t Init(void* ctx, page_alloc_fn_t test_paf = nullptr);
  // Initialize an empty page table and mark it as restricted.
  zx_status_t InitRestricted(void* ctx, page_alloc_fn_t test_paf = nullptr);
  // Initialize a page table, assign the given context, and prepopulate the top level page table
  // entries.
  zx_status_t InitShared(void* ctx, vaddr_t base, size_t size, page_alloc_fn_t test_paf = nullptr);
  // Initialize a page table, assign the given context, and set it up as a unified page table with
  // entries from the given page tables.
  //
  // The shared and restricted page tables must satisfy the following requirements:
  // 1) The shared page table must set only |is_shared_| to true.
  // 2) The restricted page table must set only |is_restricted_| to true.
  // 3) Both the shared and restricted page tables must have been initialized prior to this call.
  zx_status_t InitUnified(void* ctx, X86PageTableBase* shared, vaddr_t shared_base,
                          size_t shared_size, X86PageTableBase* restricted, vaddr_t restricted_base,
                          size_t restricted_size, page_alloc_fn_t test_paf = nullptr);

  // Calls DestroyUnified if this is a unified page table and DestroyIndividual if it is not.
  void Destroy(vaddr_t base, size_t size);

  // Returns the highest level of the page tables
  virtual PageTableLevel top_level() = 0;
  // Returns true if the given ARCH_MMU_FLAG_* flag combination is valid.
  virtual bool allowed_flags(uint flags) = 0;
  // Returns true if the given paddr is valid
  virtual bool check_paddr(paddr_t paddr) = 0;
  // Returns true if the given vaddr is valid
  virtual bool check_vaddr(vaddr_t vaddr) = 0;
  // Whether the processor supports the page size of this level
  virtual bool supports_page_size(PageTableLevel level) = 0;
  // Return the hardware flags to use on intermediate page tables entries
  virtual IntermediatePtFlags intermediate_flags() = 0;
  // Return the hardware flags to use on terminal page table entries
  virtual PtFlags terminal_flags(PageTableLevel level, uint flags) = 0;
  // Return the hardware flags to use on smaller pages after a splitting a
  // large page with flags |flags|.
  virtual PtFlags split_flags(PageTableLevel level, PtFlags flags) = 0;
  // Execute the given pending invalidation
  virtual void TlbInvalidate(const PendingTlbInvalidation* pending) = 0;

  // Convert PtFlags to ARCH_MMU_* flags.
  virtual uint pt_flags_to_mmu_flags(PtFlags flags, PageTableLevel level) = 0;
  // Returns true if a cache flush is necessary for pagetable changes to be
  // visible to hardware page table walkers. On x86, this is only true for Intel IOMMU page
  // tables when the IOMMU 'caching mode' bit is true.
  virtual bool needs_cache_flushes() = 0;

  // Page allocate function, overridable for testing.
  page_alloc_fn_t test_page_alloc_func_ = nullptr;

  // Pointer to the translation table.
  paddr_t phys_ = 0;
  pt_entry_t* virt_ = nullptr;

  // Counter of pages allocated to back the translation table.
  size_t pages_ TA_GUARDED(lock_) = 0;

  // A context structure that may used by a PageTable type above as part of
  // invalidation.
  void* ctx_ = nullptr;

  // Lock to protect the mmu code.
  DECLARE_MUTEX(X86PageTableBase, lockdep::LockFlagsNestable) lock_;

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(X86PageTableBase);
  class ConsistencyManager;

  zx_status_t AddMapping(volatile pt_entry_t* table, uint mmu_flags, PageTableLevel level,
                         ExistingEntryAction existing_action, MappingCursor& cursor,
                         ConsistencyManager* cm) TA_REQ(lock_);
  zx_status_t AddMappingL0(volatile pt_entry_t* table, uint mmu_flags,
                           ExistingEntryAction existing_action, MappingCursor& cursor,
                           ConsistencyManager* cm) TA_REQ(lock_);

  zx::result<bool> RemoveMapping(volatile pt_entry_t* table, PageTableLevel level,
                                 EnlargeOperation enlarge, MappingCursor& cursor,
                                 ConsistencyManager* cm) TA_REQ(lock_);
  bool RemoveMappingL0(volatile pt_entry_t* table, MappingCursor& cursor, ConsistencyManager* cm)
      TA_REQ(lock_);

  zx_status_t UpdateMapping(volatile pt_entry_t* table, uint mmu_flags, PageTableLevel level,
                            MappingCursor& cursor, ConsistencyManager* cm) TA_REQ(lock_);
  zx_status_t UpdateMappingL0(volatile pt_entry_t* table, uint mmu_flags, MappingCursor& cursor,
                              ConsistencyManager* cm) TA_REQ(lock_);
  bool HarvestMapping(volatile pt_entry_t* table, NonTerminalAction non_terminal_action,
                      TerminalAction terminal_action, PageTableLevel level, MappingCursor& cursor,
                      ConsistencyManager* cm) TA_REQ(lock_);
  void HarvestMappingL0(volatile pt_entry_t* table, TerminalAction terminal_action,
                        MappingCursor& cursor, ConsistencyManager* cm) TA_REQ(lock_);

  zx_status_t GetMapping(volatile pt_entry_t* table, vaddr_t vaddr, PageTableLevel level,
                         PageTableLevel* ret_level, volatile pt_entry_t** mapping) TA_REQ(lock_);
  zx_status_t GetMappingL0(volatile pt_entry_t* table, vaddr_t vaddr,
                           enum PageTableLevel* ret_level, volatile pt_entry_t** mapping)
      TA_REQ(lock_);

  zx_status_t SplitLargePage(PageTableLevel level, vaddr_t vaddr, volatile pt_entry_t* pte,
                             ConsistencyManager* cm) TA_REQ(lock_);

  void UpdateEntry(ConsistencyManager* cm, PageTableLevel level, vaddr_t vaddr,
                   volatile pt_entry_t* pte, paddr_t paddr, PtFlags flags, bool was_terminal,
                   bool exact_flags = false) TA_REQ(lock_);
  void UnmapEntry(ConsistencyManager* cm, PageTableLevel level, vaddr_t vaddr,
                  volatile pt_entry_t* pte, bool was_terminal) TA_REQ(lock_);

  pt_entry_t* AllocatePageTable();

  // Release the resources associated with this page table.  |base| and |size|
  // are only used for debug checks that the page tables have no more mappings.
  void DestroyIndividual(vaddr_t base, size_t size);

  // Releases the resources exclusively owned by this unified page table, and update the relevant
  // metadata on the associated restricted and shared page tables.
  void DestroyUnified();

  // Frees the top level page in this page table.
  void FreeTopLevelPage() TA_REQ(lock_);

  // Checks that the given page table entries are equal but ignores the accessed and dirty flags.
  bool check_equal_ignore_flags(pt_entry_t left, pt_entry_t right);

  fbl::Canary<fbl::magic("X86P")> canary_;

  // The number of times entries in the pml4 are referenced by other page tables.
  // Unified page tables increment and decrement this value on their associated shared and
  // restricted page tables, so we must hold the lock_ when doing so.
  uint32_t num_references_ TA_GUARDED(lock_) = 0;

  // The role this page table plays in unified aspaces, if any. This should only be set by the
  // Init* functions, and should not be modified anywhere else.
  PageTableRole role_ = PageTableRole::kIndependent;

  // A reference to another page table that shares entries with this one.
  // If is_restricted_ is set to true, this references the associated unified page table.
  // If is_unified_ is set to true, this references the associated restricted page table.
  // If neither is true, this is set to null.
  X86PageTableBase* referenced_pt_ TA_GUARDED(lock_) = nullptr;

  // A reference to a shared page table whose mappings are also present in this page table. This is
  // only set for unified page tables.
  X86PageTableBase* shared_pt_ TA_GUARDED(lock_) = nullptr;
};

#endif  // ZIRCON_KERNEL_ARCH_X86_PAGE_TABLES_INCLUDE_ARCH_X86_PAGE_TABLES_PAGE_TABLES_H_
