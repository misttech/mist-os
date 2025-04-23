// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_PMM_NODE_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_PMM_NODE_H_

#include <lib/memalloc/range.h>

#include <fbl/canary.h>
#include <fbl/intrusive_double_list.h>
#include <kernel/event.h>
#include <kernel/lockdep.h>
#include <kernel/mutex.h>
#include <ktl/span.h>
#include <vm/evictor.h>
#include <vm/page_queues.h>
#include <vm/physical_page_borrowing_config.h>
#include <vm/pmm_arena.h>
#include <vm/pmm_checker.h>

// Forward declaration; defined in <lib/memalloc/range.h>
namespace memalloc {
struct Range;
}

// flags for allocation routines below
#define PMM_ALLOC_FLAG_ANY (0 << 0)     // no restrictions on which arena to allocate from
#define PMM_ALLOC_FLAG_LO_MEM (1 << 0)  // allocate only from arenas marked LO_MEM
// The caller is able to wait and retry this allocation and so pmm allocation functions are allowed
// to return ZX_ERR_SHOULD_WAIT, as opposed to ZX_ERR_NO_MEMORY, to indicate that the caller should
// wait and try again. This is intended for the PMM to tell callers who are able to wait that memory
// is low. The caller should not infer anything about memory state if it is told to wait, as the PMM
// may tell it to wait for any reason.
#define PMM_ALLOC_FLAG_CAN_WAIT (1 << 1)

class FreeLoanedPagesHolder;
class VmCompression;

// per numa node collection of pmm arenas and worker threads
class PmmNode {
 public:
  // This constructor may be called early in the boot sequence so make sure it does not do any "real
  // work" or depend on any globals.
  PmmNode() = default;
  ~PmmNode() = default;

  DISALLOW_COPY_ASSIGN_AND_MOVE(PmmNode);

  zx_status_t Init(ktl::span<const memalloc::Range> ranges);

  // See pmm_end_handoff().
  void EndHandoff();

  vm_page_t* PaddrToPage(paddr_t addr) TA_NO_THREAD_SAFETY_ANALYSIS;

  static constexpr int kIndexZeroBits = 4;
  static constexpr uint32_t kIndexReserved0 = 0xffffff00;

  // Returns compressed representation a page_t*, with the following characteristics:
  // - zeros in the last kIndexZeroBits bits, used by clients to store metadata.
  // - The value 0 is never returned, it can be used as "no page" marker.
  // - The value kIndexReserved0 is never returned, it can be used as a special marker.
  //
  // Note: This method needs to traverse (up to) all the memory pools so it's cost is
  //       low but not trivial.
  uint32_t PageToIndex(const vm_page_t* page) TA_NO_THREAD_SAFETY_ANALYSIS;
  // Converts the number returned by PageToIndex() back to a page_t* pointer.
  // It does not check for invalid indexes such as 0 or kIndexReserved0.
  //
  // Note: This method is faster than PageToIndex(), about the cost of some basic math
  //       and bit manipulation.
  vm_page_t* IndexToPage(uint32_t index) TA_NO_THREAD_SAFETY_ANALYSIS;

  // main allocator routines
  zx::result<vm_page_t*> AllocPage(uint alloc_flags);
  zx_status_t AllocPages(size_t count, uint alloc_flags, list_node* list);
  zx_status_t AllocRange(paddr_t address, size_t count, list_node* list);
  zx_status_t AllocContiguous(size_t count, uint alloc_flags, uint8_t alignment_log2, paddr_t* pa,
                              list_node* list);
  void FreePage(vm_page* page);
  void FreeList(list_node* list);

  // Calls the provided function, passing |page| back into it, serialized with any other calls to
  // |AllocLoanedPage|, |BeginFreeLoanedPage| and |FinishFreeLoanedPages|. This allows caller to
  // know that while the |with_page| callback is running there are no in progress calls to these
  // methods, and that the page is not presently in holding object, i.e. it is either fully owned by
  // the PmmNode, or fully owned by an object.
  void WithLoanedPage(vm_page_t* page, fit::inline_function<void(vm_page_t*)> with_page);

