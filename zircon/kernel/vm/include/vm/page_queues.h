// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_PAGE_QUEUES_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_PAGE_QUEUES_H_

#include <lib/fit/result.h>
#include <sys/types.h>
#include <zircon/listnode.h>

#include <fbl/algorithm.h>
#include <fbl/macros.h>
#include <kernel/event.h>
#include <kernel/lockdep.h>
#include <kernel/mutex.h>
#include <kernel/semaphore.h>
#include <ktl/array.h>
#include <ktl/optional.h>
#include <ktl/variant.h>
#include <vm/debug_compressor.h>
#include <vm/page.h>

class VmCowPages;

// Allocated pages that are part of the cow pages in a VmObjectPaged can be placed in a page queue.
// The page queues provide a way to
//  * Classify and group pages across VMO boundaries
//  * Retrieve the VMO that a page is contained in (via a back reference stored in the vm_page_t)
// Once a page has been placed in a page queue its queue_node becomes owned by the page queue and
// must not be used until the page has been Remove'd. It is not sufficient to call list_delete on
// the queue_node yourself as this operation is not atomic and needs to be performed whilst holding
// the PageQueues::lock_.
class PageQueues {
 public:
  // The number of reclamation queues is slightly arbitrary, but to be useful you want at least 3
  // representing
  //  * Very new pages that you probably don't want to evict as doing so probably implies you are in
  //    swap death
  //  * Slightly old pages that could be evicted if needed
  //  * Very old pages that you'd be happy to evict
  // With two active queues 8 page queues are used so that there is some fidelity of information in
  // the inactive queues. Additional queues have reduced value as sufficiently old pages quickly
  // become equivalently unlikely to be used in the future.
  static constexpr size_t kNumReclaim = 8;

  // Two active queues are used to allow for better fidelity of active information. This prevents
  // a race between aging once and needing to collect/harvest age information.
  static constexpr size_t kNumActiveQueues = 2;

  // The amount of pages that will have to move around the queues before the active/inactive
  // ratio is re-checked. This therefore represents how much error the active ratio aging process
  // might have, or how delayed the MRU generation might be.
  // In the worst case once the active ratio is triggered this value is how much page data needs to
  // then change queues before the aging process happens.
  static constexpr size_t kActiveInactiveErrorMargin = (2 * MB) / PAGE_SIZE;

  static_assert(kNumReclaim > kNumActiveQueues, "Needs to be at least one non-active queue");

  // In addition to active and inactive, we want to consider some of the queues as 'oldest' to
  // provide an additional way to limit eviction. Presently the processing of the LRU queue to make
  // room for aging is not integrated with the Evictor, and so will not trigger eviction, therefore
  // to have a non-zero number of pages ever appear in an oldest queue for eviction the last two
  // queues are considered the oldest.
  static constexpr size_t kNumOldestQueues = 2;
  static_assert(kNumOldestQueues + kNumActiveQueues <= kNumReclaim);

  // Number of different isolate queues that are available. Different isolate queues allow for
  // separating isolate pages into different buckets such that more nuanced choices on what page to
  // reclaim can be made.
  // Presently there is no mechanism to separate pages and so the number of queues is kept at 1.
  static constexpr size_t kNumIsolateQueues = 1;

  static constexpr zx_duration_mono_t kDefaultMinMruRotateTime = ZX_SEC(5);
  static constexpr zx_duration_mono_t kDefaultMaxMruRotateTime = ZX_SEC(5);

  // This is presently an arbitrary constant, since the min and max mru rotate time are currently
  // fixed at the same value, meaning that the active ratio can not presently trigger, or prevent,
  // aging.
  static constexpr uint64_t kDefaultActiveRatioMultiplier = 0;

  // When holding the PageQueue lock, and performing an operation on an
  // arbitrary number of pages, the "max batch size" controls the maximum number
  // of number of pages for which the lock will be held before letting it go and
  // allowing other operations to proceed.
  //
  // For example, if someone is calling RemoveArrayIntoList, with a list of 200
  // pages to release, the lock will be obtained and dropped a total of 4 times,
  // removing batches of 64 pages for the first 3 iterations, and finally a
  // batch of 8 pages for the final iteration.
  static inline constexpr size_t kMaxBatchSize = 64;

  PageQueues();
  ~PageQueues();

  DISALLOW_COPY_ASSIGN_AND_MOVE(PageQueues);

  // All Set operations places a page, which must not currently be in a page queue, into the
  // specified queue. The backlink information of |object| and |page_offset| must be specified and
  // valid. If the page is either removed from the referenced object, or moved to a different
  // offset, the backlink information must be updated either by calling ChangeObjectOffsetLocked, or
  // removing the page completely from the queues.