  // Allocates a single page from the loaned pages list. The allocated page will always have
  // is_loaned() being true, and must be returned by either FreeLoanedPage or FreeLoanedList. If
  // there are not loaned pages available ZX_ERR_UNAVAILABLE is returned, as an absence of loaned
  // pages does not constitute an out of memory scenario.
  // The provided callback must transition the page into a state such that it has a valid backlink,
  // i.e. it is in the OBJECT state with an owner set, prior to returning.
  // During the execution of the callback the page contents must *not* be modified.
  zx::result<vm_page_t*> AllocLoanedPage(fit::inline_function<void(vm_page_t*), 32> allocated);

  // Begins freeing a loaned page that was previously allocated by AllocLoanPage by moving into a
  // holding object. It is an error to attempt to free a non loaned page. When this method is called
  // the |page| must have a valid backlink (i.e. be in the OBJECT state with an owner set). This
  // backlink should be removed by the |release_page| callback, which is invoked under the loaned
  // pages lock, prior to transition the page into the holding state. The caller *must*, at some
  // point in the future, complete the page freeing process by passing the provided |flph| into a
  // |FinishFreeLoanedPages| call.
  void BeginFreeLoanedPage(vm_page_t* page, fit::inline_function<void(vm_page_t*)> release_page,
                           FreeLoanedPagesHolder& flph);
  // Completes the freeing of any loaned pages in |flph|, after which |flph| is allowed to be
  // destructed. Once this method is called on a given |flph| that object is effectively 'dead' and
  // is not allowed to be passed to any PmmNode methods.
  void FinishFreeLoanedPages(FreeLoanedPagesHolder& flph);

  // Begins freeing multiple pages that were allocated by AllocLoanedPage by moving into a holding
  // object. It is an error to attempt to free any non loaned pages. When this method is called all
  // pages in the array must have a valid backlink (i.e. be in the OBJECT state with an owner set),
  // and the |release_list| method must remove the backlink from all pages, and place them in the
  // provided list in the same order.
  // The caller *must*, at some point in the future, complete the page freeing process by passing
  // the provided |flph| into a |FinishFreeLoanedPages| call.
  void BeginFreeLoanedArray(
      vm_page_t** pages, size_t count,
      fit::inline_function<void(vm_page_t**, size_t, list_node_t*)> release_list,
      FreeLoanedPagesHolder& flph);

  void UnwirePage(vm_page* page);

  // Frees all pages in the given list and places them in the loaned state available to be returned
  // from AllocLoanedPage.
  void BeginLoan(list_node* page_list);

  // Marks a page that had been previously provided to BeginLoan as cancelled. This page may be in
  // the FREE_LOANED state, or presently in use.
  //
  // This call prevents the page from being reused for any new purpose until EndLoan(). For
  // presently-FREE_LOANED pages, this removes the pages from free_loaned_list_. For presently-used
  // pages, this specifies that the page will not be added to free_loaned_list_ when later freed.
  // Once this page is FREE_LOANED (to be ensured by the caller via PhysicalPageProvider reclaim of
  // the pages), the loan can be ended with EndLoan().
  void CancelLoan(vm_page_t* page);

  // Allocates the page to the caller as a regular non-loaned page. Must currently be:
  //  * Loaned (via BeginLoan).
  //  * Have had its loan cancelled (via CancelLoan).
  //  * Be in the FREE_LOANED state.
  void EndLoan(vm_page_t* page);

  // See |pmm_set_free_memory_signal|
  bool SetFreeMemorySignal(uint64_t free_lower_bound, uint64_t free_upper_bound,
                           uint64_t delay_allocations_pages, Event* event);

  zx_status_t WaitTillShouldRetrySingleAlloc(const Deadline& deadline) {
    return free_pages_evt_.Wait(deadline);
  }

  void StopReturningShouldWait();

  uint64_t CountFreePages() const;
  uint64_t CountLoanedFreePages() const;
  uint64_t CountLoanCancelledPages() const;
  // Not actually used and cancelled is still not free, since the page can't be allocated in that
  // state.
  uint64_t CountLoanedNotFreePages() const;
  uint64_t CountLoanedPages() const;
  uint64_t CountTotalBytes() const;

  // printf free and overall state of the internal arenas
  // NOTE: both functions skip mutexes and can be called inside timer or crash context
  // though the data they return may be questionable
  void DumpFree() const TA_NO_THREAD_SAFETY_ANALYSIS;
  void Dump(bool is_panic) const TA_NO_THREAD_SAFETY_ANALYSIS;

  // Returns the number of active arenas.
  size_t NumArenas() const {
    Guard<Mutex> guard{&lock_};
    return active_arenas().size();
  }

  // Copies |count| pmm_arena_info_t objects into |buffer| starting with the |i|-th arena ordered by
  // base address.  For example, passing an |i| of 1 would skip the 1st arena.
  //
  // The objects will be sorted in ascending order by arena base address.
  //
  // Returns ZX_ERR_OUT_OF_RANGE if |count| is 0 or |i| and |count| specificy an invalid range.
  //
  // Returns ZX_ERR_BUFFER_TOO_SMALL if the buffer is too small.
  zx_status_t GetArenaInfo(size_t count, uint64_t i, pmm_arena_info_t* buffer, size_t buffer_size);

  // add new pages to the free queue. used when boostrapping a PmmArena
  void AddFreePages(list_node* list);

  PageQueues* GetPageQueues() { return &page_queues_; }

  // Retrieve any page compression instance. If this returns non-null then it's return value will
  // not change and the result can be cached.
  VmCompression* GetPageCompression() {
    Guard<Mutex> guard{&compression_lock_};
    return page_compression_.get();
  }

  // Set the page compression instance. Returns an error if one has already been set.
  zx_status_t SetPageCompression(fbl::RefPtr<VmCompression> compression);

  // Fill all free pages (both non-loaned and loaned) with a pattern and arm the checker.  See
  // |PmmChecker|.
  //
  // This is a no-op if the checker is not enabled.  See |EnableFreePageFilling|
  void FillFreePagesAndArm();

  // Synchronously walk the PMM's free list (and free loaned list) and validate each page.  This is
  // an incredibly expensive operation and should only be used for debugging purposes.
  void CheckAllFreePages();

#if __has_feature(address_sanitizer)
  // Synchronously walk the PMM's free list (and free loaned list) and poison each page.
  void PoisonAllFreePages();
#endif

  // Enable the free fill checker with the specified fill size and action, and begin filling freed
  // pages (including freed loaned pages) going forward.  See |PmmChecker| for definition of fill
  // size.
  //
  // Note, pages freed piror to calling this method will remain unfilled.  To fill them, call
  // |FillFreePagesAndArm|.
  //
  // Returns true if the checker was enabled with the requested fill_size, or |false| otherwise.
  bool EnableFreePageFilling(size_t fill_size, CheckFailAction action);

  // Return a pointer to this object's free fill checker.
  //
  // For test and diagnostic purposes.
  PmmChecker* Checker() { return &checker_; }

  static int64_t get_alloc_failed_count();

  // See |pmm_has_alloc_failed_no_mem|.
  static bool has_alloc_failed_no_mem();

  Evictor* GetEvictor() { return &evictor_; }

  // If randomly waiting on allocations is enabled, this re-seeds from the global prng, otherwise it
  // does nothing.
  void SeedRandomShouldWait();

  // Tell this PmmNode that we've failed a user-visible allocation.  Calling this method will
  // (optionally) trigger an asynchronous OOM response. To improve diagnostics some information
  // about the source of the failure can be provided.
  struct AllocFailure {
    enum class Type : uint8_t {
      None,
      Pmm,
      Heap,
      Handle,
      Other,
    };
    static const char* TypeToString(Type type);
    Type type = Type::None;
    size_t size = 0;
  };
  void ReportAllocFailure(AllocFailure failure) TA_EXCL(lock_);

  // Retrieves information given to |ReportAllocFailure|. Due to book keeping limitations this will
  // only return information from the first failure.
  AllocFailure GetFirstAllocFailure() TA_EXCL(lock_);

 private:
  zx_status_t InitArena(const PmmArenaSelection& selected);

  // An initialization subroutine to update PMM bookkeeping to reflect that a
  // given range is to be regarded as reserved (e.g., wired). This is used for
  // both special pre-PMM allocations (e.g., the kernel memory image) as well as
  // holes in RAM contained within arenas (indicated by type
  // memalloc::Type::kReserved).
  //
  // `range` is expected to be page-aligned and to not intersect with past
  // ranges provided to this method.
  void InitReservedRange(const memalloc::Range& range);