  void SetWired(vm_page_t* page, VmCowPages* object, uint64_t page_offset);
  // |skip_reclaim| controls whether reclaiming the page should be forcibly skipped regardless of
  // whether anonymous pages are considered reclaimable in general.
  void SetAnonymous(vm_page_t* page, VmCowPages* object, uint64_t page_offset,
                    bool skip_reclaim = false);
  void SetReclaim(vm_page_t* page, VmCowPages* object, uint64_t page_offset);
  void SetPagerBackedDirty(vm_page_t* page, VmCowPages* object, uint64_t page_offset);
  void SetAnonymousZeroFork(vm_page_t* page, VmCowPages* object, uint64_t page_offset);
  void SetHighPriority(vm_page_t* page, VmCowPages* object, uint64_t page_offset);

  // All Move operations change the queue that a page is considered to be in, but do not change the
  // object or offset backlink information. The page must currently be in a valid page queue.

  void MoveToWired(vm_page_t* page);
  // |skip_reclaim| controls whether reclaiming the page should be forcibly skipped regardless of
  // whether anonymous pages are considered reclaimable in general.
  void MoveToAnonymous(vm_page_t* page, bool skip_reclaim = false);
  void MoveToReclaim(vm_page_t* page);
  void MoveToReclaimDontNeed(vm_page_t* page);
  void MoveToPagerBackedDirty(vm_page_t* page);
  void MoveToAnonymousZeroFork(vm_page_t* page);
  void MoveToHighPriority(vm_page_t* page);

  // Indicates that page has failed a compression attempted, and moves it to a separate queue to
  // prevent it from being considered part of the reclaim set, which makes it neither active nor
  // inactive. The specified page must be in the page queues, but if not presently in a reclaim
  // queue this method will do nothing.
  // TODO(https://fxbug.dev/42138396): Determine whether/how pages are moved back into the reclaim
  // pool and either further generalize this to support pager backed, or specialize FailedReclaim to
  // be explicitly only anonymous.
  void CompressFailed(vm_page_t* page);

  // Changes the backlink information for a page and should only be called by the page owner under
  // its lock (that is the VMO lock). The page must currently be in a valid page queue.
  void ChangeObjectOffset(vm_page_t* page, VmCowPages* object, uint64_t page_offset);
  void ChangeObjectOffsetArray(vm_page_t** pages, VmCowPages* object, uint64_t* offsets,
                               size_t count);

  // Externally locked variant of CHangeObjectOffset that can be used for more efficient batch
  // operations. In addition to the annotated lock_, the VMO lock of the owner is also required to
  // be held.
  void ChangeObjectOffsetLockedList(vm_page_t* page, VmCowPages* object, uint64_t page_offset)
      TA_REQ(list_lock_);

  // Removes the page from any page list and returns ownership of the queue_node.
  void Remove(vm_page_t* page);
  // Batched version of Remove that also places all the pages in the specified list
  void RemoveArrayIntoList(vm_page_t** page, size_t count, list_node_t* out_list);

  // Tells the page queue this page has been accessed, and it should have its position in the queues
  // updated.
  void MarkAccessed(vm_page_t* page);

  // Provides access to the underlying lock, allowing _Locked variants to be called. Use of this is
  // highly discouraged as the underlying lock is a CriticalMutex which disables preemption.
  // Preferably *Array variations should be used, but this provides a higher performance mechanism
  // when needed.
  Lock<SpinLock>* get_lock() TA_RET_CAP(list_lock_) { return &list_lock_; }

  // Used to identify the reason that aging is triggered, mostly for debugging and informational
  // purposes.
  enum class AgeReason {
    // Aging occurred due to the maximum timeout being reached before any other reason could trigger
    Timeout,
    // The allowable ratio of active versus inactive pages was exceeded.
    ActiveRatio,
    // An explicit call to RotatePagerBackedQueues caused aging. This would typically occur due to
    // test code or via the kernel debug console.
    Manual,
  };
  static const char* string_from_age_reason(PageQueues::AgeReason reason);

  // Rotates the reclamation queues to perform aging. Every existing queue is now considered to be
  // one epoch older. To achieve these two things are done:
  //   1. A new queue, representing the current epoch, needs to be allocated to put pages that get
  //      accessed from here into. This just involves incrementing the MRU generation.
  //   2. As there is a limited number of page queues 'allocating' one might involve cleaning up an
  //      old queue. See the description of ProcessLruQueue for how this process works.
  void RotateReclaimQueues(AgeReason reason = AgeReason::Manual);