  void FreePageHelperLocked(vm_page* page, bool already_filled) TA_REQ(lock_);
  void FreeLoanedPageHelperLocked(vm_page* page, bool already_filled) TA_REQ(loaned_list_lock_);
  void FreeListLocked(list_node* list, bool already_filled) TA_REQ(lock_);
  template <typename F>
  void FreeLoanedListLocked(list_node* list, bool already_filled, F validator)
      TA_REQ(loaned_list_lock_);

  void SignalFreeMemoryChangeLocked() TA_REQ(lock_);
  void TripFreePagesLevelLocked() TA_REQ(lock_);
  void UpdateMemAvailStateLocked() TA_REQ(lock_);
  void SetMemAvailStateLocked(uint8_t mem_avail_state) TA_REQ(lock_);

  void IncrementFreeCountLocked(uint64_t amount) TA_REQ(lock_) {
    free_count_.fetch_add(amount, ktl::memory_order_relaxed);

    if (mem_signal_ && free_count_.load(ktl::memory_order_relaxed) > mem_signal_upper_bound_) {
      SignalFreeMemoryChangeLocked();
    }
  }
  void DecrementFreeCountLocked(uint64_t amount) TA_REQ(lock_) {
    [[maybe_unused]] uint64_t count = free_count_.fetch_sub(amount, ktl::memory_order_relaxed);
    DEBUG_ASSERT(count >= amount);

    if (should_wait_ == ShouldWaitState::OnceLevelTripped &&
        free_count_.load(ktl::memory_order_relaxed) < should_wait_free_pages_level_) {
      TripFreePagesLevelLocked();
    }

    if (mem_signal_ && free_count_.load(ktl::memory_order_relaxed) < mem_signal_lower_bound_) {
      SignalFreeMemoryChangeLocked();
    }
  }

  void IncrementFreeLoanedCountLocked(uint64_t amount) TA_REQ(loaned_list_lock_) {
    free_loaned_count_.fetch_add(amount, ktl::memory_order_relaxed);
  }
  void DecrementFreeLoanedCountLocked(uint64_t amount) TA_REQ(loaned_list_lock_) {
    DEBUG_ASSERT(free_loaned_count_.load(ktl::memory_order_relaxed) >= amount);
    free_loaned_count_.fetch_sub(amount, ktl::memory_order_relaxed);
  }

  void IncrementLoanedCountLocked(uint64_t amount) TA_REQ(loaned_list_lock_) {
    loaned_count_.fetch_add(amount, ktl::memory_order_relaxed);
  }
  void DecrementLoanedCountLocked(uint64_t amount) TA_REQ(loaned_list_lock_) {
    DEBUG_ASSERT(loaned_count_.load(ktl::memory_order_relaxed) >= amount);
    loaned_count_.fetch_sub(amount, ktl::memory_order_relaxed);
  }

  void IncrementLoanCancelledCountLocked(uint64_t amount) TA_REQ(loaned_list_lock_) {
    loan_cancelled_count_.fetch_add(amount, ktl::memory_order_relaxed);
  }
  void DecrementLoanCancelledCountLocked(uint64_t amount) TA_REQ(loaned_list_lock_) {
    DEBUG_ASSERT(loan_cancelled_count_.load(ktl::memory_order_relaxed) >= amount);
    loan_cancelled_count_.fetch_sub(amount, ktl::memory_order_relaxed);
  }

  bool ShouldDelayAllocationLocked() TA_REQ(lock_);

  void AllocPageHelperLocked(vm_page_t* page) TA_REQ(lock_);
  void AllocLoanedPageHelperLocked(vm_page_t* page) TA_REQ(loaned_list_lock_);

  // This method should be called when the PMM fails to allocate in a user-visible way and will
  // (optionally) trigger an asynchronous OOM response.
  void ReportAllocFailureLocked(AllocFailure failure) TA_REQ(lock_);

  fbl::Canary<fbl::magic("PNOD")> canary_;

  mutable DECLARE_MUTEX(PmmNode) lock_;

  uint64_t arena_cumulative_size_ TA_GUARDED(lock_) = 0;
  // This is both an atomic and guarded by lock_ as we would like modifications to require the lock,
  // as logic in the system relies on the free_count_ not changing whilst the lock is held, but also
  // be an atomic so it can be correctly read without the lock.
  ktl::atomic<uint64_t> free_count_ TA_GUARDED(lock_) = 0;
  ktl::atomic<uint64_t> free_loaned_count_ TA_GUARDED(loaned_list_lock_) = 0;
  ktl::atomic<uint64_t> loaned_count_ TA_GUARDED(loaned_list_lock_) = 0;
  ktl::atomic<uint64_t> loan_cancelled_count_ TA_GUARDED(loaned_list_lock_) = 0;

  // Free pages where !loaned.
  list_node free_list_ TA_GUARDED(lock_) = LIST_INITIAL_VALUE(free_list_);
  // Free pages where loaned && !loan_cancelled.
  mutable DECLARE_MUTEX(PmmNode) loaned_list_lock_;
  list_node free_loaned_list_ TA_GUARDED(loaned_list_lock_) = LIST_INITIAL_VALUE(free_loaned_list_);

  // The pages comprising the memory temporarily used during phys hand-off,
  // populated on Init(). It is the responsibility of EndHandoff() to free this
  // list.
  list_node phys_handoff_temporary_list_ TA_GUARDED(lock_) =
      LIST_INITIAL_VALUE(phys_handoff_temporary_list_);

  // The pages comprising the page-aligned regions of memory that we expect to
  // turn into VMOs to hand-off to userspace - as determined by
  // PhysHandoff::IsPhysVmoType() - populated and marked as wired on Init().
  //
  // It is expected that this memory will be unwired and turned into VMOs by the
  // end of the phys hand-off phase, and it is the responsibility of
  // PmmNode::EndHandoff() to ensure afterward that this list is empty.
  list_node phys_handoff_vmo_list_ TA_GUARDED(lock_) = LIST_INITIAL_VALUE(phys_handoff_vmo_list_);

  // The pages intended to be permanently reserved.
  //
  // TODO(https://fxbug.dev/42164859): Well, this list also currently includes
  // pages we're guaranteed to unwire (e.g., the vDSO and userboot). But that
  // soon won't be the case.
  list_node permanently_reserved_list_ TA_GUARDED(lock_) =
      LIST_INITIAL_VALUE(permanently_reserved_list_);

  // Controls the behavior of requests that have the PMM_ALLOC_FLAG_CAN_WAIT.
  enum class ShouldWaitState {
    // The PMM_ALLOC_FLAG_CAN_WAIT should never be followed and we will always attempt to perform
    // the allocation, or fail with ZX_ERR_NO_MEMORY. This state is permanent and cannot be left.
    Never,
    // Allocations do not need to be delayed, but the should_wait_free_pages_level_ should be
    // monitored and once tripped should be delayed.
    OnceLevelTripped,
    // State indicates that the level got tripped, and we should delay any allocations until the
    // level is reset.
    UntilReset,
  };
  ShouldWaitState should_wait_ TA_GUARDED(lock_) = ShouldWaitState::OnceLevelTripped;

  // Below this number of free pages the PMM will transition into delaying allocations.
  uint64_t should_wait_free_pages_level_ TA_GUARDED(lock_) = 0;

  // Event is signaled whenever allocations are allowed to happen based on the |should_wait_| state.
  // Whenever in the |UntilReset| state, this event will be Unsignaled causing waiters to block.
  Event free_pages_evt_;

  // A record of the first time an allocation failure is reported to aid in diagnostics.
  AllocFailure first_alloc_failure_ TA_GUARDED(lock_);

  // If mem_signal_ is not null, then once the available free memory falls outside of the defined
  // lower and upper bound the signal is raised. This is a one-shot signal and is cleared after
  // firing.
  Event* mem_signal_ TA_GUARDED(lock_) = nullptr;
  uint64_t mem_signal_lower_bound_ TA_GUARDED(lock_) = 0;
  uint64_t mem_signal_upper_bound_ TA_GUARDED(lock_) = 0;

  PageQueues page_queues_;