  // Used to represent and return page backlink information acquired whilst holding the page queue
  // lock. As a VMO may not destruct while it has pages in it, the cow RefPtr will always be valid,
  // although the page and offset contained here are not synchronized and must be separately
  // validated before use. This can be done by acquiring the returned vmo's lock and then validating
  // that the page is still contained at the offset.
  struct VmoBacklink {
    fbl::RefPtr<VmCowPages> cow;
    vm_page_t* page = nullptr;
    uint64_t offset = 0;
  };

  // Moves a page from from the anonymous zero fork queue into the anonymous queue and returns
  // the backlink information. If the zero fork queue is empty then a nullopt is returned, otherwise
  // if it has_value the vmo field may be null to indicate that the vmo is running its destructor
  // (see VmoBacklink for more details).
  ktl::optional<VmoBacklink> PopAnonymousZeroFork();

  // Looks at the isolate queues and returns backlink information of the first page found. If the
  // isolate queue is empty then LRU queues up to |lowest_queue| epochs from the most recent will be
  // processed to attempt to fill the isolate list. If no page was found a nullopt is returned,
  // otherwise if it has_value the vmo field may be null to indicate that the vmo is running its
  // destructor (see VmoBacklink for more details). If a page is returned its location in the
  // reclaim queue is not modified.
  ktl::optional<VmoBacklink> PeekIsolate(size_t lowest_queue);

  // Can be called while the |page| is known to be in the loaned state. This method checks if it is
  // in the page queues, and if so returns a reference to the cow pages that owns it.
  // The page must be 'owned' by the caller, in so far as the page->state() is guaranteed to not be
  // changing.
  ktl::optional<VmoBacklink> GetCowForLoanedPage(vm_page_t* page);

  // Helper struct to group reclaimable queue length counts returned by GetReclaimCounts.
  struct ReclaimCounts {
    size_t total = 0;
    size_t newest = 0;
    size_t oldest = 0;
  };

  // Returns just the reclaim queue counts. Called from the zx_object_get_info() syscall.
  ReclaimCounts GetReclaimQueueCounts() const;

  // Helper struct to group queue length counts returned by QueueCounts.
  struct Counts {
    ktl::array<size_t, kNumReclaim> reclaim = {0};
    size_t reclaim_isolate = 0;
    size_t pager_backed_dirty = 0;
    size_t anonymous = 0;
    size_t wired = 0;
    size_t anonymous_zero_fork = 0;
    size_t failed_reclaim = 0;
    size_t high_priority = 0;

    bool operator==(const Counts& other) const {
      return reclaim == other.reclaim && reclaim_isolate == other.reclaim_isolate &&
             pager_backed_dirty == other.pager_backed_dirty && anonymous == other.anonymous &&
             wired == other.wired && anonymous_zero_fork == other.anonymous_zero_fork &&
             failed_reclaim == other.failed_reclaim && high_priority == other.high_priority;
    }
    bool operator!=(const Counts& other) const { return !(*this == other); }
  };

  Counts QueueCounts() const;

  struct ActiveInactiveCounts {
    // Pages that would normally be available for eviction, but are presently considered active and
    // so will not be evicted.
    size_t active = 0;
    // Pages that are available for eviction due to not presently being considered active.
    size_t inactive = 0;

    bool operator==(const ActiveInactiveCounts& other) const {
      return active == other.active && inactive == other.inactive;
    }
    bool operator!=(const ActiveInactiveCounts& other) const { return !(*this == other); }
  };
  ActiveInactiveCounts GetActiveInactiveCounts() const;

  void Dump() TA_EXCL(lock_);

  // Returns a global count of all pages compressed at the point of LRU change. This is a global
  // method and will include stats from every PageQueues that has been instantiated.
  static uint64_t GetLruPagesCompressed();

  // Enables reclamation of anonymous pages by causing them to be placed into the reclaimable queue
  // instead of the dedicated anonymous queue. The |zero_forks| parameter controls whether the
  // anonymous zero forks should also go into the general reclaimable queue or not.
  // Any pages already placed into the anonymous queues will be moved over, and there is no way to
  // disable this once enabled.
  void EnableAnonymousReclaim(bool zero_forks);

  // Returns whether or not the reclaim queues only include pager backed pages or not.
  bool ReclaimIsOnlyPagerBacked() const { return !anonymous_is_reclaimable_; }

  // These query functions are marked Debug as it is generally a racy way to determine a pages state
  // and these are exposed for the purpose of writing tests or asserts against the pagequeue.

  // This takes an optional output parameter that, if the function returns true, will contain the
  // index of the queue that the page was in.
  bool DebugPageIsReclaim(const vm_page_t* page, size_t* queue = nullptr) const;
  bool DebugPageIsReclaimIsolate(const vm_page_t* page) const;
  bool DebugPageIsPagerBackedDirty(const vm_page_t* page) const;
  bool DebugPageIsAnonymous(const vm_page_t* page) const;
  bool DebugPageIsAnonymousZeroFork(const vm_page_t* page) const;
  bool DebugPageIsAnyAnonymous(const vm_page_t* page) const;
  bool DebugPageIsWired(const vm_page_t* page) const;
  bool DebugPageIsHighPriority(const vm_page_t* page) const;