  Evictor evictor_;

  // The page_compression_ is a lazily initialized RefPtr to keep the PmmNode constructor simple, at
  // the cost needing to hold a lock to read the RefPtr. To avoid unnecessarily contending on the
  // main pmm lock_, use a separate one.
  DECLARE_MUTEX(PmmNode) compression_lock_;
  fbl::RefPtr<VmCompression> page_compression_ TA_GUARDED(compression_lock_);

  // Indicates whether pages should have a pattern filled into them when they are freed. This value
  // can only transition from false->true, and never back to false again. Once this value is set,
  // the fill size in checker_ may no longer be changed, and it becomes safe to call FillPattern
  // even without the lock held.
  // This is an atomic to allow for reading this outside of the lock, but modifications only happen
  // with the lock held.
  ktl::atomic<bool> free_fill_enabled_ TA_GUARDED(lock_) TA_GUARDED(loaned_list_lock_) = false;
  // Indicates whether it is known that all pages in the free list have had a pattern filled into
  // them. This value can only transition from false->true, and never back to false again. Once this
  // value is set the action and armed state in checker_ may no longer be changed, and it becomes
  // safe to call AssertPattern even without the lock held.
  bool all_free_pages_filled_ TA_GUARDED(loaned_list_lock_) TA_GUARDED(lock_) = false;
  PmmChecker checker_;

  // This method is racy as it allows us to read free_fill_enabled_ without holding the lock. If we
  // receive a value of 'true', then as there is no mechanism to re-set it to false, we know it is
  // still true. If we receive the value of 'false', then it could still become 'true' later.
  // The intent of this method is to allow for filling the free pattern outside of the lock in most
  // cases, and in the unlikely event of a race during the checker being armed, the pattern can
  // resort to being filled inside the lock.
  bool IsFreeFillEnabledRacy() const TA_NO_THREAD_SAFETY_ANALYSIS {
    // Read with acquire semantics to ensure that any modifications to checker_ are visible before
    // changes to free_fill_enabled_. See EnableFreePageFilling for where the release is performed.
    return free_fill_enabled_.load(ktl::memory_order_acquire);
  }
  // The free_fill_enabled_ and all_free_pages_filled_ members require both locks to modify, but can
  // be safely read with either lock held. These methods provide a convenient way to do so with
  // either of the locks held.
  bool FreeFillEnabledLocked() const TA_REQ(lock_) TA_NO_THREAD_SAFETY_ANALYSIS {
    return free_fill_enabled_;
  }
  bool FreeFillEnabledLoanedLocked() const TA_REQ(loaned_list_lock_) TA_NO_THREAD_SAFETY_ANALYSIS {
    return free_fill_enabled_;
  }
  bool FreePagesFilledLocked() const TA_REQ(lock_) TA_NO_THREAD_SAFETY_ANALYSIS {
    return all_free_pages_filled_;
  }
  bool FreePagesFilledLoanedLocked() const TA_REQ(loaned_list_lock_) TA_NO_THREAD_SAFETY_ANALYSIS {
    return all_free_pages_filled_;
  }

  // The rng state for random waiting on allocations. This allows us to use rand_r, which requires
  // no further thread synchronization, unlike rand().
  uintptr_t random_should_wait_seed_ TA_GUARDED(lock_) = 0;

  // Arenas are allocated from the node itself to avoid any boot allocations. Walking linearly
  // through them at run time should also be fairly efficient.
  static constexpr size_t kArenaCount = 16;

  // Bit constants to fold a vm_page_t pointer into a uint32 and back. The format from LSB
  // to MSB is : zero-bits | arena-index | page-index |. Where arena-index is 4 bits wide and
  // zero-bits is 4 bits wide. This limits the number of pages per arena to 2^24 -1 since the
  // last arena index of 0xffffff is used by kIndexReserved0.
  static constexpr int kArenaBits = 4;
  static_assert(kArenaCount == (1ul << kArenaBits));
  static constexpr size_t kMaxPagesPerArena = (1u << (32u - (kArenaBits + kIndexZeroBits))) - 1;
  static constexpr uint32_t kArenaMask = (1u << kArenaBits) - 1u;

  size_t used_arena_count_ TA_GUARDED(lock_) = 0;
  PmmArena arenas_[kArenaCount] TA_GUARDED(lock_);

  // Return the span of arenas from the built-in array that are known to be active. Used in loops
  // that iterate across all arenas.
  ktl::span<PmmArena> active_arenas() TA_REQ(lock_) {
    return ktl::span<PmmArena>(arenas_, used_arena_count_);
  }
  ktl::span<const PmmArena> active_arenas() const TA_REQ(lock_) {
    return ktl::span<const PmmArena>(arenas_, used_arena_count_);
  }
};

// We don't need to hold the arena lock while executing this, since it is
// only accesses values that are set once during system initialization.
inline vm_page_t* PmmNode::PaddrToPage(paddr_t addr) TA_NO_THREAD_SAFETY_ANALYSIS {
  for (auto& a : active_arenas()) {
    if (a.address_in_arena(addr)) {
      size_t index = (addr - a.base()) / PAGE_SIZE;
      return a.get_page(index);
    }
  }
  return nullptr;
}

// Same locking requirements as PaddrToPage().
inline vm_page_t* PmmNode::IndexToPage(uint32_t index) TA_NO_THREAD_SAFETY_ANALYSIS {
  index = index >> kIndexZeroBits;
  uint32_t arena_ix = index & kArenaMask;
  uint32_t page_ix = (index >> kArenaBits);
  return active_arenas()[arena_ix].get_page(page_ix - 1);
}

// Same locking requirements as PaddrToPage().
inline uint32_t PmmNode::PageToIndex(const vm_page_t* page) TA_NO_THREAD_SAFETY_ANALYSIS {
  uint32_t arena_ix = 0;
  for (auto& a : active_arenas()) {
    if (a.page_belongs_to_arena(page)) {
      auto page_ix = static_cast<uint32_t>(a.get_index(page) + 1);
      return ((page_ix << kArenaBits) | arena_ix) << kIndexZeroBits;
    }
    arena_ix += 1;
  }
  return 0;
}

// Object for managing freeing of loaned pages via a temporary holding object. Can be instantiated
// on the stack and then passed into different PmmNode methods, the object itself has no publicly
// available methods.
// This object is not thread safe, and multiple threads must not pass the same instance of this
// object into PmmNode methods.
// A given FreeLoanedPagesHolder, as described in |FinishFreeLoanedPages|, may only be used for a
// single call to |FinishFreeLoanedPages|, after which it is 'dead', and may not be passed to any
// other PmmNode methods.
class FreeLoanedPagesHolder {
 public:
  FreeLoanedPagesHolder() : pages_(LIST_INITIAL_VALUE(pages_)) {}
  ~FreeLoanedPagesHolder() {
    ASSERT(list_is_empty(&pages_));
    ASSERT(num_waiters_ == 0);
  }

 private:
  // A given FreeLoanedPagesHolder interval is only allowed to be used once to return pages to the
  // PMM, this tracks whether this has happened or not.
  // Only permitting a single instance of freeing simplifies any need to reason about a single
  // FreeLoanedPagesHolder repeatedly having pages moved into it and free'd to the PMM concurrently
  // with attempts to wait on it.
  // Although the lock cannot be annotated, this member is guarded by the relevant
  // PmmNode::loaned_list_lock_.
  bool used_ = false;
  // List of pages presently owned by this object. Every page in this list is defined to be in the
  // ALLOC state with |owner| set to this object.
  // Although the lock cannot be annotated, this member is guarded by the relevant
  // PmmNode::loaned_list_lock_.
  list_node_t pages_;
  // Count of the number of threads that are in the process of attempting to wait on
  // |freed_pages_event_| and have not yet completed the method. This is used, along with
  // |freed_pages_event_| and |no_waiters_event_| so that |withLoanedPage| can ensure the object it
  // references lives long enough.
  // See comments in |FinishFreeLoanedPages| and |WithLoanedPage| for more details on how these
  // work.
  // Although the lock cannot be annotated, this member is guarded by the relevant
  // PmmNode::loaned_list_lock_.
  uint64_t num_waiters_ = 0;
  Event freed_pages_event_;
  Event no_waiters_event_;
  friend PmmNode;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_PMM_NODE_H_