  // These methods are public so that the scanner can call. Once the scanner is an object that can
  // be friended, and not a collection of anonymous functions, these can be made private.

  // Creates any threads for queue management. This needs to be done separately to construction as
  // there is a recursive dependency where creating threads will need to manipulate pages, which
  // will call back into the page queues.
  // Delaying thread creation is fine as these threads are purely for aging and eviction management,
  // which is not needed during early kernel boot.
  // Failure to start the threads may cause operations such as RotatePagerBackedQueues to block
  // indefinitely as they might attempt to offload work to a nonexistent thread. This issue is only
  // relevant for unittests that may wish to avoid starting the threads for some tests.
  // It is the responsibility of the caller to only call this once, otherwise it will panic.
  void StartThreads(zx_duration_mono_t min_mru_rotate_time, zx_duration_mono_t max_mru_rotate_time);

  // Initializes and starts the debug compression, which attempts to immediately compress a random
  // subset of pages added to the page queues. It is an error to call this if there is no compressor
  // or if not running in debug mode.
  void StartDebugCompressor();

  // Sets the active ratio multiplier.
  void SetActiveRatioMultiplier(uint32_t multiplier);

  // Describes any action to take when processing the LRU queue. This is applied to pages that would
  // otherwise have to be moved from the old LRU queue into the isolate queue.
  enum class LruAction {
    None,
    EvictOnly,
    CompressOnly,
    EvictAndCompress,
  };
  void SetLruAction(LruAction action);

  // Controls to enable and disable the active aging system. These must be called alternately and
  // not in parallel. That is, it is an error to call DisableAging twice without calling EnableAging
  // in between. Similar for EnableAging.
  void DisableAging() TA_EXCL(lock_);
  void EnableAging() TA_EXCL(lock_);

  // Register an Event that will be signalled every time aging occurs. This can be used to know if
  // if PeekReclaim might now return items (due to aging having occurred) where it had previously
  // ceased.
  // Only a single Event may be registered at a time and the Event is assumed to live as long as the
  // PageQueues object. A nullptr can be passed in to unregister an Event, otherwise it is an error
  // to attempt to register over the top of an existing event.
  void SetAgingEvent(Event* event);

  // Debug methods to retrieve a reference to any lru and mru threads. These are intended for use
  // during tests / debugging and hence bypass the lock normally needed to read these members. It is
  // up to the caller to know if these objects are alive or not.
  Thread* DebugGetLruThread() TA_NO_THREAD_SAFETY_ANALYSIS { return lru_thread_; }
  Thread* DebugGetMruThread() TA_NO_THREAD_SAFETY_ANALYSIS { return mru_thread_; }

 private:
  // Specifies the indices for both the page_queues_ and the page_queue_counts_
  enum PageQueue : uint8_t {
    PageQueueNone = 0,
    PageQueueAnonymous,
    PageQueueWired,
    PageQueueHighPriority,
    PageQueueAnonymousZeroFork,
    PageQueuePagerBackedDirty,
    PageQueueFailedReclaim,
    PageQueueReclaimIsolate,
    PageQueueReclaimBase,
    PageQueueReclaimLast = PageQueueReclaimBase + kNumReclaim - 1,
    PageQueueNumQueues,
  };

  // Ensure that the reclaim queue counts are always at the end.
  static_assert(PageQueueReclaimLast + 1 == PageQueueNumQueues);

  // The page queue index, unlike the full generation count, needs to be able to fit inside a
  // uint8_t in the vm_page_t.
  static_assert(PageQueueNumQueues < 256);

  // Converts free running generation to reclaim queue.
  static constexpr PageQueue gen_to_queue(uint64_t gen) {
    return static_cast<PageQueue>((gen % kNumReclaim) + PageQueueReclaimBase);
  }

  // Checks if a candidate reclaim page queue would be valid given a specific lru and mru
  // queue.
  static constexpr bool queue_is_valid(PageQueue page_queue, PageQueue lru, PageQueue mru) {
    DEBUG_ASSERT(page_queue >= PageQueueReclaimBase);
    if (lru <= mru) {
      return page_queue >= lru && page_queue <= mru;
    } else {
      return page_queue <= mru || page_queue >= lru;
    }
  }

  // Returns whether this queue is reclaimable, and hence can be active or inactive. If this
  // returns false then it is guaranteed that both |queue_is_active| and |queue_is_inactive| would
  // return false.
  static constexpr bool queue_is_reclaim(PageQueue page_queue) {
    // We check against the the Isolate queue and not the base queue so that accessing a page can
    // move it from the Isolate list into the LRU queues. To keep this case efficient we require
    // that the Isoalte queue be directly before the LRU queues.
    static_assert(PageQueueReclaimIsolate + 1 == PageQueueReclaimBase);

    // Ensure that the Dirty queue comes before the smallest queue that would return true for this
    // function. This function is used for computing active/inactive sets for the purpose of
    // eviction, and dirty pages cannot be evicted. The Dirty queue also needs to come before the
    // Isolate queue so that MarkAccessed does not try to move the page to the MRU queue on
    // access.
    static_assert(PageQueuePagerBackedDirty < PageQueueReclaimIsolate);

    return page_queue >= PageQueueReclaimIsolate;
  }

  // Calculates the age of a queue against a given mru, with 0 meaning page_queue==mru
  // This is only meaningful to call on reclaimable queues.
  static constexpr uint queue_age(PageQueue page_queue, PageQueue mru) {
    DEBUG_ASSERT(page_queue >= PageQueueReclaimBase);
    if (page_queue <= mru) {
      return mru - page_queue;
    } else {
      return (static_cast<uint>(kNumReclaim) - page_queue) + mru;
    }
  }

  // Returns whether the given page queue would be considered active against a given mru.
  // This is valid to call on any page queue, not just reclaimable ones, and as such this returning
  // false does not imply the queue is inactive.
  static constexpr bool queue_is_active(PageQueue page_queue, PageQueue mru) {
    if (page_queue < PageQueueReclaimBase) {
      return false;
    }
    return queue_age(page_queue, mru) < kNumActiveQueues;
  }

  // Returns whether the given page queue would be considered inactive against a given mru.
  // This is valid to call on any page queue, not just reclaimable ones, and as such this returning
  // false does not imply the queue is active.
  static constexpr bool queue_is_inactive(PageQueue page_queue, PageQueue mru) {
    // The Isolate queue does not have an age, and so we cannot call queue_age on it, but it should
    // definitely be considered part of the inactive set.
    if (page_queue == PageQueueReclaimIsolate) {
      return true;
    }
    if (page_queue < PageQueueReclaimBase) {
      return false;
    }
    return queue_age(page_queue, mru) >= kNumActiveQueues;
  }

  PageQueue mru_gen_to_queue() const {
    return gen_to_queue(mru_gen_.load(ktl::memory_order_relaxed));
  }

  PageQueue lru_gen_to_queue() const {
    return gen_to_queue(lru_gen_.load(ktl::memory_order_relaxed));
  }

  // This processes the LRU queue with the goal of increasing the lru_gen_ to the target_gen. It
  // achieves this by walking all the pages in the queue and doing one of the following:
  //   1. For pages that have a newest accessed time and are in the wrong queue, are moved into the
  //      correct queue.
  //   2. For pages that are in the correct queue, and |isolate| is true, they are moved to the
  //      Isolate queue.
  //   3. For pages that are in the correct queue, and |isolate| is false, they are moved to
  //      the queue of |target_gen|.
  void ProcessLruQueue(uint64_t target_gen, bool isolate);

  // Processes the Isolate list with the goal to move any pages into the correct list, if their
  // queue is no longer the Isolate list. If |peek| contains a value then only the isolate lists up
  // to and including that value are processed, and the first page found is returned.
  ktl::optional<VmoBacklink> ProcessIsolateList(ktl::optional<size_t> peek) TA_EXCL(lock_);

  // Helpers for adding and removing to the queues. All of the public Set/Move/Remove operations
  // are convenience wrappers around these.
  void RemoveLockedList(vm_page_t* page) TA_REQ(list_lock_);
  void SetQueueBacklinkLockedList(vm_page_t* page, void* object, uintptr_t page_offset,
                                  PageQueue queue) TA_REQ(list_lock_);
  void MoveToQueueLockedList(vm_page_t* page, PageQueue queue) TA_REQ(list_lock_);

  // Potentially calls |CheckActiveRatioAgingLocked| based on the kActiveInactiveErrorMargin.
  // |pages| indicates how many pages might have changed queue, and hence how much the ratio could
  // have changed by.
  void MaybeCheckActiveRatioAging(size_t pages) TA_EXCL(lock_);
  void MaybeCheckActiveRatioAgingLocked(size_t pages) TA_REQ(lock_);

  // Internal helper for shutting down any threads created in |StartThreads|.
  void StopThreads();

  // Entry point for the thread that will performing aging and increment the mru generation.
  void MruThread();

  // Checks if the active ratio has exceeded the threshold to cause aging, and if so signals the
  // event.
  void CheckActiveRatioAgingLocked() TA_REQ(lock_);

  // Consumes any pending age reason and either returns the reason, or how long till aging will
  // happen. This timeout does not take into account that other changes, namely the active ratio,
  // could cause aging to be necessary before that timeout.
  // Due to the active ratio being sticky, it needs to be reset, which is why this method is called
  // consume. As a result, calling ConsumeAgeReason and then GetAgeReasonLocked could give different
  // results if an active ratio event was consumed and returned by the first call.
  ktl::variant<AgeReason, zx_instant_mono_t> ConsumeAgeReason() TA_EXCL(lock_);

  // Checks if there is any pending age reason that could be consumed. See ConsumeAgeReason for more
  // details.
  ktl::variant<AgeReason, zx_instant_mono_t> GetAgeReasonLocked() const TA_REQ(lock_);

  // Synchronizes with any outstanding aging. This is intended to allow a reclamation process to
  // ensure it is not racing with, and falsely failing to reclaim, the aging thread due to
  // scheduling or other delays.
  void SynchronizeWithAging() TA_EXCL(lock_);

  // Helper method that calculates whether the current active ratio would trigger aging.
  bool IsActiveRatioTriggeringAging() TA_REQ(lock_);

  void LruThread();
  void MaybeTriggerLruProcessing() TA_EXCL(lock_);
  bool NeedsLruProcessingLocked() const TA_REQ(lock_);

  // Returns true if a page is both in one of the Reclaim queues, and succeeds the passed in
  // validator, which takes a fbl::RefPtr<VmCowPages>.
  template <typename F>
  bool DebugPageIsSpecificReclaim(const vm_page_t* page, F validator, size_t* queue) const;

  // Returns true if a page is both in the specified |queue|, and succeeds the passed in validator,
  // which takes a fbl::RefPtr<VmCowPages>.
  template <typename F>
  bool DebugPageIsSpecificQueue(const vm_page_t* page, PageQueue queue, F validator) const;

  void AdvanceIsolateCursorIf(vm_page_t* page) TA_REQ(list_lock_) {
    if (page == isolate_cursor_.page) {
      isolate_cursor_.page =
          list_next_type(isolate_cursor_.list, &page->queue_node, vm_page_t, queue_node);
    }
  }

  // Records that |pages| have potentially changed queue impacting the active/inactive ratio, and
  // returns |true| if checking the active ratio can be skipped.
  bool RecordActiveRatioSkips(size_t pages) {
    // Add the pages to the skip count and check if our specific addition caused the count to cross
    // the threshold. This prevents a thundering herd of threads all noticing once the count passes
    // the threshold.
    uint64_t old_count = lazy_active_ratio_aging_skips_.fetch_add(pages);
    if (unlikely(old_count < kActiveInactiveErrorMargin &&
                 old_count + pages >= kActiveInactiveErrorMargin)) {
      // Reset the skips counter to zero. This possibly loses some counts, but as the active ratio
      // has not yet been checked, this is fine.
      lazy_active_ratio_aging_skips_ = 0;
      return false;
    }
    return true;
  }

  // The list_lock_ is used to protect the linked lists queues as these cannot be implemented with
  // atomics. A few related members are also protected with this lock, such as the isolate_cursor.
  // The purpose of this separate spinlock, compared to the general lock_, is so that latency
  // sensitive operations, such as adding / removing pages, that only need to modify the list, can
  // happen without false contention with other page queues operations.
  DECLARE_SPINLOCK(PageQueues) mutable list_lock_;

  // General lock used to protect all logic and members that are not part of critical latency
  // sensitive operations.
  // Where both locks need to be acquire lock_, being a mutex, must be acquired prior to the
  // list_lock_.
  DECLARE_CRITICAL_MUTEX(PageQueues) mutable lock_;

  Event* aging_event_ TA_GUARDED(lock_) = nullptr;

  // This Event is a binary semaphore and is used to control aging. Is acquired by the aging thread
  // when it performs aging, and can be acquired separately to block aging. For this purpose it
  // needs to start as being initially signalled.
  AutounsignalEvent aging_token_{true};
  // Flag used to catch programming errors related to double enabling or disabling aging.
  ktl::atomic<bool> aging_disabled_ = false;

  // Time at which the mru_gen_ was last incremented.
  ktl::atomic<zx_instant_mono_t> last_age_time_ = ZX_TIME_INFINITE_PAST;
  // Reason the last aging event happened, this is purely for informational/debugging purposes.
  // Initialized to Timeout as a somewhat arbitrary choice.
  AgeReason last_age_reason_ TA_GUARDED(lock_) = AgeReason::Timeout;
  // Used to signal the aging thread that the active ratio has changed sufficiently that aging might
  // be required. Due to other factors, such as a min timeouts, races, etc, this being signaled does
  // not mean aging will happen.
  AutounsignalEvent aging_active_ratio_event_;
  // Tracks whether the active ratio has been tripped and should contribute as an aging trigger.
  // This is stored as a boolean so that it is sticky in the advent of a race with additional
  // modifications to the page queues. Were this not sticky then, in the absence of a debounce
  // threshold, we could repeatedly trigger the active ratio on and off, causing the aging thread
  // to repeatedly wake up, miss the trigger, and do nothing.
  bool active_ratio_triggered_ TA_GUARDED(lock_) = false;
  // Used to signal the lru thread that it should wake up and check if the lru queue needs
  // processing.
  AutounsignalEvent lru_event_;

  // Tracks whether there is a pending aging event that will happen that can be waited on. This is a
  // raw Event, and not an AutounsignalEvent, as it is a level triggered signal. The signal itself,
  // and hence any calls to Signal or Unsignal on the Event, must be coordinated by under the lock_
  // to ensure no races. Due to this need to coordinate a PendingSignalEvent cannot be used, and
  // preemption must be disabled to allow for manipulating the event with lock_ held.
  // This event itself gets set/cleared in both ConsumeAgeReason and SynchronizeWithAging based on
  // whether this is any aging reason present.
  Event no_pending_aging_signal_{true};

  // What to do with pages when processing the LRU queue.
  LruAction lru_action_ TA_GUARDED(lock_) = LruAction::None;

  // The page queues are placed into an array, indexed by page queue, for consistency and uniformity
  // of access. This does mean that the list for PageQueueNone does not actually have any pages in
  // it, and should always be empty.
  // The reclaimable queues are the more complicated as, unlike the other categories, pages can be
  // in one of the queues, and can move around. The reclaimable queues themselves store pages that
  // are roughly grouped by their last access time. The relationship is not precise as pages are not
  // moved between queues unless it becomes strictly necessary. This is in contrast to the queue
  // counts that are always up to date.
  //
  // What this means is that the vm_page::page_queue index is always up to do date, and the
  // page_queue_counts_ represent an accurate count of pages with that vm_page::page_queue index,
  // but counting the pages actually in the linked list may not yield the correct number.
  //
  // New reclaimable pages are always placed into the queue associated with the MRU generation. If
  // they get accessed the vm_page_t::page_queue gets updated along with the counts. At some point
  // the LRU queue will get processed (see |ProcessIsolateAndLruQueues|) and this will cause pages
  // to get relocated to their correct list.
  //
  // Consider the following example:
  //
  //  LRU  MRU            LRU  MRU            LRU   MRU            LRU   MRU        MRU  LRU
  //    |  |                |  |                |     |              |     |            |  |
  //    |  |    Insert A    |  |    Age         |     |  Touch A     |     |  Age       |  |
  //    V  v    Queue=2     v  v    Queue=2     v     v  Queue=3     v     v  Queue=3   v  v
  // [][ ][ ][] -------> [][ ][a][] -------> [][ ][a][ ] -------> [][ ][a][ ] -------> [ ][ ][a][]
  //
  // At this point page A, in its vm_page_t, has its queue marked as 3, and the page_queue_counts
  // are {0,0,1,0}, but the page itself remains in the linked list for queue 2. If the LRU queue is
  // then processed to increment it we would do.
  //
  //  MRU  LRU             MRU  LRU            MRU    LRU
  //    |  |                 |    |              |      |
  //    |  |       Move LRU  |    |    Move LRU  |      |
  //    V  v       Queue=3   v    v    Queue=3   v      v
  //   [ ][ ][a][] -------> [ ][][a][] -------> [][ ][][a]
  //
  // In the second processing of the LRU queue it gets noticed that the page, based on
  // vm_page_t::page_queue, is in the wrong queue and gets moved into the correct one.
  //
  // For specifics on how LRU and MRU generations map to LRU and MRU queues, see comments on
  // |lru_gen_| and |mru_gen_|.
  ktl::array<list_node_t, PageQueueNumQueues> page_queues_ TA_GUARDED(list_lock_);

  // When a page is in the PageQueueReclaimIsolate state, instead of being in the page_queues_ list
  // it is in, potentially one of several different, isolate_queuse_ lists. This is just an
  // implementation simplification as there's no need to 'save' the memory of the unused list_node_t
  // in the other page_queues_ array.
  ktl::array<list_node_t, kNumIsolateQueues> isolate_queues_ TA_GUARDED(list_lock_);

  // The generation counts are monotonic increasing counters and used to represent the effective age
  // of the oldest and newest reclaimable queues. The page queues themselves are treated as a fixed
  // size circular buffer that the generations map onto (see definition of |gen_to_queue|).This
  // means all pages in the system have an age somewhere in [lru_gen_, mru_gen_] and so the lru and
  // mru generations cannot drift apart by more than kNumPagerBacked, otherwise there would not be
  // enough queues.
  // A pages age being between [lru_gen_, mru_gen_] is not an invariant as MarkAccessed can race and
  // mark pages as being in an invalid queue. This race will get noticed by ProcessLruQueues and
  // the page will get updated at that point to have a valid queue. Importantly, whilst pages can
  // think they are in a queue that is invalid, only valid linked lists in the page_queues_ will
  // ever have pages in them. This invariant is easy to enforce as the page_queues_ are updated
  // under a lock.
  // These are atomic so they can be safely read without the lock held, however they are always
  // modified with the lock hold.
  ktl::atomic<uint64_t> lru_gen_ = 0;
  ktl::atomic<uint64_t> mru_gen_ = kNumReclaim - 1;

  // This semaphore counts the amount of space remaining for the mru to grow before it would overlap
  // with the lru. Having this as a semaphore (even though it can always be calculated from lru_gen_
  // and mru_gen_ above) provides a way for the aging thread to block when it needs to wait for
  // eviction/lru processing to happen. This allows eviction/lru processing to be happening
  // concurrently in a different thread, without requiring it to happen in-line in the aging thread.
  // Without this the aging thread would need to process the LRU queue directly if it needed to make
  // space. Initially, with the lru_gen_ and mru_gen_ definitions above, we start with no space for
  // the mru to grow, so initialize this to 0.
  Semaphore mru_semaphore_ = Semaphore(0);

  // Tracks the counts of pages in each queue in O(1) time complexity. As pages are moved between
  // queues, the corresponding source and destination counts are decremented and incremented,
  // respectively.
  //
  // The first entry of the array is left special: it logically represents pages not in any queue.
  // For simplicity, it is initialized to zero rather than the total number of pages in the system.
  // Consequently, the value of this entry is a negative number with absolute value equal to the
  // total number of pages in all queues. This approach avoids unnecessary branches when updating
  // counts.
  ktl::array<ktl::atomic<size_t>, PageQueueNumQueues> page_queue_counts_ = {};

  // Count for how many pages have moved queue without us recalculating the active ratio. This is a
  // RelaxedAtomic to allow for completely skipping lock acquisition in MarkAccessed, except when
  // the ratio actually needs to be recalculated.
  RelaxedAtomic<uint64_t> lazy_active_ratio_aging_skips_ = 0;

  // Track the mru and lru threads and have a signalling mechanism to shut them down.
  ktl::atomic<bool> shutdown_threads_ = false;
  Thread* mru_thread_ TA_GUARDED(lock_) = nullptr;
  Thread* lru_thread_ TA_GUARDED(lock_) = nullptr;

  // Debug compressor is only available when debug asserts are also enabled. This ensures it can
  // never have an impact on production builds.
#if DEBUG_ASSERT_IMPLEMENTED
  ktl::unique_ptr<VmDebugCompressor> debug_compressor_ TA_GUARDED(list_lock_);
#endif

  // Queue rotation parameters. These are not locked as they are only read by the mru thread, and
  // are set before the mru thread is started.
  zx_duration_mono_t min_mru_rotate_time_;
  zx_duration_mono_t max_mru_rotate_time_;

  // Determines if anonymous zero page forks are placed in the zero fork queue or in the reclaimable
  // queue.
  RelaxedAtomic<bool> zero_fork_is_reclaimable_ = false;

  // Determines if anonymous pages are placed in the reclaimable queues, or in their own non aging
  // anonymous queues.
  RelaxedAtomic<bool> anonymous_is_reclaimable_ = false;

  // Current active ratio multiplier.
  int64_t active_ratio_multiplier_ TA_GUARDED(lock_) = 0;

  // In order to process all the items in the Isolate list, whilst still being able to periodically
  // drop the lock to avoid a long running operation, we use a cursor to record the current page
  // being examined. Any operation that removes a page from the Isolate list must check if its
  // removing this page, and advance it to the next page in the list if it is. This is automated by
  // calling AdvanceDontNeedCursorIf.
  struct {
    vm_page_t* page = nullptr;
    list_node_t* list = nullptr;
  } isolate_cursor_ TA_GUARDED(list_lock_);

  // There is only a single cursor and this lock acts as a resource control for it. This process
  // needs to be done on LRU rotation / when peeking pages for reclamation, which need to be
  // synchronized anyway so having this additional lock does not impact parallelism.
  // The actual object member |isolate_cursor| is not guarded by this lock, since it needs to be
  // read, and potentially updated, specifically by other threads who do not own the logical cursor.
  DECLARE_MUTEX(PageQueues) isolate_cursor_lock_;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_PAGE_QUEUES_H_
