// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "vm/vm_cow_pages.h"

#include <lib/arch/intrin.h>
#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/fit/defer.h>
#include <trace.h>

#include <kernel/range_check.h>
#include <ktl/move.h>
#include <ktl/type_traits.h>
#include <lk/init.h>
#include <vm/compression.h>
#include <vm/discardable_vmo_tracker.h>
#include <vm/fault.h>
#include <vm/page.h>
#include <vm/physmap.h>
#include <vm/pmm.h>
#include <vm/stack_owned_loaned_pages_interval.h>
#include <vm/vm_object.h>
#include <vm/vm_object_paged.h>
#include <vm/vm_page_list.h>

#include "ktl/optional.h"
#include "vm_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE VM_GLOBAL_TRACE(0)

// add expensive code to do a full validation of the VMO at various points.
#define VMO_VALIDATION (0 || (LK_DEBUGLEVEL > 2))

// Assertion that is only enabled if VMO_VALIDATION is enabled.
#define VMO_VALIDATION_ASSERT(x) \
  do {                           \
    if (VMO_VALIDATION) {        \
      ASSERT(x);                 \
    }                            \
  } while (0)

// Add not-as-expensive code to do some extra validation at various points.  This is off in normal
// debug builds because it can add O(n) validation to an O(1) operation, so can still make things
// slower, despite not being as slow as VMO_VALIDATION.
#define VMO_FRUGAL_VALIDATION (0 || (LK_DEBUGLEVEL > 2))

// Assertion that is only enabled if VMO_FRUGAL_VALIDATION is enabled.
#define VMO_FRUGAL_VALIDATION_ASSERT(x) \
  do {                                  \
    if (VMO_FRUGAL_VALIDATION) {        \
      ASSERT(x);                        \
    }                                   \
  } while (0)

namespace {

KCOUNTER(vm_vmo_high_priority, "vm.vmo.high_priority")
KCOUNTER(vm_vmo_no_reclamation_strategy, "vm.vmo.no_reclamation_strategy")
KCOUNTER(vm_vmo_dont_need, "vm.vmo.dont_need")
KCOUNTER(vm_vmo_always_need, "vm.vmo.always_need")
KCOUNTER(vm_vmo_always_need_skipped_reclaim, "vm.vmo.always_need_skipped_reclaim")
KCOUNTER(vm_vmo_compression_zero_slot, "vm.vmo.compression.zero_empty_slot")
KCOUNTER(vm_vmo_compression_marker, "vm.vmo.compression_zero_marker")
KCOUNTER(vm_vmo_discardable_failed_reclaim, "vm.vmo.discardable_failed_reclaim")
KCOUNTER(vm_vmo_range_update_from_parent_skipped, "vm.vmo.range_updated_from_parent.skipped")
KCOUNTER(vm_vmo_range_update_from_parent_performed, "vm.vmo.range_updated_from_parent.performed")

template <typename T>
uint32_t GetShareCount(T p) {
  DEBUG_ASSERT(p->IsPageOrRef());

  uint32_t share_count = 0;
  if (p->IsPage()) {
    share_count = p->Page()->object.share_count;
  } else if (p->IsReference()) {
    share_count = Pmm::Node().GetPageCompression()->GetMetadata(p->Reference());
  }

  return share_count;
}

void ZeroPage(paddr_t pa) {
  void* ptr = paddr_to_physmap(pa);
  DEBUG_ASSERT(ptr);

  arch_zero_page(ptr);
}

void ZeroPage(vm_page_t* p) {
  paddr_t pa = p->paddr();
  ZeroPage(pa);
}

bool IsZeroPage(vm_page_t* p) {
  uint64_t* base = (uint64_t*)paddr_to_physmap(p->paddr());
  for (int i = 0; i < PAGE_SIZE / (int)sizeof(uint64_t); i++) {
    if (base[i] != 0)
      return false;
  }
  return true;
}

void InitializeVmPage(vm_page_t* p) {
  DEBUG_ASSERT(p);
  DEBUG_ASSERT(!list_in_list(&p->queue_node));
  // Page should be in the ALLOC state so we can transition it to the OBJECT state.
  DEBUG_ASSERT(p->state() == vm_page_state::ALLOC);
  p->set_state(vm_page_state::OBJECT);
  p->object.share_count = 0;
  p->object.pin_count = 0;
  p->object.always_need = 0;
  p->object.dirty_state = uint8_t(VmCowPages::DirtyState::Untracked);
}

inline uint64_t CheckedAdd(uint64_t a, uint64_t b) {
  uint64_t result;
  bool overflow = add_overflow(a, b, &result);
  DEBUG_ASSERT(!overflow);
  return result;
}

inline uint64_t ClampedLimit(uint64_t offset, uint64_t limit, uint64_t max_limit) {
  // Return a clamped `limit` value such that `offset + clamped_limit <= max_limit`.
  // If `offset > max_limit` to begin with, then clamp `limit` to 0 to avoid underflow.
  //
  // This is typically used to update a child node's parent limit when its parent is resized or the
  // child moves to a new parent. This guaranatees that the child cannot see any ancestor content
  // beyond what it could before the resize or move operation.
  uint64_t offset_limit = CheckedAdd(offset, limit);
  return ktl::max(ktl::min(offset_limit, max_limit), offset) - offset;
}

ktl::optional<vm_page_t*> MaybeDecompressReference(VmCompression* compression,
                                                   VmCompression::CompressedRef ref) {
  if (auto maybe_page_and_metadata = compression->MoveReference(ref)) {
    InitializeVmPage(maybe_page_and_metadata->page);
    // Ensure the share count is propagated from the compressed page.
    maybe_page_and_metadata->page->object.share_count = maybe_page_and_metadata->metadata;

    return maybe_page_and_metadata->page;
  }

  return ktl::nullopt;
}

void FreeReference(VmPageOrMarker::ReferenceValue content) {
  VmCompression* compression = Pmm::Node().GetPageCompression();
  DEBUG_ASSERT(compression);
  compression->Free(content);
}

}  // namespace

// Helper class for collecting pages to performed batched Removes from the page queue to not incur
// its spinlock overhead for every single page. Pages that it removes from the page queue get placed
// into a provided list. Note that pages are not moved into the list until *after* Flush has been
// called and Flush must be called prior to object destruction.
//
// This class has a large internal array and should be marked uninitialized.
class BatchPQRemove {
 public:
  BatchPQRemove(list_node_t* freed_list) : freed_list_(freed_list) {}
  ~BatchPQRemove() { DEBUG_ASSERT(count_ == 0); }
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(BatchPQRemove);

  // Add a page to the batch set. Automatically calls |Flush| if the limit is reached.
  void Push(vm_page_t* page) {
    DEBUG_ASSERT(page);
    ASSERT(page->object.pin_count == 0);
    DEBUG_ASSERT(count_ < kMaxPages);
    pages_[count_] = page;
    count_++;
    if (count_ == kMaxPages) {
      Flush();
    }
  }

  // Removes any content from the supplied |page_or_marker| and either calls |Push| or otherwise
  // frees it. Always leaves the |page_or_marker| in the empty state.
  // Automatically calls |Flush| if the limit on pages is reached.
  void PushContent(VmPageOrMarker* page_or_marker) {
    if (page_or_marker->IsPage()) {
      Push(page_or_marker->ReleasePage());
    } else if (page_or_marker->IsReference()) {
      // TODO(https://fxbug.dev/42138396): Consider whether it is worth batching these.
      FreeReference(page_or_marker->ReleaseReference());
    } else {
      *page_or_marker = VmPageOrMarker::Empty();
    }
  }

  // Performs |Remove| on any pending pages. This allows you to know that all pages are in the
  // original list so that you can do operations on the list.
  void Flush() {
    if (count_ > 0) {
      pmm_page_queues()->RemoveArrayIntoList(pages_, count_, freed_list_);
      freed_count_ += count_;
      count_ = 0;
    }
  }

  // Returns the number of pages that were added to |freed_list_| by calls to Flush(). The
  // |freed_count_| counter keeps a running count of freed pages as they are removed and added to
  // |freed_list_|, avoiding having to walk |freed_list_| to compute its length.
  size_t freed_count() const { return freed_count_; }

  // Produces a callback suitable for passing to VmPageList::RemovePages that will |PushContent| all
  // items.
  auto RemovePagesCallback() {
    return [this](VmPageOrMarker* p, uint64_t off) {
      PushContent(p);
      return ZX_ERR_NEXT;
    };
  }

 private:
  // The value of 64 was chosen as there is minimal performance gains originally measured by using
  // higher values. There is an incentive on this being as small as possible due to this typically
  // being created on the stack, and our stack space is limited.
  static constexpr size_t kMaxPages = 64;

  size_t count_ = 0;
  size_t freed_count_ = 0;
  vm_page_t* pages_[kMaxPages];
  list_node_t* freed_list_ = nullptr;
};

// Helper class for collecting pages to perform batched calls of |ChangeObjectOffset| on the page
// queue in order to avoid incurring its spinlock overhead for every single page. Note that pages
// are not modified until *after* Flush has been called and Flush must be called prior to object
// destruction.
//
// This class has a large internal array and should be marked uninitialized.
class BatchPQUpdateBacklink {
 public:
  explicit BatchPQUpdateBacklink(VmCowPages* object) : object_(object) {}
  ~BatchPQUpdateBacklink() { DEBUG_ASSERT(count_ == 0); }
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(BatchPQUpdateBacklink);

  // Add a page to the batch set. Automatically calls |Flush| if the limit is reached.
  void Push(vm_page_t* page, uint64_t offset) {
    DEBUG_ASSERT(page);
    DEBUG_ASSERT(count_ < kMaxPages);

    pages_[count_] = page;
    offsets_[count_] = offset;
    count_++;

    if (count_ == kMaxPages) {
      Flush();
    }
  }

  // Performs |ChangeObjectOffset| on any pending pages.
  void Flush() {
    if (count_ > 0) {
      pmm_page_queues()->ChangeObjectOffsetArray(pages_, object_, offsets_, count_);
      count_ = 0;
    }
  }

 private:
  // Align the batch size here with the overall PageQueues batch size.
  // We measured no performance gains from using larger values and this value should be as small as
  // is reasonable due to this object being stack allocated.
  static constexpr size_t kMaxPages = PageQueues::kMaxBatchSize;

  VmCowPages* object_ = nullptr;

  size_t count_ = 0;
  vm_page_t* pages_[kMaxPages];
  uint64_t offsets_[kMaxPages];
};

bool VmCowRange::IsBoundedBy(uint64_t max) const { return InRange(offset, len, max); }

// Allocates a new page and populates it with the data at |parent_paddr|.
zx_status_t VmCowPages::AllocateCopyPage(paddr_t parent_paddr, list_node_t* alloc_list,
                                         AnonymousPageRequest* request, vm_page_t** clone) {
  DEBUG_ASSERT(request || !(pmm_alloc_flags_ & PMM_ALLOC_FLAG_CAN_WAIT));
  DEBUG_ASSERT(!is_source_supplying_specific_physical_pages());

  vm_page_t* p_clone = nullptr;
  if (alloc_list) {
    p_clone = list_remove_head_type(alloc_list, vm_page, queue_node);
  }

  if (p_clone) {
    InitializeVmPage(p_clone);
  } else {
    zx_status_t status = AllocPage(&p_clone, request);
    if (status != ZX_OK) {
      return status;
    }
    DEBUG_ASSERT(p_clone);
  }

  void* dst = paddr_to_physmap(p_clone->paddr());
  DEBUG_ASSERT(dst);

  if (parent_paddr != vm_get_zero_page_paddr()) {
    // do a direct copy of the two pages
    const void* src = paddr_to_physmap(parent_paddr);
    DEBUG_ASSERT(src);
    memcpy(dst, src, PAGE_SIZE);
  } else {
    // avoid pointless fetches by directly zeroing dst
    arch_zero_page(dst);
  }

  *clone = p_clone;

  return ZX_OK;
}

zx_status_t VmCowPages::AllocUninitializedPage(vm_page_t** page, AnonymousPageRequest* request) {
  paddr_t paddr = 0;
  DEBUG_ASSERT(!is_source_supplying_specific_physical_pages());
  zx_status_t status = CacheAllocPage(pmm_alloc_flags_, page, &paddr);
  if (status == ZX_ERR_SHOULD_WAIT) {
    request->MakeActive();
  }
  return status;
}

zx_status_t VmCowPages::AllocPage(vm_page_t** page, AnonymousPageRequest* request) {
  zx_status_t status = AllocUninitializedPage(page, request);
  if (status == ZX_OK) {
    InitializeVmPage(*page);
  }
  return status;
}

zx_status_t VmCowPages::AllocLoanedPage(vm_page_t** page) {
  uint32_t pmm_alloc_flags = pmm_alloc_flags_;
  // Loaned page allocations will always precisely succeed or fail and the CAN_WAIT flag cannot be
  // combined and so we remove it if it exists.
  pmm_alloc_flags &= ~PMM_ALLOC_FLAG_CAN_WAIT;
  pmm_alloc_flags |= PMM_ALLOC_FLAG_LOANED;
  zx_status_t status = pmm_alloc_page(pmm_alloc_flags, page);
  if (status == ZX_OK) {
    InitializeVmPage(*page);
  }
  return status;
}

zx_status_t VmCowPages::CacheAllocPage(uint alloc_flags, vm_page_t** p, paddr_t* pa) {
  if (!page_cache_) {
    return pmm_alloc_page(alloc_flags, p, pa);
  }

  zx::result result = page_cache_.Allocate(1, alloc_flags);
  if (result.is_error()) {
    return result.error_value();
  }

  vm_page_t* page = list_remove_head_type(&result->page_list, vm_page_t, queue_node);
  DEBUG_ASSERT(page != nullptr);
  DEBUG_ASSERT(result->page_list.is_empty());

  *p = page;
  *pa = page->paddr();
  return ZX_OK;
}

void VmCowPages::CacheFree(list_node_t* list) {
  if (!page_cache_) {
    pmm_free(list);
    return;
  }

  page_cache_.Free(ktl::move(*list));
}

void VmCowPages::CacheFree(vm_page_t* p) {
  if (!page_cache_) {
    pmm_free_page(p);
    return;
  }

  page_cache::PageCache::PageList list;
  list_add_tail(&list, &p->queue_node);

  page_cache_.Free(ktl::move(list));
}

zx_status_t VmCowPages::MakePageFromReference(VmPageOrMarkerRef page_or_mark,
                                              AnonymousPageRequest* page_request) {
  DEBUG_ASSERT(page_or_mark->IsReference());
  VmCompression* compression = Pmm::Node().GetPageCompression();
  DEBUG_ASSERT(compression);

  vm_page_t* p;
  zx_status_t status = AllocPage(&p, page_request);
  if (status != ZX_OK) {
    return status;
  }

  const auto ref = page_or_mark.SwapReferenceForPage(p);
  void* page_data = paddr_to_physmap(p->paddr());
  uint32_t page_metadata;
  compression->Decompress(ref, page_data, &page_metadata);
  // Ensure the share count is propagated from the compressed page.
  p->object.share_count = page_metadata;

  return ZX_OK;
}

zx_status_t VmCowPages::ReplaceReferenceWithPageLocked(VmPageOrMarkerRef page_or_mark,
                                                       uint64_t offset,
                                                       AnonymousPageRequest* page_request) {
  // First replace the ref with a page.
  zx_status_t status = MakePageFromReference(page_or_mark, page_request);
  if (status != ZX_OK) {
    return status;
  }
  IncrementHierarchyGenerationCountLocked();
  // Add the new page to the page queues for tracking. References are by definition not pinned, so
  // we know this is not wired.
  SetNotPinnedLocked(page_or_mark->Page(), offset);
  return ZX_OK;
}

VmCowPages::VmCowPages(const fbl::RefPtr<VmHierarchyState> hierarchy_state_ptr,
                       VmCowPagesOptions options, uint32_t pmm_alloc_flags, uint64_t size,
                       fbl::RefPtr<PageSource> page_source,
                       ktl::unique_ptr<DiscardableVmoTracker> discardable_tracker)
    : VmHierarchyBase(ktl::move(hierarchy_state_ptr)),
      pmm_alloc_flags_(pmm_alloc_flags),
      options_(options),
      size_(size),
      page_source_(ktl::move(page_source)),
      discardable_tracker_(ktl::move(discardable_tracker)) {
  DEBUG_ASSERT(IS_PAGE_ALIGNED(size));
  DEBUG_ASSERT(!(pmm_alloc_flags & PMM_ALLOC_FLAG_LOANED));
}

void VmCowPages::TransitionToAliveLocked() {
  ASSERT(life_cycle_ == LifeCycle::Init);
  life_cycle_ = LifeCycle::Alive;
}

void VmCowPages::MaybeDeadTransitionLocked(Guard<CriticalMutex>& guard) {
  if (!paged_ref_ && children_list_len_ == 0 && life_cycle_ == LifeCycle::Alive) {
    DeadTransition(guard);
  }
}

void VmCowPages::MaybeDeadTransition() {
  Guard<CriticalMutex> guard{lock()};
  MaybeDeadTransitionLocked(guard);
}

void VmCowPages::DeadTransition(Guard<CriticalMutex>& guard) {
  canary_.Assert();
  DEBUG_ASSERT(life_cycle_ == LifeCycle::Alive);

  // To prevent races with a hidden parent creation or merging, it is necessary to hold the lock
  // over the is_hidden and parent_ check and into the subsequent removal call.

  // We'll be making changes to the hierarchy we're part of.
  IncrementHierarchyGenerationCountLocked();

  // At the point of destruction we should no longer have any mappings or children still
  // referencing us, and by extension our priority count must therefore be back to zero.
  DEBUG_ASSERT(high_priority_count_ == 0);
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());

  // If we're not a hidden vmo then we need to remove ourselves from our parent and free any pages
  // that we own.
  if (!is_hidden_locked()) {
    // Clear out all content that we can see. This means dropping references to any pages in our
    // parents, as well as removing any pages in our own page list.
    ReleaseOwnedPagesLocked(0);
    if (parent_) {
      parent_locked().RemoveChildLocked(this);
    }

    if (parent_) {
      // We removed a child from the parent, and so it may also need to be cleaned.
      // Avoid recursing destructors and dead transitions when we delete our parent by using the
      // deferred deletion method. See common in parent else branch for why we can avoid this on a
      // hidden parent.
      if (!parent_locked().is_hidden_locked()) {
        guard.CallUnlocked([parent = ktl::move(parent_)]() mutable {
          VmDeferredDeleter<VmCowPages>::DoDeferredDelete(ktl::move(parent));
        });
      } else {
        parent_locked().MaybeDeadTransitionLocked(guard);
      }
    }
  } else {
    // Most of the hidden vmo's state should have already been cleaned up when it merged
    // itself into its child in ::RemoveChildLocked.
    DEBUG_ASSERT(children_list_len_ == 0);
    DEBUG_ASSERT(page_list_.HasNoPageOrRef());
    // Even though we are hidden we might have a parent. Unlike in the other branch of this if we
    // do not need to perform any deferred deletion. The reason for this is that the deferred
    // deletion mechanism is intended to resolve the scenario where there is a chain of 'one ref'
    // parent pointers that will chain delete. However, with hidden parents we *know* that a
    // hidden parent has two children (and hence at least one other ref to it) and so we cannot be
    // in a one ref chain. Even if N threads all tried to remove children from the hierarchy at
    // once, this would ultimately get serialized through the lock and the hierarchy would go from
    //
    //          [..]
    //           /
    //          A                             [..]
    //         / \                             /
    //        B   E           TO         B    A
    //       / \                        /    / \.
    //      C   D                      C    D   E
    //
    // And so each serialized deletion breaks of a discrete two VMO chain that can be safely
    // finalized with one recursive step.
    if (parent_) {
      DEBUG_ASSERT(!parent_locked().parent_);
      // We explicitly call DeadTransition on our parent (even though we are still a child of it) as
      // otherwise its destructor will run without this transition happening, which is an error.
      // This otherwise does not cause any actual cleanup to happen, since our parent is hidden and
      // will have had all its pages removed already.
      parent_locked().DeadTransition(guard);
    }
  }

  DEBUG_ASSERT(page_list_.IsEmpty());
  // We must Close() after removing pages, so that all pages will be loaned by the time
  // PhysicalPageProvider::OnClose() calls pmm_delete_lender() on the whole physical range.
  if (page_source_) {
    page_source_->Close();
  }

  life_cycle_ = LifeCycle::Dead;
}

VmCowPages::~VmCowPages() {
  // Most of the explicit cleanup happens in DeadTransition() with asserts and some remaining
  // cleanup happening here in the destructor.
  canary_.Assert();
  DEBUG_ASSERT(page_list_.HasNoPageOrRef());
  DEBUG_ASSERT(life_cycle_ != LifeCycle::Alive);
  // The discardable tracker is unlinked explicitly in the destructor to ensure that no RefPtrs can
  // be constructed to the VmCowPages from here. See comment in
  // DiscardableVmoTracker::DebugDiscardablePageCounts that depends upon this being here instead of
  // during the dead transition.
  if (discardable_tracker_) {
    Guard<CriticalMutex> guard{lock()};
    discardable_tracker_->assert_cow_pages_locked();
    discardable_tracker_->RemoveFromDiscardableListLocked();
  }
}

template <typename T>
zx_status_t VmCowPages::ForEveryOwnedHierarchyPageInRangeLocked(T func, uint64_t offset,
                                                                uint64_t size) const {
  return ForEveryOwnedHierarchyPageInRange(this, func, offset, size);
}

template <typename T>
zx_status_t VmCowPages::ForEveryOwnedMutableHierarchyPageInRangeLocked(T func, uint64_t offset,
                                                                       uint64_t size) {
  return ForEveryOwnedHierarchyPageInRange(this, func, offset, size);
}

template <typename S, typename T>
zx_status_t VmCowPages::ForEveryOwnedHierarchyPageInRange(S* self, T func, uint64_t offset,
                                                          uint64_t size) {
  constexpr bool MUTABLE = !ktl::is_const_v<S>;
  static_assert((MUTABLE && ktl::is_same_v<S, VmCowPages>) ||
                (!MUTABLE && ktl::is_same_v<S, const VmCowPages>));

  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(size));

  uint64_t start_in_self = offset;
  uint64_t end_in_self = CheckedAdd(offset, size);
  uint64_t start_in_cur = start_in_self;
  uint64_t end_in_cur = end_in_self;
  S* cur = self;

  while (start_in_self < end_in_self) {
    AssertHeld(cur->lock_ref());

    // Early out if the current node is empty.
    // TODO(https://fxbug.dev/338300943): This early out shouldn't be necessary as long as the
    // VmPageList iterator functions like `ForEveryPageInRange` early out instead. Microbenchmarks
    // slightly regressed when we attempted this though. Investigate adding the `IsEmpty` early-out
    // to the VMPageList methods and removing this block.
    if (cur->page_list_.IsEmpty()) {
      if (cur->is_parent_hidden_locked() && start_in_cur < cur->parent_limit_) {
        // Some of this range within the parent node is still owned by `self`, so walk up and
        // process the range from within the parent instead.
        start_in_cur = start_in_cur + cur->parent_offset_;
        end_in_cur = ktl::min(end_in_cur, cur->parent_limit_) + cur->parent_offset_;
        cur = cur->parent_.get();
      } else {
        // The range within the current node is owned by `self`, but the same range is not owned by
        // `self` within the parent node. Either:
        //  1. The parent is a visible node and owns all its pages, so `self` cannot own them.
        //  2. This range within the parent is not visible to `self` due to the `parent_limit_`, and
        //     thus it cannot be owned by `self` either.
        // Mark the range as processed and restart another walk up from `self`.
        start_in_self += end_in_cur - start_in_cur;
        start_in_cur = start_in_self;
        end_in_cur = end_in_self;
        cur = self;
      }

      continue;
    }

    // We attempt to always inline these lambdas, as its a huge performance benefit and has minimal
    // impact on code size.
    bool stopped_early = false;
    bool walk_up = false;
    auto page_callback = [func, cur, cur_to_self = start_in_cur - start_in_self, &stopped_early](
                             auto p, uint64_t page_offset) __ALWAYS_INLINE {
      zx_status_t status = func(p, cur, page_offset - cur_to_self, page_offset);
      if (status == ZX_ERR_STOP) {
        stopped_early = true;
      }
      return status;
    };
    auto gap_callback = [&cur, &start_in_self, &start_in_cur, &end_in_cur, &walk_up](
                            uint64_t gap_start_offset, uint64_t gap_end_offset) __ALWAYS_INLINE {
      AssertHeld(cur->lock_ref());

      // The gap is empty, so walk up if the parent is accessible from any part of it.
      // Mark the range immediately preceding the gap as processed.
      if (gap_start_offset < cur->parent_limit_) {
        start_in_self += gap_start_offset - start_in_cur;
        start_in_cur = gap_start_offset + cur->parent_offset_;
        end_in_cur = ktl::min(gap_end_offset, cur->parent_limit_) + cur->parent_offset_;
        cur = cur->parent_.get();
        walk_up = true;
        return ZX_ERR_STOP;
      }

      return ZX_ERR_NEXT;
    };

    zx_status_t status = ZX_OK;
    if (cur->is_parent_hidden_locked() && start_in_cur < cur->parent_limit_) {
      // We know the parent is hidden here, so we may need to walk up into it if it's accessible
      // from any empty offset within the range.
      //
      // Otherwise process pages within the range directly owned by `cur`.
      if constexpr (MUTABLE) {
        status = cur->page_list_.RemovePagesAndIterateGaps(page_callback, gap_callback,
                                                           start_in_cur, end_in_cur);
      } else {
        status = cur->page_list_.ForEveryPageAndGapInRange(page_callback, gap_callback,
                                                           start_in_cur, end_in_cur);
      }
    } else {
      // There is either no parent here, or the parent is visible.
      //
      // Visible parents represent cases of unidirectional cloning where the parent owns its pages
      // exclusively, so we don't walk up into them and thus don't need to process any gaps.
      //
      // Processing gaps is expensive due to additional per-page overhead involved in tracking the
      // gaps and intervals, so save time by avoiding that and only processing pages directly owned
      // by `cur`.
      if constexpr (MUTABLE) {
        status = cur->page_list_.RemovePages(page_callback, start_in_cur, end_in_cur);
      } else {
        status = cur->page_list_.ForEveryPageInRange(page_callback, start_in_cur, end_in_cur);
      }
    }
    if (status != ZX_OK) {
      return status;
    }

    // If the page callback wanted to stop early, then do so.
    if (stopped_early) {
      return ZX_OK;
    }

    // If we didn't walk up, then mark the entire range as processed and begin another walk up from
    // `self`.
    if (!walk_up) {
      start_in_self += end_in_cur - start_in_cur;
      start_in_cur = start_in_self;
      end_in_cur = end_in_self;
      cur = self;
    }
  }

  return ZX_OK;
}

bool VmCowPages::DedupZeroPage(vm_page_t* page, uint64_t offset) {
  canary_.Assert();

  Guard<CriticalMutex> guard{lock()};

  // Forbid zero page deduping if this is high priority.
  if (high_priority_count_ != 0) {
    return false;
  }

  // The VmObjectPaged could have been destroyed, or this could be a hidden node. Check if the
  // paged_ref_ is valid first.
  if (paged_ref_) {
    AssertHeld(paged_ref_->lock_ref());
    if (!paged_ref_->CanDedupZeroPagesLocked()) {
      return false;
    }
  }

  // Check this page is still a part of this VMO. object.page_offset could be wrong, but there's no
  // harm in looking up a random slot as we'll then notice it's the wrong page.
  // Also ignore any references since we cannot efficiently scan them, and they should presumably
  // already be deduped.
  // Pinned pages cannot be decommited and so also must not be committed. We must also not decommit
  // pages from kernel VMOs, as the kernel cannot fault them back in, but all kernel pages will be
  // pinned.
  VmPageOrMarkerRef page_or_marker = page_list_.LookupMutable(offset);
  if (!page_or_marker || !page_or_marker->IsPage() || page_or_marker->Page() != page ||
      page->object.pin_count > 0 || (is_page_dirty_tracked(page) && !is_page_clean(page))) {
    return false;
  }

  // We expect most pages to not be zero, as such we will first do a 'racy' zero page check where
  // we leave write permissions on the page. If the page isn't zero, which is our hope, then we
  // haven't paid the price of modifying page tables.
  if (!IsZeroPage(page_or_marker->Page())) {
    return false;
  }

  RangeChangeUpdateLocked(VmCowRange(offset, PAGE_SIZE), RangeChangeOp::RemoveWrite);

  if (IsZeroPage(page_or_marker->Page())) {
    // We stack-own loaned pages from when they're removed until they're freed.
    __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

    // Replace the slot with a marker.
    __UNINITIALIZED auto result =
        BeginAddPageWithSlotLocked(offset, page_or_marker, CanOverwriteContent::NonZero);
    DEBUG_ASSERT(result.is_ok());
    VmPageOrMarker old_page = CompleteAddPageLocked(*result, VmPageOrMarker::Marker(),
                                                    /*do_range_update=*/true);
    DEBUG_ASSERT(old_page.IsPage());

    // Free the old page.
    vm_page_t* released_page = old_page.ReleasePage();
    pmm_page_queues()->Remove(released_page);

    DEBUG_ASSERT(!list_in_list(&released_page->queue_node));
    FreePageLocked(released_page, /*freeing_owned_page=*/true);

    reclamation_event_count_++;
    IncrementHierarchyGenerationCountLocked();
    VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
    VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
    return true;
  }
  return false;
}

zx_status_t VmCowPages::Create(fbl::RefPtr<VmHierarchyState> root_lock, VmCowPagesOptions options,
                               uint32_t pmm_alloc_flags, uint64_t size,
                               ktl::unique_ptr<DiscardableVmoTracker> discardable_tracker,
                               fbl::RefPtr<VmCowPages>* cow_pages) {
  DEBUG_ASSERT(!(options & VmCowPagesOptions::kInternalOnlyMask));
  fbl::AllocChecker ac;
  auto cow = fbl::AdoptRef<VmCowPages>(new (&ac) VmCowPages(ktl::move(root_lock), options,
                                                            pmm_alloc_flags, size, nullptr,
                                                            ktl::move(discardable_tracker)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  if (cow->discardable_tracker_) {
    cow->discardable_tracker_->InitCowPages(cow.get());
  }

  *cow_pages = ktl::move(cow);
  return ZX_OK;
}

zx_status_t VmCowPages::CreateExternal(fbl::RefPtr<PageSource> src, VmCowPagesOptions options,
                                       fbl::RefPtr<VmHierarchyState> root_lock, uint64_t size,
                                       fbl::RefPtr<VmCowPages>* cow_pages) {
  DEBUG_ASSERT(!(options & VmCowPagesOptions::kInternalOnlyMask));
  fbl::AllocChecker ac;
  auto cow = fbl::AdoptRef<VmCowPages>(new (&ac) VmCowPages(
      ktl::move(root_lock), options, PMM_ALLOC_FLAG_CAN_WAIT, size, ktl::move(src), nullptr));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *cow_pages = ktl::move(cow);
  return ZX_OK;
}

void VmCowPages::ReplaceChildLocked(VmCowPages* old, VmCowPages* new_child) {
  canary_.Assert();

  [[maybe_unused]] VmCowPages* replaced = children_list_.replace(*old, new_child);
  DEBUG_ASSERT(replaced == old);
}

void VmCowPages::DropChildLocked(VmCowPages* child) {
  canary_.Assert();

  [[maybe_unused]] VmCowPages* erased = children_list_.erase(*child);
  DEBUG_ASSERT(erased == child);
  DEBUG_ASSERT(children_list_len_ > 0);
  --children_list_len_;
}

void VmCowPages::AddChildLocked(VmCowPages* child, uint64_t offset, uint64_t parent_limit) {
  canary_.Assert();

  // This function must succeed, as failure here requires the caller to roll back allocations.
  AssertHeld(child->lock_ref());

  // The child should definitely stop seeing into the parent at the limit of its size.
  DEBUG_ASSERT(parent_limit <= child->size_);
  // The child's offsets must not overflow when projected onto the root.
  // Callers should validate this externally and report errors as appropriate.
  const uint64_t root_parent_offset = CheckedAdd(offset, root_parent_offset_);
  CheckedAdd(root_parent_offset, child->size_);

  // Write in the parent view values.
  child->root_parent_offset_ = root_parent_offset;
  child->parent_offset_ = offset;
  child->parent_limit_ = parent_limit;

  // The child's page list should skew by the child's offset relative to the parent. This allows
  // fast copies of page list entries when merging the lists later (entire blocks of entries can be
  // copied at once).
  child->page_list_.InitializeSkew(page_list_.GetSkew(), offset);

  // If the child has a non-zero high priority count, then it is counting as an incoming edge to our
  // count.
  if (child->high_priority_count_ > 0) {
    ChangeSingleHighPriorityCountLocked(1);
  }

  child->parent_ = fbl::RefPtr(this);
  children_list_.push_front(child);
  children_list_len_++;
}

zx_status_t VmCowPages::CreateChildSliceLocked(uint64_t offset, uint64_t size,
                                               fbl::RefPtr<VmCowPages>* cow_slice) {
  canary_.Assert();

  LTRACEF("vmo %p offset %#" PRIx64 " size %#" PRIx64 "\n", this, offset, size);

  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(size));
  // The new slice must lie completely in range of its parent.
  // As offsets within the parent do not overflow when projected onto the root, this check implies
  // that the slice's offsets cannot either.
  DEBUG_ASSERT(CheckedAdd(offset, size) <= size_);

  // If this is a slice re-home this on our parent. Due to this logic we can guarantee that any
  // slice parent is, itself, not a slice.
  // We are able to do this for two reasons:
  //  * Slices are subsets and so every position in a slice always maps back to the paged parent.
  //  * Slices are not permitted to be resized and so nothing can be done on the intermediate parent
  //    that requires us to ever look at it again.
  if (is_slice_locked()) {
    return slice_parent_locked().CreateChildSliceLocked(offset + parent_offset_, size, cow_slice);
  }

  fbl::AllocChecker ac;
  // Slices just need the slice option and default alloc flags since they will propagate any
  // operation up to a parent and use their options and alloc flags.
  auto slice = fbl::AdoptRef<VmCowPages>(new (&ac) VmCowPages(
      hierarchy_state_ptr_, VmCowPagesOptions::kSlice, PMM_ALLOC_FLAG_ANY, size, nullptr, nullptr));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  AssertHeld(slice->lock_ref());

  AddChildLocked(slice.get(), offset, size);

  // Checking this node's hierarchy will also check the parent's hierarchy.
  // It will not check the child's page sharing however, so check that independently.
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  VMO_VALIDATION_ASSERT(slice->DebugValidatePageSharingLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(slice->DebugValidateVmoPageBorrowingLocked());

  *cow_slice = slice;

  return ZX_OK;
}

VmCowPages::ParentAndRange VmCowPages::FindParentAndRangeForCloneLocked(
    VmCowPages* parent, uint64_t offset, uint64_t size, bool parent_must_be_hidden) {
  AssertHeld(parent->lock_ref());
  DEBUG_ASSERT(!parent->is_hidden_locked());

  // The clone's parent limit starts out equal to its size, but it can't exceed the parent's size.
  // This ensures that any clone pages beyond the parent's range get initialized from zeroes.
  uint64_t parent_limit = ClampedLimit(offset, size, parent->size_);

  // Walk up the hierarchy until we find the last node which can correctly be the clone's parent.
  while (VmCowPages* next_parent = parent->parent_.get()) {
    AssertHeld(next_parent->lock_ref());

    // `parent` will always satisfy `parent_must_be_hidden` at this point.
    //
    // If `next_parent` doesn't satisfy `parent_must_be_hidden` then we must use `parent` as the
    // clone's parent, even if it doesn't have any pages for the clone to snapshot.
    if (parent_must_be_hidden && !next_parent->is_hidden_locked()) {
      break;
    }

    // If `parent` owns any pages in the clone's range then we muse use it as the clone's parent.
    // If we continued iterating, the clone couldn't snapshot all ancestor pages that it would be
    // able to if `this` had been the parent.
    if (parent_limit > 0 &&
        parent->page_list_.AnyPagesOrIntervalsInRange(offset, offset + parent_limit)) {
      break;
    }

    // Before the loop the caller validated that the clone's offsets cannot overflow when projected
    // onto the root. Verify this will remain true.
    //
    // Each iteration of this loop must leave the clone's ultimate `root_parent_offset_` unchanged.
    // We will increase the clone's `offset` by the current parent's `parent_offset_` but the new
    // parent's `root_parent_offset_` is smaller by the same amount.
    DEBUG_ASSERT(CheckedAdd(next_parent->root_parent_offset_, parent->parent_offset_) ==
                 parent->root_parent_offset_);

    // To move to `next_parent` we need to translate the clone's window to be relative to it.
    //
    // The clone's last visible offset into `next_parent` cannot exceed `parent`'s parent limit, as
    // it shouldn't be able to see more pages than it could see if `parent` had been the parent.
    parent_limit = ClampedLimit(offset, parent_limit, parent->parent_limit_);
    offset = CheckedAdd(parent->parent_offset_, offset);

    parent = next_parent;
  }

  return ParentAndRange{parent, offset, parent_limit, size};
}

void VmCowPages::CloneParentIntoChildLocked(fbl::RefPtr<VmCowPages> child) {
  canary_.Assert();

  AssertHeld(child->lock_ref());
  // This function is invalid to call if any pages are pinned as the unpin after we change the
  // backlink will not work.
  DEBUG_ASSERT(pinned_page_count_ == 0);

  // We are going to change this node's `paged_ref_` to eventually point to `child` instead.
  // We need to make the new child look equivalent to this node. To do this it inherits this nodes'
  // children, eviction count, high priority count, and size.
  for (auto& c : children_list_) {
    AssertHeld(c.lock_ref());
    c.parent_ = child;
  }
  child->children_list_ = ktl::move(children_list_);
  child->children_list_len_ = children_list_len_;
  children_list_len_ = 0;
  child->reclamation_event_count_ = reclamation_event_count_;
  child->high_priority_count_ = high_priority_count_;
  high_priority_count_ = 0;
  AddChildLocked(child.get(), 0, size_);

  // Time to change the VmCowPages that our paged_ref_ is pointing to.
  // We could only have gotten here from a valid VmObjectPaged since we're trying to create a child.
  // The paged_ref_ should therefore be valid.
  DEBUG_ASSERT(paged_ref_);
  child->paged_ref_ = paged_ref_;
  AssertHeld(paged_ref_->lock_ref());
  DEBUG_ASSERT(child->life_cycle_ == LifeCycle::Init);
  child->life_cycle_ = LifeCycle::Alive;
  [[maybe_unused]] fbl::RefPtr<VmCowPages> previous =
      paged_ref_->SetCowPagesReferenceLocked(ktl::move(child));
  // Validate that we replaced a reference to ourself as we expected, this ensures we can safely
  // drop the refptr without triggering our own destructor, since we know someone else must be
  // holding a refptr to us to be in this function.
  DEBUG_ASSERT(previous.get() == this);
  paged_ref_ = nullptr;
}

zx_status_t VmCowPages::CloneBidirectionalLocked(uint64_t offset, uint64_t size,
                                                 fbl::RefPtr<VmCowPages>* cow_child) {
  canary_.Assert();

  ParentAndRange child_range = FindParentAndRangeForCloneLocked(this, offset, size, true);
  VmCowPages* parent = child_range.parent;
  AssertHeld(parent->lock_ref());

  fbl::AllocChecker ac;
  auto cow_clone = fbl::AdoptRef<VmCowPages>(
      new (&ac) VmCowPages(hierarchy_state_ptr_, VmCowPagesOptions::kNone, pmm_alloc_flags_,
                           child_range.size, nullptr, nullptr));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  AssertHeld(cow_clone->lock_ref());

  // If `parent` is to be the new child's parent then it must become hidden first.
  // That requires cloning it into a new visible child of the (now) hidden parent.
  fbl::RefPtr<VmCowPages> parent_clone;
  if (!parent->is_hidden_locked()) {
    DEBUG_ASSERT(parent == this);  // `parent` must either be hidden already, or be `this` node.
    DEBUG_ASSERT(life_cycle_ == LifeCycle::Alive);

    parent_clone = fbl::AdoptRef<VmCowPages>(new (&ac) VmCowPages(
        hierarchy_state_ptr_, VmCowPagesOptions::kNone, pmm_alloc_flags_, size_, nullptr, nullptr));
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    // The child becomes a full clone of the parent: inheriting its children, paged backref, etc.
    CloneParentIntoChildLocked(parent_clone);
    DEBUG_ASSERT(children_list_len_ == 1);

    // Transition into being the hidden node.
    options_ |= VmCowPagesOptions::kHidden;
  }

  // The COW clone's parent must be hidden because the clone must not see any future parent writes.
  DEBUG_ASSERT(parent->is_hidden_locked());
  parent->AddChildLocked(cow_clone.get(), child_range.parent_offset, child_range.parent_limit);

  VmCompression* compression = Pmm::Node().GetPageCompression();
  // Add references to pages that the COW clone now shares ownership over.
  zx_status_t status = parent->ForEveryOwnedHierarchyPageInRangeLocked(
      [compression](const VmPageOrMarker* p, const VmCowPages* owner, uint64_t cow_clone_offset,
                    uint64_t owner_offset) __ALWAYS_INLINE {
        if (p->IsPage()) {
          p->Page()->object.share_count++;
        } else if (p->IsReference()) {
          VmPageOrMarker::ReferenceValue ref = p->Reference();
          compression->SetMetadata(ref, compression->GetMetadata(ref) + 1);
        } else {
          // Markers do not have references counts.
        }

        return ZX_ERR_NEXT;
      },
      child_range.parent_offset, child_range.parent_limit);
  DEBUG_ASSERT(status == ZX_OK);

  // Checking this node's hierarchy will also check the parent's hierarchy.
  // It will not check either child's page sharing however, so check those independently.
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(parent->DebugValidateVmoPageBorrowingLocked());
  VMO_VALIDATION_ASSERT(cow_clone->DebugValidatePageSharingLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(cow_clone->DebugValidateVmoPageBorrowingLocked());
  if (parent_clone) {
    AssertHeld(parent_clone->lock_ref());
    VMO_VALIDATION_ASSERT(parent_clone->DebugValidatePageSharingLocked());
    VMO_FRUGAL_VALIDATION_ASSERT(parent_clone->DebugValidateVmoPageBorrowingLocked());
  }

  *cow_child = ktl::move(cow_clone);

  return ZX_OK;
}

zx_status_t VmCowPages::CloneUnidirectionalLocked(uint64_t offset, uint64_t size,
                                                  fbl::RefPtr<VmCowPages>* cow_child) {
  canary_.Assert();

  ParentAndRange child_range = FindParentAndRangeForCloneLocked(this, offset, size, false);
  VmCowPages* parent = child_range.parent;
  AssertHeld(parent->lock_ref());

  fbl::AllocChecker ac;
  auto cow_clone = fbl::AdoptRef<VmCowPages>(
      new (&ac) VmCowPages(hierarchy_state_ptr_, VmCowPagesOptions::kNone, pmm_alloc_flags_,
                           child_range.size, nullptr, nullptr));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  AssertHeld(cow_clone->lock_ref());

  // The COW clone's parent must not be hidden because the clone may see future parent writes.
  DEBUG_ASSERT(!parent->is_hidden_locked());
  parent->AddChildLocked(cow_clone.get(), child_range.parent_offset, child_range.parent_limit);

  // Checking this node's hierarchy will also check the parent's hierarchy.
  // It will not check the child's page sharing however, so check that independently.
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(parent->DebugValidateVmoPageBorrowingLocked());
  VMO_VALIDATION_ASSERT(cow_clone->DebugValidatePageSharingLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(cow_clone->DebugValidateVmoPageBorrowingLocked());

  *cow_child = ktl::move(cow_clone);

  return ZX_OK;
}

zx_status_t VmCowPages::CreateCloneLocked(CloneType type, uint64_t offset, uint64_t size,
                                          fbl::RefPtr<VmCowPages>* cow_child) {
  canary_.Assert();

  LTRACEF("vmo %p offset %#" PRIx64 " size %#" PRIx64 "\n", this, offset, size);

  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(size));
  DEBUG_ASSERT(!is_hidden_locked());
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());

  // Upgrade clone type, if possible.
  if (type == CloneType::SnapshotAtLeastOnWrite && !is_snapshot_at_least_on_write_supported()) {
    if (can_snapshot_modified_locked()) {
      type = CloneType::SnapshotModified;
    } else {
      type = CloneType::Snapshot;
    }
  } else if (type == CloneType::SnapshotModified) {
    if (!can_snapshot_modified_locked()) {
      type = CloneType::Snapshot;
    }
  }

  // All validation *must* be performed here prior to construction the VmCowPages, as the
  // destructor for VmCowPages may acquire the lock, which we are already holding.

  switch (type) {
    case CloneType::Snapshot: {
      if (!is_cow_clonable_locked()) {
        return ZX_ERR_NOT_SUPPORTED;
      }

      // If this is non-zero, that means that there are pages which hardware can
      // touch, so the vmo can't be safely cloned.
      // TODO: consider immediately forking these pages.
      if (pinned_page_count_locked()) {
        return ZX_ERR_BAD_STATE;
      }
      break;
    }
    case CloneType::SnapshotAtLeastOnWrite: {
      if (!is_snapshot_at_least_on_write_supported()) {
        return ZX_ERR_NOT_SUPPORTED;
      }

      break;
    }
    case CloneType::SnapshotModified: {
      if (!can_snapshot_modified_locked()) {
        return ZX_ERR_NOT_SUPPORTED;
      }
      if (pinned_page_count_locked()) {
        return ZX_ERR_BAD_STATE;
      }
      break;
    }
  }

  // Offsets within the new clone must not overflow when projected onto the root.
  {
    uint64_t child_root_parent_offset;
    bool overflow;
    overflow = add_overflow(root_parent_offset_, offset, &child_root_parent_offset);
    if (overflow) {
      return ZX_ERR_INVALID_ARGS;
    }
    uint64_t child_root_parent_end;
    overflow = add_overflow(child_root_parent_offset, size, &child_root_parent_end);
    if (overflow) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  // Invalidate everything the clone will be able to see. They're COW pages now,
  // so any existing mappings can no longer directly write to the pages.
  RangeChangeUpdateLocked(VmCowRange(offset, size), RangeChangeOp::RemoveWrite);

  switch (type) {
    case CloneType::Snapshot: {
      return CloneBidirectionalLocked(offset, size, cow_child);
    }
    case CloneType::SnapshotAtLeastOnWrite: {
      return CloneUnidirectionalLocked(offset, size, cow_child);
    }
    case CloneType::SnapshotModified: {
      // If at the root of vmo hierarchy or the slice of the root VMO, create a unidirectional clone
      // TODO(https://fxbug.dev/42074633): consider extinding this to take unidirectional clones of
      // snapshot-modified leaves if possible.
      if (!parent_ || is_slice_locked()) {
        if (is_slice_locked()) {
          // ZX_ERR_NOT_SUPPORTED should have already been returned in the case of a non-root slice
          // clone.
          AssertHeld(parent_->lock_ref());
          DEBUG_ASSERT(!parent_->parent_);
        }
        return CloneUnidirectionalLocked(offset, size, cow_child);
        // Else, take a snapshot.
      } else {
        return CloneBidirectionalLocked(offset, size, cow_child);
      }
    }
  }

  return ZX_ERR_NOT_SUPPORTED;
}

void VmCowPages::RemoveChildLocked(VmCowPages* removed) {
  canary_.Assert();

  AssertHeld(removed->lock_ref());
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());

  if (!is_hidden_locked() || children_list_len_ > 2) {
    DropChildLocked(removed);
    // Things should be consistent after dropping the child.
    VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
    VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
    return;
  }

  // Hidden vmos have 0, 2 or more children. If we had more we would have already returned, and we
  // cannot be here with 0 children, therefore we must have 2, including the one we are removing.
  DEBUG_ASSERT(children_list_len_ == 2);
  DropChildLocked(removed);
  MergeContentWithChildLocked(removed);

  VmCowPages* child = &children_list_.front();
  DEBUG_ASSERT(child);

  // The child which removed itself and led to the invocation should have a reference
  // to us, in addition to child.parent_ which we are about to clear.
  DEBUG_ASSERT(ref_count_debug() >= 2);

  AssertHeld(child->lock_ref());

  // We can have a priority count of at most 1, and only if the remaining child is the one
  // contributing to it.
  DEBUG_ASSERT(high_priority_count_ == 0 ||
               (high_priority_count_ == 1 && child->high_priority_count_ > 0));
  // Similarly if we have a priority count, and we have a parent, then our parent must have a
  // non-zero count.
  if (parent_) {
    DEBUG_ASSERT(high_priority_count_ == 0 || parent_locked().high_priority_count_ != 0);
  }
  // If our child has a non-zero count, then it is propagating a +1 count to us, and we in turn are
  // propagating a +1 count to our parent. In the final arrangement after ReplaceChildLocked then
  // the +1 count child was giving to us needs to go to parent, but as we were already giving a +1
  // count to parent, everything is correct.
  // Although the final hierarchy has correct counts, there is still an assertion in our destructor
  // that our count is zero, so subtract of any count that we might have.
  ChangeSingleHighPriorityCountLocked(-high_priority_count_);

  // Drop the child from our list, but don't recurse back into this function. Then
  // remove ourselves from the clone tree.
  DropChildLocked(child);
  if (parent_) {
    parent_locked().ReplaceChildLocked(this, child);
  }
  child->parent_ = ktl::move(parent_);
  // We have lost our parent which, if we had a parent, could lead us to now be violating the
  // invariant that parent_limit_ being non-zero implies we have a parent. Although this generally
  // should not matter, we have not transitioned to being dead yet, so we should maintain the
  // correct invariants.
  parent_offset_ = parent_limit_ = 0;

  // Things should be consistent after dropping one child and merging with the other.
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_VALIDATION_ASSERT(child->DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(child->DebugValidateVmoPageBorrowingLocked());
}

void VmCowPages::MergeContentWithChildLocked(VmCowPages* removed) {
  canary_.Assert();

  DEBUG_ASSERT(is_hidden_locked());
  // There's no technical reason why this merging code cannot be run if there is a page source,
  // however a bi-directional clone will never have a page source and so in case there are any
  // consequence that have no been considered, ensure we are not in this case.
  DEBUG_ASSERT(!is_source_preserving_page_content());
  DEBUG_ASSERT(children_list_len_ == 1);

  VmCowPages& child = children_list_.front();
  AssertHeld(child.lock_ref());
  // We don't check the hierarchy because it is inconsistent at this point.
  // It will be made consistent by the caller and checked then.
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(child.DebugValidateVmoPageBorrowingLocked());

  const uint64_t merge_start_offset = child.parent_offset_;
  const uint64_t merge_end_offset = child.parent_offset_ + child.parent_limit_;
  VmCompression* compression = Pmm::Node().GetPageCompression();

  __UNINITIALIZED BatchPQUpdateBacklink page_backlink_updater(&child);
  page_list_.MergeRangeOntoAndClear(
      [&](VmPageOrMarkerRef p, uint64_t off) __ALWAYS_INLINE {
        if (p->IsReference()) {
          // A regular reference we can move, a temporary reference we need to turn back into its
          // page so we can move it. To determine if we have a temporary reference we can just
          // attempt to move it, and if it was a temporary reference we will get a page returned.
          if (auto maybe_page = MaybeDecompressReference(compression, p->Reference())) {
            // For simplicity, since this is a very uncommon edge case, just update the page in
            // place in this page list, then move it as a regular page.
            AssertHeld(lock_ref());
            SetNotPinnedLocked(*maybe_page, off);
            VmPageOrMarker::ReferenceValue ref = p.SwapReferenceForPage(*maybe_page);
            ASSERT(compression->IsTempReference(ref));
          }
        }
        // Not an else-if to intentionally perform this if the previous block turned a reference
        // into a page.
        if (p->IsPage()) {
          page_backlink_updater.Push(p->Page(), off);
        }
      },
      child.page_list_, merge_start_offset, merge_end_offset);

  page_backlink_updater.Flush();

  // MergeRangeOntoAndClear clears out the page_list_ for us.
  DEBUG_ASSERT(page_list_.IsEmpty());

  // Adjust the child's offset and limit so it will still see the correct range after it replaces
  // this node. The limit must be adjusted before the offset.
  child.parent_limit_ = ClampedLimit(child.parent_offset_, child.parent_limit_, parent_limit_);
  child.parent_offset_ = CheckedAdd(parent_offset_, child.parent_offset_);

  // The child's last visible offset into this node's parent must be no larger than this node's last
  // visible offset, unless the child can't see anything in this node's parent - in which case its
  // limit will be 0.
  DEBUG_ASSERT(child.parent_limit_ == 0 ||
               (parent_offset_ + parent_limit_ >= child.parent_offset_ + child.parent_limit_));

  // We don't check the hierarchy because it is inconsistent at this point.
  // It will be made consistent by the caller and checked then.
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(child.DebugValidateVmoPageBorrowingLocked());
}

void VmCowPages::DumpLocked(uint depth, bool verbose) const {
  canary_.Assert();

  size_t page_count = 0;
  size_t compressed_count = 0;
  page_list_.ForEveryPage([&page_count, &compressed_count](const auto* p, uint64_t) {
    if (p->IsPage()) {
      page_count++;
    } else if (p->IsReference()) {
      compressed_count++;
    }
    return ZX_ERR_NEXT;
  });

  const char* node_type = "";
  if (is_hidden_locked()) {
    node_type = "(hidden) ";
  } else if (is_slice_locked()) {
    node_type = "(slice) ";
  }

  for (uint i = 0; i < depth; ++i) {
    printf("  ");
  }
  printf("cow_pages %p %ssize %#" PRIx64 " offset %#" PRIx64 " limit %#" PRIx64
         " content pages %zu compressed pages %zu ref %d parent %p\n",
         this, node_type, size_, parent_offset_, parent_limit_, page_count, compressed_count,
         ref_count_debug(), parent_.get());

  if (page_source_) {
    for (uint i = 0; i < depth + 1; ++i) {
      printf("  ");
    }
    printf("page_source preserves content %d\n", is_source_preserving_page_content());
    page_source_->Dump(depth + 1);
  }

  if (verbose) {
    auto f = [depth](const auto* p, uint64_t offset) {
      for (uint i = 0; i < depth + 1; ++i) {
        printf("  ");
      }
      if (p->IsMarker()) {
        printf("offset %#" PRIx64 " zero page marker\n", offset);
      } else if (p->IsPage()) {
        vm_page_t* page = p->Page();
        printf("offset %#" PRIx64 " page %p paddr %#" PRIxPTR " share %" PRIu32 "(%c)\n", offset,
               page, page->paddr(), page->object.share_count, page->object.always_need ? 'A' : '.');
      } else if (p->IsReference()) {
        const uint64_t cookie = p->Reference().value();
        printf("offset %#" PRIx64 " reference %#" PRIx64 " share %" PRIu32 "\n", offset, cookie,
               Pmm::Node().GetPageCompression()->GetMetadata(p->Reference()));
      } else if (p->IsIntervalStart()) {
        printf("offset %#" PRIx64 " page interval start\n", offset);
      } else if (p->IsIntervalEnd()) {
        printf("offset %#" PRIx64 " page interval end\n", offset);
      } else if (p->IsIntervalSlot()) {
        printf("offset %#" PRIx64 " single page interval slot\n", offset);
      }
      return ZX_ERR_NEXT;
    };
    page_list_.ForEveryPage(f);
  }
}

uint32_t VmCowPages::DebugLookupDepthLocked() const {
  canary_.Assert();

  // Count the number of parents we need to traverse to find the root, and call this our lookup
  // depth. Slices don't need to be explicitly handled as they are just a parent.
  uint32_t depth = 0;
  const VmCowPages* cur = this;
  AssertHeld(cur->lock_ref());
  while (cur->parent_) {
    depth++;
    cur = cur->parent_.get();
  }
  return depth;
}

VmCowPages::AttributionCounts VmCowPages::GetAttributedMemoryInRangeLocked(VmCowRange range) const {
  canary_.Assert();

  // Due to the need to manipulate fields in AttributionCounts that only exist based on the #define
  // we cannot use the normal if constexpr guard and instead need a preprocessor guard.
  DEBUG_ASSERT(!is_hidden_locked());

  VmCompression* compression = Pmm::Node().GetPageCompression();

  // Accumulate bytes for all pages and references this node has ownership over.
  AttributionCounts counts;
  zx_status_t status = ForEveryOwnedHierarchyPageInRangeLocked(
      [&](const VmPageOrMarker* p, const VmCowPages* owner, uint64_t this_offset,
          uint64_t owner_offset) {
        auto do_attribution = [&](auto get_share_count, auto& bytes, auto& private_bytes,
                                  auto& scaled_bytes) {
          // The short-circuit condition of (owner == this) greatly improves performance by removing
          // the need to dereference 'random' vm_page_ts/references in the common case, greatly
          // reducing memory stalls. For this reason the get_share_count is a callback, and not a
          // value.
          const uint32_t share_count = (owner == this) ? 0 : get_share_count();
          if (share_count == 0) {
            bytes += PAGE_SIZE;
            private_bytes += PAGE_SIZE;
            scaled_bytes += PAGE_SIZE;
          } else {
            // An unshared (i.e. private) page has a share count of 0, add 1 to get the number of
            // owners and scale the full page by this.
            const vm::FractionalBytes scaled_contribution =
                vm::FractionalBytes(PAGE_SIZE, share_count + 1);
            bytes += PAGE_SIZE;
            scaled_bytes += scaled_contribution;
          }
        };
        if (p->IsPage()) {
          do_attribution([&]() { return p->Page()->object.share_count; }, counts.uncompressed_bytes,
                         counts.private_uncompressed_bytes, counts.scaled_uncompressed_bytes);
        } else if (p->IsReference()) {
          do_attribution([&]() { return compression->GetMetadata(p->Reference()); },
                         counts.compressed_bytes, counts.private_compressed_bytes,
                         counts.scaled_compressed_bytes);
        }
        return ZX_ERR_NEXT;
      },
      range.offset, range.len);
  DEBUG_ASSERT(status == ZX_OK);

  return counts;
}

VmPageOrMarker VmCowPages::AddPageTransaction::Complete(VmPageOrMarker p) {
  VmPageOrMarker ret = slot_.SwapContent(ktl::move(p));
  slot_ = VmPageOrMarkerRef();
  return ret;
}

void VmCowPages::AddPageTransaction::Cancel(VmPageList& pl) {
  DEBUG_ASSERT(slot_);
  if (slot_->IsEmpty()) {
    pl.ReturnEmptySlot(offset_);
  }
  slot_ = VmPageOrMarkerRef();
}

zx::result<VmCowPages::AddPageTransaction> VmCowPages::BeginAddPageWithSlotLocked(
    uint64_t offset, VmPageOrMarkerRef slot, CanOverwriteContent overwrite) {
  canary_.Assert();
  zx_status_t status = CheckOverwriteConditionsLocked(offset, slot, overwrite);
  if (unlikely(status != ZX_OK)) {
    return zx::error(status);
  }
  // Do additinoal checks. The IsOffsetInZeroInterval check is expensive, but the assumption is that
  // this method is not used when is_source_preserving_page_content is true, so the assertion should
  // short circuit.
  DEBUG_ASSERT(!is_source_preserving_page_content() || !slot->IsEmpty() ||
               !page_list_.IsOffsetInZeroInterval(offset));
  return zx::ok(AddPageTransaction(slot, offset, overwrite));
}

zx::result<VmCowPages::AddPageTransaction> VmCowPages::BeginAddPageLocked(
    uint64_t offset, CanOverwriteContent overwrite) {
  canary_.Assert();
  auto interval_handling = VmPageList::IntervalHandling::NoIntervals;
  // If we're backed by a page source that preserves content (user pager), we cannot directly update
  // empty slots in the page list. An empty slot might lie in a sparse zero interval, which would
  // require splitting the interval around the required offset before it can be manipulated.
  if (is_source_preserving_page_content()) {
    // We can overwrite zero intervals if we're allowed to overwrite zeros (or non-zeros).
    interval_handling = overwrite != CanOverwriteContent::None
                            ? VmPageList::IntervalHandling::SplitInterval
                            : VmPageList::IntervalHandling::CheckForInterval;
  }
  auto [slot, is_in_interval] = page_list_.LookupOrAllocate(offset, interval_handling);
  if (is_in_interval) {
    // We should not have found an interval if we were not expecting any.
    DEBUG_ASSERT(interval_handling != VmPageList::IntervalHandling::NoIntervals);
    // Return error if the offset lies in an interval but we cannot overwrite intervals.
    if (interval_handling != VmPageList::IntervalHandling::SplitInterval) {
      // The lookup should not have returned a slot for us to manipulate if it was in an interval
      // that cannot be overwritten, even if that slot was already populated (by an interval
      // sentinel).
      DEBUG_ASSERT(!slot);
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    // If offset was in an interval, we should have an interval slot to overwrite at this point.
    DEBUG_ASSERT(slot && slot->IsIntervalSlot());
  }

  if (unlikely(!slot)) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t status = CheckOverwriteConditionsLocked(offset, VmPageOrMarkerRef(slot), overwrite);
  if (unlikely(status != ZX_OK)) {
    if (slot->IsEmpty()) {
      page_list_.ReturnEmptySlot(offset);
    }
    return zx::error(status);
  }

  return zx::ok(AddPageTransaction(VmPageOrMarkerRef(slot), offset, overwrite));
}

zx_status_t VmCowPages::CheckOverwriteConditionsLocked(uint64_t offset, VmPageOrMarkerRef slot,
                                                       CanOverwriteContent overwrite) {
  // Pages can be added as part of Init, but not once we transition to dead.
  DEBUG_ASSERT(life_cycle_ != LifeCycle::Dead);

  if (offset >= size_) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // We cannot overwrite any kind of content.
  if (overwrite == CanOverwriteContent::None) {
    // An anonymous VMO starts off with all its content set to zero, i.e. at no point can it have
    // absence of content.
    if (!page_source_) {
      return ZX_ERR_ALREADY_EXISTS;
    }
    // This VMO is backed by a page source, so empty slots represent absence of content. Fail if the
    // slot is not empty.
    if (!slot->IsEmpty()) {
      return ZX_ERR_ALREADY_EXISTS;
    }
  }

  // We're only permitted to overwrite zero content. This has different meanings based on the
  // whether the VMO is anonymous or is backed by a pager.
  //
  //  * For anonymous VMOs, the initial content for the entire VMO is implicitly all zeroes at the
  //  time of creation. So both zero page markers and empty slots represent zero content. Therefore
  //  the only content type that cannot be overwritten in this case is an actual page.
  //
  //  * For pager backed VMOs, content is either explicitly supplied by the user pager, or
  //  implicitly supplied as zeros by the kernel. Zero content is represented by either zero page
  //  markers (supplied by the user pager), or by sparse zero intervals (supplied by the kernel).
  //  Therefore the only content type that cannot be overwritten in this case as well is an actual
  //  page.
  if (overwrite == CanOverwriteContent::Zero && slot->IsPageOrRef()) {
    // If we have a page source, the page source should be able to validate the page.
    // Note that having a page source implies that any content must be an actual page and so
    // although we return an error for any kind of content, the debug check only gets run for page
    // sources where it will be a real page.
    DEBUG_ASSERT(!page_source_ || page_source_->DebugIsPageOk(slot->Page(), offset));
    return ZX_ERR_ALREADY_EXISTS;
  }
  // If the old entry and actual content then we should be permitted to overwrite any kind of
  // content (zero or non-zero).
  DEBUG_ASSERT(overwrite == CanOverwriteContent::NonZero || !slot->IsPageOrRef());
  return ZX_OK;
}

VmPageOrMarker VmCowPages::CompleteAddPageLocked(AddPageTransaction& transaction,
                                                 VmPageOrMarker&& p, bool do_range_update) {
  if (p.IsPage()) {
    LTRACEF("vmo %p, offset %#" PRIx64 ", page %p (%#" PRIxPTR ")\n", this, transaction.offset(),
            p.Page(), p.Page()->paddr());
  } else if (p.IsReference()) {
    [[maybe_unused]] const uint64_t cookie = p.Reference().value();
    LTRACEF("vmo %p, offset %#" PRIx64 ", reference %#" PRIx64 "\n", this, transaction.offset(),
            cookie);
  } else {
    DEBUG_ASSERT(p.IsMarker());
    LTRACEF("vmo %p, offset %#" PRIx64 ", marker\n", this, transaction.offset());
  }

  // If the new page is an actual page and we have a page source, the page source should be able to
  // validate the page.
  // Note that having a page source implies that any content must be an actual page and so
  // although we return an error for any kind of content, the debug check only gets run for page
  // sources where it will be a real page.
  DEBUG_ASSERT(!p.IsPageOrRef() || !page_source_ ||
               page_source_->DebugIsPageOk(p.Page(), transaction.offset()));

  // If this is actually a real page, we need to place it into the appropriate queue.
  if (p.IsPage()) {
    vm_page_t* low_level_page = p.Page();
    DEBUG_ASSERT(low_level_page->state() == vm_page_state::OBJECT);
    DEBUG_ASSERT(low_level_page->object.pin_count == 0);
    SetNotPinnedLocked(low_level_page, transaction.offset());
  }
  VmPageOrMarker old = transaction.Complete(ktl::move(p));

  if (do_range_update) {
    // If the old entry is a reference then we know that there can be no mappings to it, since a
    // reference cannot be mapped in, and we can skip the range update.
    if (!old.IsReference()) {
      // other mappings may have covered this offset into the vmo, so unmap those ranges
      RangeChangeUpdateLocked(VmCowRange(transaction.offset(), PAGE_SIZE),
                              transaction.overwrite() == CanOverwriteContent::NonZero
                                  ? RangeChangeOp::Unmap
                                  : RangeChangeOp::UnmapZeroPage);
    }
  }

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  return old;
}

void VmCowPages::CancelAddPageLocked(AddPageTransaction& transaction) {
  transaction.Cancel(page_list_);
}

zx::result<VmPageOrMarker> VmCowPages::AddPageLocked(uint64_t offset, VmPageOrMarker&& p,
                                                     CanOverwriteContent overwrite,
                                                     bool do_range_update) {
  __UNINITIALIZED auto result = BeginAddPageLocked(offset, overwrite);
  if (unlikely(result.is_error())) {
    if (p.IsPage()) {
      FreePageLocked(p.ReleasePage(), true);
    } else if (p.IsReference()) {
      FreeReference(p.ReleaseReference());
    }
    return result.take_error();
  }
  return zx::ok(CompleteAddPageLocked(*result, ktl::move(p), do_range_update));
}

zx_status_t VmCowPages::AddNewPageLocked(uint64_t offset, vm_page_t* page,
                                         CanOverwriteContent overwrite,
                                         VmPageOrMarker* released_page, bool zero,
                                         bool do_range_update) {
  canary_.Assert();

  __UNINITIALIZED auto result = BeginAddPageLocked(offset, overwrite);
  if (result.is_error()) {
    return result.status_value();
  }
  VmPageOrMarker old = CompleteAddNewPageLocked(*result, page, zero, do_range_update);
  if (released_page) {
    *released_page = ktl::move(old);
  } else {
    DEBUG_ASSERT(!old.IsPageOrRef());
  }
  return ZX_OK;
}

VmPageOrMarker VmCowPages::CompleteAddNewPageLocked(AddPageTransaction& transaction,
                                                    vm_page_t* page, bool zero,
                                                    bool do_range_update) {
  DEBUG_ASSERT(IS_PAGE_ALIGNED(transaction.offset()));

  InitializeVmPage(page);
  if (zero) {
    ZeroPage(page);
  }

  // Pages being added to pager backed VMOs should have a valid dirty_state before being added to
  // the page list, so that they can be inserted in the correct page queue. New pages start off
  // clean.
  if (is_source_preserving_page_content()) {
    // Only zero pages can be added as new pages to pager backed VMOs.
    DEBUG_ASSERT(zero || IsZeroPage(page));
    UpdateDirtyStateLocked(page, transaction.offset(), DirtyState::Clean, /*is_pending_add=*/true);
  }
  return CompleteAddPageLocked(transaction, VmPageOrMarker::Page(page), do_range_update);
}

zx_status_t VmCowPages::AddNewPagesLocked(uint64_t start_offset, list_node_t* pages,
                                          CanOverwriteContent overwrite, bool zero,
                                          bool do_range_update) {
  ASSERT(overwrite != CanOverwriteContent::NonZero);
  canary_.Assert();

  DEBUG_ASSERT(IS_PAGE_ALIGNED(start_offset));

  uint64_t offset = start_offset;
  while (vm_page_t* p = list_remove_head_type(pages, vm_page_t, queue_node)) {
    // Defer the range change update by passing false as we will do it in bulk at the end if needed.
    zx_status_t status = AddNewPageLocked(offset, p, overwrite, nullptr, zero, false);
    if (status != ZX_OK) {
      // Put the page back on the list so that someone owns it and it'll get free'd.
      list_add_head(pages, &p->queue_node);
      // Decommit any pages we already placed.
      if (offset > start_offset) {
        DecommitRangeLocked(VmCowRange(start_offset, offset - start_offset));
      }

      // Free all the pages back as we had ownership of them.
      FreePagesLocked(pages, /*freeing_owned_pages=*/true);
      return status;
    }
    offset += PAGE_SIZE;
  }

  if (do_range_update) {
    // other mappings may have covered this offset into the vmo, so unmap those ranges
    RangeChangeUpdateLocked(VmCowRange(start_offset, offset - start_offset), RangeChangeOp::Unmap);
  }

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  return ZX_OK;
}

zx_status_t VmCowPages::CloneCowPageLocked(uint64_t offset, list_node_t* alloc_list,
                                           VmCowPages* page_owner, vm_page_t* page,
                                           uint64_t owner_offset,
                                           AnonymousPageRequest* page_request,
                                           vm_page_t** out_page) {
  AssertHeld(page_owner->lock_ref());
  DEBUG_ASSERT(page != vm_get_zero_page());
  DEBUG_ASSERT(parent_);
  DEBUG_ASSERT(page_request);
  // We only clone pages from hidden to visible nodes.
  DEBUG_ASSERT(page_owner->is_hidden_locked());
  DEBUG_ASSERT(!is_hidden_locked());
  // We don't want to handle intervals here. They should only be present when this node is backed by
  // a user pager, and such nodes don't have parents so cannot be the target of a forked page.
  DEBUG_ASSERT(!is_source_preserving_page_content());

  // Ensure this node is ready to accept a newly-allocated page. If a subsequent step fails (such as
  // allocating the page itself), cancelling the `page_transaction` will handle any rollback logic.
  //
  // By the time this function returns, the transaction will be either completed or canceled.
  __UNINITIALIZED auto page_transaction = BeginAddPageLocked(offset, CanOverwriteContent::Zero);
  auto cancel_transaction = fit::defer([this, out_page, &page_transaction] {
    AssertHeld(lock_ref());

    if (!page_transaction.is_error()) {
      CancelAddPageLocked(*page_transaction);
    }
    *out_page = nullptr;  // Ensure the `out_page` is initialized if we fail at any point.
  });
  if (page_transaction.is_error()) {
    return page_transaction.status_value();
  }

  // If the page is shared we must fork it, otherwise we can migrate it.
  if (page->object.share_count > 0) {
    // Create a fork of the page. This may fail due to inability to allocate a new page.
    // The page is not writable so there is no need to unmap or protect it before reading it for the
    // fork.
    vm_page_t* forked_page = nullptr;
    zx_status_t status = AllocateCopyPage(page->paddr(), alloc_list, page_request, &forked_page);
    if (unlikely(status != ZX_OK)) {
      return status;
    }

    // The page is now shared one less time.
    page->object.share_count--;

    *out_page = forked_page;
  } else {
    // Remove the page from the owner.
    VmPageOrMarker removed = page_owner->page_list_.RemoveContent(owner_offset);
    vm_page* removed_page = removed.ReleasePage();
    DEBUG_ASSERT(removed_page == page);
    // TODO: This could be optimized to a ChangeObjectOffset instead of doing a Remove here and an
    // insert in CompleteAddPageLocked.
    pmm_page_queues()->Remove(removed_page);

    *out_page = removed_page;
  }

  // Now that we can no longer fail to insert the new page into this node, complete the add page
  // transaction.
  //
  // If the new page is different from the original page, then we must remove the original page
  // from any mappings that reference this node or its descendants.
  const bool do_range_update = (*out_page != page);
  [[maybe_unused]] VmPageOrMarker prev_content =
      CompleteAddPageLocked(*page_transaction, VmPageOrMarker::Page(*out_page), do_range_update);
  // We should not have been trying to fork at this offset if something already existed.
  DEBUG_ASSERT(prev_content.IsEmpty());
  // Transaction completed successfully, so it should no longer be cancelled.
  cancel_transaction.cancel();

  return ZX_OK;
}

zx_status_t VmCowPages::CloneCowPageAsZeroLocked(uint64_t offset, list_node_t* freed_list,
                                                 VmCowPages* page_owner, vm_page_t* page,
                                                 uint64_t owner_offset,
                                                 AnonymousPageRequest* page_request) {
  AssertHeld(page_owner->lock_ref());
  DEBUG_ASSERT(page != vm_get_zero_page());
  DEBUG_ASSERT(parent_);
  DEBUG_ASSERT(!page_source_ || page_source_->DebugIsPageOk(page, offset));
  // We only clone pages from hidden to visible nodes.
  DEBUG_ASSERT(page_owner->is_hidden_locked());
  DEBUG_ASSERT(!is_hidden_locked());
  // We don't want to handle intervals here. They should only be present when this node is backed by
  // a user pager, and such nodes don't have parents so cannot be the target of a forked page.
  DEBUG_ASSERT(!is_source_preserving_page_content());

  // Go ahead and insert the new zero marker into the target. We don't have anything to rollback
  // if this fails so we can just bail immediately.
  //
  // We expect the caller to update any mappings as it can more efficiently do this in bulk.
  zx::result<VmPageOrMarker> prev_content = AddPageLocked(
      offset, VmPageOrMarker::Marker(), CanOverwriteContent::Zero, /*do_range_update=*/false);
  if (prev_content.is_error()) {
    return prev_content.status_value();
  }
  DEBUG_ASSERT(prev_content->IsEmpty());

  // Release the reference we held to the forked page.
  if (page->object.share_count > 0) {
    // The page is now shared one less time.
    page->object.share_count--;
  } else {
    // Remove the page from the owner.
    VmPageOrMarker removed = page_owner->page_list_.RemoveContent(owner_offset);
    vm_page* removed_page = removed.ReleasePage();
    DEBUG_ASSERT(removed_page == page);
    pmm_page_queues()->Remove(removed_page);

    list_add_tail(freed_list, &page->queue_node);
  }

  return ZX_OK;
}

void VmCowPages::ReleaseOwnedPagesLocked(uint64_t start) {
  DEBUG_ASSERT(!is_hidden_locked());
  DEBUG_ASSERT(start <= size_);

  // We stack-own loaned pages between removing the page from PageQueues and freeing the page
  // via call to |FreePagesLocked|.
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  list_node_t freed_list;
  list_initialize(&freed_list);
  __UNINITIALIZED BatchPQRemove page_remover(&freed_list);

  // If we know that the only pages in this range that need to be freed are from our own page list,
  // and we no longer need to consider our parent, then just remove them.
  if (!is_parent_hidden_locked() || start >= parent_limit_) {
    if (start == 0) {
      page_list_.RemoveAllContent(
          [&page_remover](VmPageOrMarker&& p) { page_remover.PushContent(&p); });
    } else {
      page_list_.RemovePages(page_remover.RemovePagesCallback(), start, size_);
    }
    page_remover.Flush();
    FreePagesLocked(&freed_list, /*freeing_owned_pages=*/true);
    // Potentially trim the parent limit to reflect the range that has been freed.
    parent_limit_ = ktl::min(parent_limit_, start);
    return;
  }

  VmCompression* compression = Pmm::Node().GetPageCompression();

  // Decrement the share count on all pages, both directly owned by us and shared via our parents,
  // that this node can see, and free any pages with a zero ref count.
  zx_status_t status = ForEveryOwnedMutableHierarchyPageInRangeLocked(
      [&](VmPageOrMarker* p, const VmCowPages* owner, uint64_t this_offset, uint64_t owner_offset) {
        // Explicitly handle this case separately since although we would naturally find these to
        // have a share_count of 0 and free them, we would always like to free any markers, however
        // we can only free markers that are precisely in 'this' since markers have no refcount.
        if (this == owner) {
          page_remover.PushContent(p);
          return ZX_ERR_NEXT;
        }

        if (p->IsPage()) {
          vm_page_t* page = p->Page();
          if (page->object.share_count == 0) {
            page_remover.PushContent(p);
          } else {
            page->object.share_count--;
          }
        } else if (p->IsReference()) {
          const uint32_t share_count = compression->GetMetadata(p->Reference());
          if (share_count == 0) {
            page_remover.PushContent(p);
          } else {
            compression->SetMetadata(p->Reference(), share_count - 1);
          }
        }
        return ZX_ERR_NEXT;
      },
      start, size_ - start);
  DEBUG_ASSERT(status == ZX_OK);

  // This node can no longer see into its parent in the range we just released.
  DEBUG_ASSERT(start < parent_limit_);
  parent_limit_ = start;

  page_remover.Flush();
  // We are freeing a mixture of owned and non-owned pages, but this is fine since we know all pages
  // are from an anonymous hidden hierarchy and so none of these pages are owned by a page source.
  FreePagesLocked(&freed_list, /*freeing_owned_pages=*/false);
}

VMPLCursor VmCowPages::FindInitialPageContentLocked(uint64_t offset, VmCowPages** owner_out,
                                                    uint64_t* owner_offset_out,
                                                    uint64_t* owner_length) {
  // Search up the clone chain for any committed pages. cur_offset is the offset
  // into cur we care about. The loop terminates either when that offset contains
  // a committed page or when that offset can't reach into the parent.
  VMPLCursor page;
  VmCowPages* cur = this;
  AssertHeld(cur->lock_ref());
  uint64_t cur_offset = offset;
  while (cur_offset < cur->parent_limit_) {
    VmCowPages* parent = cur->parent_.get();
    // If there's no parent, then parent_limit_ is 0 and we'll never enter the loop
    DEBUG_ASSERT(parent);
    AssertHeld(parent->lock_ref());

    uint64_t parent_offset;
    bool overflowed = add_overflow(cur->parent_offset_, cur_offset, &parent_offset);
    ASSERT(!overflowed);
    if (parent_offset >= parent->size_) {
      // The offset is off the end of the parent, so cur is the VmObjectPaged
      // which will provide the page.
      break;
    }
    if (owner_length) {
      // Before we walk up, need to check to see if there's any forked pages that require us to
      // restrict the owner length. Additionally need to restrict the owner length to the actual
      // parent limit.
      *owner_length = ktl::min(*owner_length, cur->parent_limit_ - cur_offset);
      cur->page_list_.ForEveryPageInRange(
          [owner_length, cur_offset](const VmPageOrMarker* p, uint64_t off) {
            // VMO children do not support page intervals.
            ASSERT(!p->IsInterval());
            *owner_length = off - cur_offset;
            return ZX_ERR_STOP;
          },
          cur_offset, cur_offset + *owner_length);
    }

    cur = parent;
    cur_offset = parent_offset;
    VMPLCursor next_cursor = cur->page_list_.LookupMutableCursor(parent_offset);
    VmPageOrMarkerRef p = next_cursor.current();
    if (p && !p->IsEmpty()) {
      page = ktl::move(next_cursor);
      break;
    }
  }

  *owner_out = cur;
  *owner_offset_out = cur_offset;

  return page;
}

void VmCowPages::UpdateDirtyStateLocked(vm_page_t* page, uint64_t offset, DirtyState dirty_state,
                                        bool is_pending_add) {
  ASSERT(page);
  ASSERT(is_source_preserving_page_content());

  // If the page is not pending being added to the page list, it should have valid object info.
  DEBUG_ASSERT(is_pending_add || page->object.get_object() == this);
  DEBUG_ASSERT(is_pending_add || page->object.get_page_offset() == offset);

  // If the page is Dirty or AwaitingClean, it should not be loaned.
  DEBUG_ASSERT(!(is_page_dirty(page) || is_page_awaiting_clean(page)) || !page->is_loaned());

  // Perform state-specific checks and actions. We will finally update the state below.
  switch (dirty_state) {
    case DirtyState::Clean:
      // If the page is not in the process of being added, we can only see a transition to Clean
      // from AwaitingClean.
      ASSERT(is_pending_add || is_page_awaiting_clean(page));

      // If we are expecting a pending Add[New]PageLocked, we can defer updating the page queue.
      if (!is_pending_add) {
        // Move to evictable pager backed queue to start tracking age information.
        pmm_page_queues()->MoveToReclaim(page);
      }
      break;
    case DirtyState::Dirty:
      // If the page is not in the process of being added, we can only see a transition to Dirty
      // from Clean or AwaitingClean.
      ASSERT(is_pending_add || (is_page_clean(page) || is_page_awaiting_clean(page)));

      // A loaned page cannot be marked Dirty as loaned pages are reclaimed by eviction; Dirty pages
      // cannot be evicted.
      DEBUG_ASSERT(!page->is_loaned());

      // If we are expecting a pending Add[New]PageLocked, we can defer updating the page queue.
      if (!is_pending_add) {
        // Move the page to the Dirty queue, which does not track page age. While the page is in the
        // Dirty queue, age information is not required (yet). It will be required when the page
        // becomes Clean (and hence evictable) again, at which point it will get moved to the MRU
        // pager backed queue and will age as normal.
        // TODO(rashaeqbal): We might want age tracking for the Dirty queue in the future when the
        // kernel generates writeback pager requests.
        pmm_page_queues()->MoveToPagerBackedDirty(page);
      }
      break;
    case DirtyState::AwaitingClean:
      // A newly added page cannot start off as AwaitingClean.
      ASSERT(!is_pending_add);
      // A pinned page will be kept Dirty as long as it is pinned.
      //
      // Note that there isn't a similar constraint when setting the Clean state as it is possible
      // to pin a page for read after it has been marked AwaitingClean. Since it is a pinned read it
      // does not need to dirty the page. So when the writeback is done it can transition from
      // AwaitingClean -> Clean with a non-zero pin count.
      //
      // It is also possible for us to observe an intermediate pin count for a write-pin that has
      // not fully completed yet, as we will only attempt to dirty pages after pinning them. So it
      // is possible for a thread to be waiting on a DIRTY request on a pinned page, while a racing
      // writeback transitions the page from AwaitingClean -> Clean with a non-zero pin count.
      ASSERT(page->object.pin_count == 0);
      // We can only transition to AwaitingClean from Dirty.
      ASSERT(is_page_dirty(page));
      // A loaned page cannot be marked AwaitingClean as loaned pages are reclaimed by eviction;
      // AwaitingClean pages cannot be evicted.
      DEBUG_ASSERT(!page->is_loaned());
      // No page queue update. Leave the page in the Dirty queue for now as it is not clean yet;
      // it will be moved out on WritebackEnd.
      DEBUG_ASSERT(pmm_page_queues()->DebugPageIsPagerBackedDirty(page));
      break;
    default:
      ASSERT(false);
  }
  page->object.dirty_state = static_cast<uint8_t>(dirty_state) & VM_PAGE_OBJECT_DIRTY_STATES_MASK;
}

zx_status_t VmCowPages::PrepareForWriteLocked(VmCowRange range, LazyPageRequest* page_request,
                                              uint64_t* dirty_len_out) {
  DEBUG_ASSERT(range.is_page_aligned());
  DEBUG_ASSERT(range.IsBoundedBy(size_));

  if (is_slice_locked()) {
    return slice_parent_locked().PrepareForWriteLocked(range.OffsetBy(parent_offset_), page_request,
                                                       dirty_len_out);
  }

  DEBUG_ASSERT(page_source_);
  DEBUG_ASSERT(is_source_preserving_page_content());

  uint64_t dirty_len = 0;
  const uint64_t start_offset = range.offset;
  const uint64_t end_offset = range.end();

  // If the VMO does not require us to trap dirty transitions, simply mark the pages dirty, and move
  // them to the dirty page queue. Do this only for the first consecutive run of committed pages
  // within the range starting at offset. Any absent pages will need to be provided by the page
  // source, which might fail and terminate the lookup early. Any zero page markers and zero
  // intervals might need to be forked, which can fail too. Only mark those pages dirty that the
  // lookup is guaranteed to return successfully.
  if (!page_source_->ShouldTrapDirtyTransitions()) {
    zx_status_t status = page_list_.ForEveryPageAndGapInRange(
        [this, &dirty_len, start_offset](const VmPageOrMarker* p, uint64_t off) {
          // TODO(johngro): remove this explicit unused-capture warning suppression
          // when https://bugs.llvm.org/show_bug.cgi?id=35450 gets fixed.
          (void)start_offset;  // used only in DEBUG_ASSERT
          if (p->IsMarker() || p->IsIntervalZero()) {
            // Found a marker or zero interval. End the traversal.
            return ZX_ERR_STOP;
          }
          // VMOs with a page source will never have compressed references, so this should be a
          // real page.
          DEBUG_ASSERT(p->IsPage());
          vm_page_t* page = p->Page();
          DEBUG_ASSERT(is_page_dirty_tracked(page));
          DEBUG_ASSERT(page->object.get_object() == this);
          DEBUG_ASSERT(page->object.get_page_offset() == off);

          // End the traversal if we encounter a loaned page. We reclaim loaned pages by evicting
          // them, and dirty pages cannot be evicted.
          if (page->is_loaned()) {
            // If this is a loaned page, it should be clean.
            DEBUG_ASSERT(is_page_clean(page));
            return ZX_ERR_STOP;
          }
          DEBUG_ASSERT(!page->is_loaned());

          // Mark the page dirty.
          if (!is_page_dirty(page)) {
            AssertHeld(lock_ref());
            UpdateDirtyStateLocked(page, off, DirtyState::Dirty);
          }
          // The page was either already dirty, or we just marked it dirty. Proceed to the next one.
          DEBUG_ASSERT(start_offset + dirty_len == off);
          dirty_len += PAGE_SIZE;
          return ZX_ERR_NEXT;
        },
        [](uint64_t start, uint64_t end) {
          // We found a gap. End the traversal.
          return ZX_ERR_STOP;
        },
        start_offset, end_offset);
    // We don't expect a failure from the traversal.
    DEBUG_ASSERT(status == ZX_OK);

    *dirty_len_out = dirty_len;
    VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
    return ZX_OK;
  }

  // Otherwise, generate a DIRTY page request for pages in the range which need to transition to
  // Dirty. Pages that qualify are:
  //  - Any contiguous run of non-Dirty pages (committed pages as well as zero page markers).
  //  For the purpose of generating DIRTY requests, both Clean and AwaitingClean pages are
  //  considered equivalent. This is because pages that are in AwaitingClean will need another
  //  acknowledgment from the user pager before they can be made Dirty (the filesystem might need to
  //  reserve additional space for them etc.).
  //  - Any zero intervals are implicit zero pages, i.e. the kernel supplies zero pages when they
  //  are accessed. Since these pages are not supplied by the user pager via zx_pager_supply_pages,
  //  we will need to wait on a DIRTY request before the sparse range can be replaced by an actual
  //  page for writing (the filesystem might need to reserve additional space).
  uint64_t pages_to_dirty_len = 0;

  // Helper lambda used in the page list traversal below. Try to add pages in the range
  // [dirty_pages_start, dirty_pages_end) to the run of dirty pages being tracked. Return codes are
  // the same as those used by VmPageList::ForEveryPageAndGapInRange to continue or terminate
  // traversal.
  auto accumulate_dirty_pages = [&pages_to_dirty_len, &dirty_len, start_offset](
                                    uint64_t dirty_pages_start,
                                    uint64_t dirty_pages_end) -> zx_status_t {
    // Bail if we were tracking a non-zero run of pages to be dirtied as we cannot extend
    // pages_to_dirty_len anymore.
    if (pages_to_dirty_len > 0) {
      return ZX_ERR_STOP;
    }
    // Append the page to the dirty range being tracked if it immediately follows it.
    if (start_offset + dirty_len == dirty_pages_start) {
      dirty_len += (dirty_pages_end - dirty_pages_start);
      return ZX_ERR_NEXT;
    }
    // Otherwise we cannot accumulate any more contiguous dirty pages.
    return ZX_ERR_STOP;
  };

  // Helper lambda used in the page list traversal below. Try to add pages in the range
  // [to_dirty_start, to_dirty_end) to the run of to-be-dirtied pages being tracked. Return codes
  // are the same as those used by VmPageList::ForEveryPageAndGapInRange to continue or terminate
  // traversal.
  auto accumulate_pages_to_dirty = [&pages_to_dirty_len, &dirty_len, start_offset](
                                       uint64_t to_dirty_start,
                                       uint64_t to_dirty_end) -> zx_status_t {
    // Bail if we were already accumulating a non-zero run of Dirty pages.
    if (dirty_len > 0) {
      return ZX_ERR_STOP;
    }
    // Append the pages to the range being tracked if they immediately follow it.
    if (start_offset + pages_to_dirty_len == to_dirty_start) {
      pages_to_dirty_len += (to_dirty_end - to_dirty_start);
      return ZX_ERR_NEXT;
    }
    // Otherwise we cannot accumulate any more contiguous to-dirty pages.
    return ZX_ERR_STOP;
  };

  // This tracks the beginning of an interval that falls in the specified range. Since we might
  // start partway inside an interval, this is initialized to start_offset so that we only consider
  // the portion of the interval inside the range. If we did not start inside an interval, we will
  // end up reinitializing this when we do find an interval start, before this value is used, so it
  // is safe to initialize to start_offset in all cases.
  uint64_t interval_start_off = start_offset;
  // This tracks whether we saw an interval start sentinel in the traversal, but have not yet
  // encountered a matching interval end sentinel. Should we end the traversal partway in an
  // interval, we will need to handle the portion of the interval between the interval start and the
  // end of the specified range.
  bool unmatched_interval_start = false;
  bool found_page_or_gap = false;
  zx_status_t status = page_list_.ForEveryPageAndGapInRange(
      [&accumulate_dirty_pages, &accumulate_pages_to_dirty, &interval_start_off,
       &unmatched_interval_start, &found_page_or_gap, this](const VmPageOrMarker* p, uint64_t off) {
        found_page_or_gap = true;
        if (p->IsPage()) {
          vm_page_t* page = p->Page();
          DEBUG_ASSERT(is_page_dirty_tracked(page));
          // VMOs that trap dirty transitions should not have loaned pages.
          DEBUG_ASSERT(!page->is_loaned());
          // Page is already dirty. Try to add it to the dirty run.
          if (is_page_dirty(page)) {
            return accumulate_dirty_pages(off, off + PAGE_SIZE);
          }
          // If the page is clean, mark it accessed to grant it some protection from eviction
          // until the pager has a chance to respond to the DIRTY request.
          if (is_page_clean(page)) {
            AssertHeld(lock_ref());
            pmm_page_queues()->MarkAccessed(page);
          }
        } else if (p->IsIntervalZero()) {
          if (p->IsIntervalStart() || p->IsIntervalSlot()) {
            unmatched_interval_start = true;
            interval_start_off = off;
          }
          if (p->IsIntervalEnd() || p->IsIntervalSlot()) {
            unmatched_interval_start = false;
            // We need to commit pages if this is an interval, irrespective of the dirty state.
            return accumulate_pages_to_dirty(interval_start_off, off + PAGE_SIZE);
          }
          return ZX_ERR_NEXT;
        }

        // We don't compress pages in pager-backed VMOs.
        DEBUG_ASSERT(!p->IsReference());
        // This is a either a zero page marker (which represents a clean zero page) or a committed
        // page which is not already Dirty. Try to add it to the range of pages to be dirtied.
        DEBUG_ASSERT(p->IsMarker() || !is_page_dirty(p->Page()));
        return accumulate_pages_to_dirty(off, off + PAGE_SIZE);
      },
      [&found_page_or_gap](uint64_t start, uint64_t end) {
        found_page_or_gap = true;
        // We found a gap. End the traversal.
        return ZX_ERR_STOP;
      },
      start_offset, end_offset);

  // We don't expect an error from the traversal above. If an incompatible contiguous page or
  // a gap is encountered, we will simply terminate early.
  DEBUG_ASSERT(status == ZX_OK);

  // Process the last remaining interval if there is one.
  if (unmatched_interval_start) {
    accumulate_pages_to_dirty(interval_start_off, end_offset);
  }

  // Account for the case where we started and ended in unpopulated slots inside an interval, i.e we
  // did not find either a page or a gap in the traversal. We would not have accumulated any pages
  // in that case.
  if (!found_page_or_gap) {
    DEBUG_ASSERT(page_list_.IsOffsetInZeroInterval(start_offset));
    DEBUG_ASSERT(page_list_.IsOffsetInZeroInterval(end_offset - PAGE_SIZE));
    DEBUG_ASSERT(dirty_len == 0);
    DEBUG_ASSERT(pages_to_dirty_len == 0);
    // The entire range falls in an interval so it needs a DIRTY request.
    pages_to_dirty_len = end_offset - start_offset;
  }

  // We should either have found dirty pages or pages that need to be dirtied, but not both.
  DEBUG_ASSERT(dirty_len == 0 || pages_to_dirty_len == 0);
  // Check that dirty_len and pages_to_dirty_len both specify valid ranges.
  DEBUG_ASSERT(start_offset + dirty_len <= end_offset);
  DEBUG_ASSERT(pages_to_dirty_len == 0 || start_offset + pages_to_dirty_len <= end_offset);

  *dirty_len_out = dirty_len;

  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());

  // No pages need to transition to Dirty.
  if (pages_to_dirty_len == 0) {
    return ZX_OK;
  }

  // Found a contiguous run of pages that need to transition to Dirty. There might be more such
  // pages later in the range, but we will come into this call again for them via another
  // LookupCursor call after the waiting caller is unblocked for this range.

  VmoDebugInfo vmo_debug_info{};
  // We have a page source so this cannot be a hidden node, but the VmObjectPaged could have been
  // destroyed. We could be looking up a page via a lookup in a child (slice) after the parent
  // VmObjectPaged has gone away, so paged_ref_ could be null. Let the page source handle any
  // failures requesting the dirty transition.
  if (paged_ref_) {
    AssertHeld(paged_ref_->lock_ref());
    vmo_debug_info = {.vmo_ptr = reinterpret_cast<uintptr_t>(paged_ref_),
                      .vmo_id = paged_ref_->user_id_locked()};
  }
  status = page_source_->RequestDirtyTransition(page_request->get(), start_offset,
                                                pages_to_dirty_len, vmo_debug_info);
  // The page source will never succeed synchronously.
  DEBUG_ASSERT(status != ZX_OK);
  return status;
}

inline VmCowPages::LookupCursor::RequireResult VmCowPages::LookupCursor::PageAsResultNoIncrement(
    vm_page_t* page, bool in_target) {
  // The page is writable if it's present in the target (non owned pages are never writable) and it
  // does not need a dirty transition. A page doesn't need a dirty transition if the target isn't
  // preserving page contents, or if the page is just already dirty.
  RequireResult result{page,
                       (in_target && (!target_preserving_page_content_ || is_page_dirty(page)))};
  return result;
}

void VmCowPages::LookupCursor::IncrementOffsetAndInvalidateCursor(uint64_t delta) {
  offset_ += delta;
  owner_ = nullptr;
}

bool VmCowPages::LookupCursor::CursorIsContentZero() const {
  // Markers are always zero.
  if (CursorIsMarker()) {
    return true;
  }

  if (owner_->page_source_) {
    // With a page source emptiness implies needing to request content, however we can have zero
    // intervals which do start as zero content.
    return CursorIsInIntervalZero();
  }
  // Without a page source emptiness is filled with zeros and intervals are only permitted if there
  // is a page source.
  return CursorIsEmpty();
}

bool VmCowPages::LookupCursor::TargetZeroContentSupplyDirty(bool writing) const {
  if (!TargetDirtyTracked()) {
    return false;
  }
  if (writing) {
    return true;
  }
  // Markers start clean
  if (CursorIsMarker()) {
    return false;
  }
  // The only way this offset can have been zero content and reach here, is if we are in an
  // interval. If this slot were empty then, since we are dirty tracked and hence must have a
  // page source, we would not consider this zero.
  DEBUG_ASSERT(CursorIsInIntervalZero());
  // Zero intervals are considered implicitly dirty and allocating them, even for reading, causes
  // them to be supplied as new dirty pages.
  return true;
}

zx::result<VmCowPages::LookupCursor::RequireResult>
VmCowPages::LookupCursor::TargetAllocateCopyPageAsResult(vm_page_t* source, DirtyState dirty_state,
                                                         AnonymousPageRequest* page_request) {
  // The general pmm_alloc_flags_ are not allowed to contain the LOANED option, and this is relied
  // upon below to assume the page allocated cannot be loaned.
  DEBUG_ASSERT(!(target_->pmm_alloc_flags_ & PMM_ALLOC_FLAG_LOANED));

  vm_page_t* out_page = nullptr;
  zx_status_t status =
      target_->AllocateCopyPage(source->paddr(), alloc_list_, page_request, &out_page);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  // The forked page was just allocated, and so cannot be a loaned page.
  DEBUG_ASSERT(!out_page->is_loaned());

  // We could be allocating a page to replace a zero page marker in a pager-backed VMO. If so then
  // set its dirty state to what was requested, AddPageLocked below will then insert the page into
  // the appropriate page queue.
  if (target_preserving_page_content_) {
    // The only page we can be forking here is the zero page.
    DEBUG_ASSERT(source == vm_get_zero_page());
    // The object directly owns the page.
    DEBUG_ASSERT(owner_ == target_);

    target_->UpdateDirtyStateLocked(out_page, offset_, dirty_state,
                                    /*is_pending_add=*/true);
  }

  // For efficiently we would like to use the slot we already have in our cursor if possible,
  // however that can only be done if all of the following hold:
  //  * owner_ == target_ - If not true then we do not even have a cursor (and hence slot) for where
  //    the insertion is happening.
  //  * owner_pl_cursor_.current() != nullptr - Must be an actual node and slot already allocated,
  //    it is just Empty()
  //  * !is_source_preserving_page_content() - A source preserving page content may have intervals,
  //    which are zeroes that we could be overwriting here, but the slot itself we have found could
  //    be empty and the interval may need splitting. For simplicity we do not attempt to check for
  //    and handle interval splitting, and just skip reusing our slot in this case.
  const bool can_reuse_slot = (owner_ == target_ && owner_pl_cursor_.current() &&
                               !owner_->is_source_preserving_page_content());
  __UNINITIALIZED auto page_transaction =
      can_reuse_slot ? target_->BeginAddPageWithSlotLocked(offset_, owner_pl_cursor_.current(),
                                                           CanOverwriteContent::Zero)
                     : target_->BeginAddPageLocked(offset_, CanOverwriteContent::Zero);
  if (page_transaction.is_error()) {
    // We are freeing a page we just got from the PMM (or from the alloc_list), so we do not own
    // it yet.
    target_->FreePageLocked(out_page, /*freeing_owned_page=*/false);
    return page_transaction.take_error();
  }

  [[maybe_unused]] VmPageOrMarker old = target_->CompleteAddPageLocked(
      *page_transaction, VmPageOrMarker::Page(out_page), /*do_range_update=*/true);
  DEBUG_ASSERT(!old.IsPageOrRef());
  target_->IncrementHierarchyGenerationCountLocked();

  // If asked to explicitly mark zero forks, and this is actually fork of the zero page, move to the
  // correct queue. Discardable pages are not considered zero forks as they are always in the
  // reclaimable page queues.
  if (zero_fork_ && source == vm_get_zero_page() && !target_->is_discardable()) {
    pmm_page_queues()->MoveToAnonymousZeroFork(out_page);
  }

  // This is the only path where we can allocate a new page without being a clone (clones are
  // always cached). So we check here if we are not fully cached and if so perform a
  // clean/invalidate to flush our zeroes. After doing this we will not touch the page via the
  // physmap and so we can pretend there isn't an aliased mapping.
  // There are three potential states that may exist
  //  * VMO is cached, paged_ref_ might be null, we might have children -> no cache op needed
  //  * VMO is uncached, paged_ref_ is not null, we have no children -> cache op needed
  //  * VMO is uncached, paged_ref_ is null, we have no children -> cache op not needed /
  //                                                                state cannot happen
  // In the uncached case we know we have no children, since it is by definition not valid to
  // have copy-on-write children of uncached pages. The third case cannot happen, but even if it
  // could with no children and no paged_ref_ the pages cannot actually be referenced so any
  // cache operation is pointless.
  // The paged_ref_ could be null if the VmObjectPaged has been destroyed.
  if (target_->paged_ref_) {
    AssertHeld(target_->paged_ref_->lock_ref());
    if (target_->paged_ref_->GetMappingCachePolicyLocked() != ARCH_MMU_FLAG_CACHED) {
      arch_clean_invalidate_cache_range((vaddr_t)paddr_to_physmap(out_page->paddr()), PAGE_SIZE);
    }
  }

  // Need to increment the cursor, but we have also potentially modified the page lists in the
  // process of inserting the page.
  if (owner_ == target_) {
    // In the case of owner_ == target_ we may have create a node and need to establish a cursor.
    // However, if we already had a node, i.e. the cursor was valid, then it would have had the page
    // inserted into it.
    if (!owner_pl_cursor_.current()) {
      IncrementOffsetAndInvalidateCursor(PAGE_SIZE);
    } else {
      // Cursor should have been updated to the new page
      DEBUG_ASSERT(CursorIsPage());
      DEBUG_ASSERT(owner_cursor_->Page() == out_page);
      IncrementCursor();
    }
  } else {
    // If owner_ != target_ then owner_ page list will not have been modified, so safe to just
    // increment.
    IncrementCursor();
  }

  // Return the page. We know it's in the target, since we just put it there, but let PageAsResult
  // determine if that means it is actually writable or not.
  return zx::ok(PageAsResultNoIncrement(out_page, true));
}

zx_status_t VmCowPages::LookupCursor::CursorReferenceToPage(AnonymousPageRequest* page_request) {
  DEBUG_ASSERT(CursorIsReference());

  return owner()->ReplaceReferenceWithPageLocked(owner_cursor_, owner_offset_, page_request);
}

zx_status_t VmCowPages::LookupCursor::ReadRequest(uint max_request_pages,
                                                  PageRequest* page_request) {
  // The owner must have a page_source_ to be doing a read request.
  DEBUG_ASSERT(owner_->page_source_);
  // The cursor should be explicitly empty as read requests are only for complete content absence.
  DEBUG_ASSERT(CursorIsEmpty());
  DEBUG_ASSERT(!CursorIsInIntervalZero());
  // The total range requested should not be beyond the cursors valid range.
  DEBUG_ASSERT(offset_ + PAGE_SIZE * max_request_pages <= end_offset_);
  DEBUG_ASSERT(max_request_pages > 0);

  VmoDebugInfo vmo_debug_info{};
  // The page owner has a page source so it cannot be a hidden node, but the VmObjectPaged
  // could have been destroyed. We could be looking up a page via a lookup in a child after
  // the parent VmObjectPaged has gone away, so paged_ref_ could be null. Let the page source
  // handle any failures requesting the pages.
  if (owner()->paged_ref_) {
    AssertHeld(owner()->paged_ref_->lock_ref());
    vmo_debug_info = {.vmo_ptr = reinterpret_cast<uintptr_t>(owner()->paged_ref_),
                      .vmo_id = owner()->paged_ref_->user_id_locked()};
  }

  // Try and batch more pages up to |max_request_pages|.
  uint64_t request_size = static_cast<uint64_t>(max_request_pages) * PAGE_SIZE;
  if (owner_ != target_) {
    DEBUG_ASSERT(visible_end_ > offset_);
    // Limit the request by the number of pages that are actually visible from the target_ to
    // owner_
    request_size = ktl::min(request_size, visible_end_ - offset_);
  }
  // Limit |request_size| to the first page visible in the page owner to avoid requesting pages
  // that are already present. If there is one page present in an otherwise long run of absent pages
  // then it might be preferable to have one big page request, but for now only request absent
  // pages.If already requesting a single page then can avoid the page list operation.
  if (request_size > PAGE_SIZE) {
    owner()->page_list_.ForEveryPageInRange(
        [&](const VmPageOrMarker* p, uint64_t offset) {
          // Content should have been empty initially, so should not find anything at the start
          // offset.
          DEBUG_ASSERT(offset > owner_offset_);
          // If this is an interval sentinel, it can only be a start or slot, since we know we
          // started in a true gap outside of an interval.
          DEBUG_ASSERT(!p->IsInterval() || p->IsIntervalSlot() || p->IsIntervalStart());
          const uint64_t new_size = offset - owner_offset_;
          // Due to the limited range of the operation, the only way this callback ever fires is if
          // the range is actually getting trimmed.
          DEBUG_ASSERT(new_size < request_size);
          request_size = new_size;
          return ZX_ERR_STOP;
        },
        owner_offset_, owner_offset_ + request_size);
  }
  DEBUG_ASSERT(request_size >= PAGE_SIZE);

  zx_status_t status =
      owner_->page_source_->GetPages(owner_offset_, request_size, page_request, vmo_debug_info);
  // Pager page sources will never synchronously return a page.
  DEBUG_ASSERT(status != ZX_OK);
  return status;
}

zx_status_t VmCowPages::LookupCursor::DirtyRequest(uint max_request_pages,
                                                   LazyPageRequest* page_request) {
  // Dirty requests, unlike read requests, happen directly against the target, and not the owner.
  // This is because to make something dirty you must own it, i.e. target_ is already equal to
  // owner_. Unfortunately we cannot explicitly check for target_ == owner_, since owner_ doubles as
  // tracking for whether the cursor is valid, and it may have been made invalid just prior to
  // generating this dirty request, and we do not otherwise need the cursor here.
  // Instead we validate that we have no parent, and that we have a page source.
  DEBUG_ASSERT(target_ == owner_ || !IsCursorValid());
  DEBUG_ASSERT(!target_->parent_);
  DEBUG_ASSERT(target_->page_source_);
  DEBUG_ASSERT(max_request_pages > 0);
  DEBUG_ASSERT(offset_ + PAGE_SIZE * max_request_pages <= end_offset_);

  // As we know target_==owner_ there is no need to trim the requested range to any kind of visible
  // range, so just attempt to dirty the entire range.
  uint64_t dirty_len = 0;
  zx_status_t status = target_->PrepareForWriteLocked(
      VmCowRange(offset_, PAGE_SIZE * max_request_pages), page_request, &dirty_len);
  if (status == ZX_OK) {
    // If success is claimed then it must be the case that at least one page was dirtied, allowing
    // us to make progress.
    DEBUG_ASSERT(dirty_len != 0 && dirty_len <= max_request_pages * PAGE_SIZE);
  } else {
    DEBUG_ASSERT(dirty_len == 0);
  }
  return status;
}

vm_page_t* VmCowPages::LookupCursor::MaybePage(bool will_write) {
  EstablishCursor();

  // If the page is immediately usable, i.e. no dirty transitions etc needed, then we can provide
  // it. Otherwise just increment the cursor and return the nullptr.
  vm_page_t* page = CursorIsUsablePage(will_write) ? owner_cursor_->Page() : nullptr;

  if (page && mark_accessed_) {
    pmm_page_queues()->MarkAccessed(page);
  }

  IncrementCursor();

  return page;
}

uint64_t VmCowPages::LookupCursor::SkipMissingPages() {
  EstablishCursor();

  // Check if the cursor is truly empty
  if (!CursorIsEmpty() || CursorIsInIntervalZero()) {
    return 0;
  }

  uint64_t possibly_empty = visible_end_ - offset_;
  // Limit possibly_empty by the first page visible in the owner which, since our cursor is empty,
  // would also be the root vmo.
  if (possibly_empty > PAGE_SIZE) {
    owner()->page_list_.ForEveryPageInRange(
        [&](const VmPageOrMarker* p, uint64_t offset) {
          // Content should have been empty initially, so should not find anything at the start
          // offset.
          DEBUG_ASSERT(offset > owner_offset_);
          // If this is an interval sentinel, it can only be a start or slot, since we know we
          // started in a true gap outside of an interval.
          DEBUG_ASSERT(!p->IsInterval() || p->IsIntervalSlot() || p->IsIntervalStart());
          const uint64_t new_size = offset - owner_offset_;
          // Due to the limited range of the operation, the only way this callback ever fires is if
          // the range is actually getting trimmed.
          DEBUG_ASSERT(new_size < possibly_empty);
          possibly_empty = new_size;
          return ZX_ERR_STOP;
        },
        owner_offset_, owner_offset_ + possibly_empty);
  }
  // The cursor was empty, so we should have ended up with at least one page.
  DEBUG_ASSERT(possibly_empty >= PAGE_SIZE);
  DEBUG_ASSERT(IS_PAGE_ALIGNED(possibly_empty));
  DEBUG_ASSERT(possibly_empty + offset_ <= end_offset_);
  IncrementOffsetAndInvalidateCursor(possibly_empty);
  return possibly_empty / PAGE_SIZE;
}

uint VmCowPages::LookupCursor::IfExistPages(bool will_write, uint max_pages, paddr_t* paddrs) {
  // Ensure that the requested range is valid.
  DEBUG_ASSERT(offset_ + PAGE_SIZE * max_pages <= end_offset_);
  DEBUG_ASSERT(paddrs);

  EstablishCursor();

  // We only return actual pages that are ready to use right now without any dirty transitions or
  // copy-on-write or needing to mark them accessed.
  if (!CursorIsUsablePage(will_write) || mark_accessed_) {
    return 0;
  }

  // Trim max pages to the visible length of the current owner. This only has an effect when
  // target_ != owner_ as otherwise the visible_end_ is the same as end_offset_ and we already
  // validated that we are within that range.
  if (owner_ != target_) {
    max_pages = ktl::min(max_pages, static_cast<uint>((visible_end_ - offset_) / PAGE_SIZE));
  }
  DEBUG_ASSERT(max_pages > 0);

  // Take up to the max_pages as long as they exist contiguously.
  uint pages = 0;
  owner_pl_cursor_.ForEveryContiguous([&](VmPageOrMarkerRef page) {
    if (page->IsPage()) {
      paddrs[pages] = page->Page()->paddr();
      pages++;
      return pages == max_pages ? ZX_ERR_STOP : ZX_ERR_NEXT;
    }
    return ZX_ERR_STOP;
  });
  // Update the cursor to reflect the number of pages we found and are returning.
  // We could check if cursor is still valid, but it's more efficient to just invalidate it and let
  // any potential next page request recalculate it.
  IncrementOffsetAndInvalidateCursor(pages * PAGE_SIZE);
  return pages;
}

zx::result<VmCowPages::LookupCursor::RequireResult> VmCowPages::LookupCursor::RequireOwnedPage(
    bool will_write, uint max_request_pages, MultiPageRequest* page_request) {
  DEBUG_ASSERT(page_request);

  // Make sure the cursor is valid.
  EstablishCursor();

  // Convert any references to pages.
  if (CursorIsReference()) {
    // Decompress in place.
    zx_status_t status = CursorReferenceToPage(page_request->GetAnonymous());
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }

  // If page exists in the target, i.e. the owner is the target, then we handle this case separately
  // as it's the only scenario where we might be dirtying an existing committed page.
  if (owner_ == target_ && CursorIsPage()) {
    // If we're writing to a root VMO backed by a user pager, i.e. a VMO whose page source preserves
    // page contents, we might need to mark pages Dirty so that they can be written back later. This
    // is the only path that can result in a write to such a page; if the page was not present, we
    // would have already blocked on a read request the first time, and ended up here when
    // unblocked, at which point the page would be present.
    if (will_write && target_preserving_page_content_) {
      // If this page was loaned, it should be replaced with a non-loaned page, so that we can make
      // progress with marking pages dirty. PrepareForWriteLocked terminates its page walk when it
      // encounters a loaned page; loaned pages are reclaimed by evicting them and we cannot evict
      // dirty pages.
      if (owner_cursor_->Page()->is_loaned()) {
        vm_page_t* res_page = nullptr;
        DEBUG_ASSERT(is_page_clean(owner_cursor_->Page()));
        zx_status_t status =
            target_->ReplacePageLocked(owner_cursor_->Page(), offset_, /*with_loaned=*/false,
                                       &res_page, page_request->GetAnonymous());
        if (status != ZX_OK) {
          return zx::error(status);
        }
        // Cursor should remain valid and have been replaced with the page.
        DEBUG_ASSERT(CursorIsPage());
        DEBUG_ASSERT(owner_cursor_->Page() == res_page);
        DEBUG_ASSERT(!owner_cursor_->Page()->is_loaned());
      }
      // If the page is not already dirty, then generate a dirty request. The dirty request code can
      // handle the page already being dirty, this is just a short circuit optimization.
      if (!is_page_dirty(owner_cursor_->Page())) {
        zx_status_t status = DirtyRequest(max_request_pages, page_request->GetLazyDirtyRequest());
        if (status != ZX_OK) {
          if (status == ZX_ERR_SHOULD_WAIT) {
            page_request->MadeDirtyRequest();
          }
          return zx::error(status);
        }
      }
    }
    // Return the page.
    return zx::ok(CursorAsResult());
  }

  // Should there be page, but it not be owned by the target, then we are performing copy on write
  // into the target. As the target cannot have a page source do not need to worry about writes or
  // dirtying.
  if (CursorIsPage()) {
    DEBUG_ASSERT(owner_ != target_);
    vm_page_t* res_page = nullptr;
    // Although we are not returning the page, the act of forking counts as an access, and this is
    // an access regardless of whether the final returned page should be considered accessed, so
    // ignore the mark_accessed_ check here.
    pmm_page_queues()->MarkAccessed(owner_cursor_->Page());
    if (!owner()->is_hidden_locked()) {
      // Directly copying the page from the owner into the target.
      return TargetAllocateCopyPageAsResult(owner_cursor_->Page(), DirtyState::Untracked,
                                            page_request->GetAnonymous());
    }
    zx_status_t result =
        target_->CloneCowPageLocked(offset_, alloc_list_, owner_, owner_cursor_->Page(),
                                    owner_offset_, page_request->GetAnonymous(), &res_page);
    if (result != ZX_OK) {
      return zx::error(result);
    }
    target_->IncrementHierarchyGenerationCountLocked();
    // Cloning the cow page may have impacted our cursor due to a page being moved so invalidate the
    // cursor to perform a fresh lookup on the next page requested.
    IncrementOffsetAndInvalidateCursor(PAGE_SIZE);
    // This page as just allocated so no need to worry about update access times, can just return.
    return zx::ok(RequireResult{res_page, true});
  }

  // Zero content is the most complicated cases where, even if reading, dirty requests might need to
  // be performed and the resulting committed pages may / may not be dirty.
  if (CursorIsContentZero()) {
    // If the page source is preserving content (is a PagerProxy), and is configured to trap dirty
    // transitions, we first need to generate a DIRTY request *before* the zero page can be forked
    // and marked dirty. If dirty transitions are not trapped, we will fall through to allocate the
    // page and then mark it dirty below.
    //
    // Note that the check for ShouldTrapDirtyTransitions() is an optimization here.
    // PrepareForWriteLocked() would do the right thing depending on ShouldTrapDirtyTransitions(),
    // however we choose to avoid the extra work only to have it be a no-op if dirty transitions
    // should not be trapped.
    const bool target_page_dirty = TargetZeroContentSupplyDirty(will_write);
    if (target_page_dirty && target_->page_source_->ShouldTrapDirtyTransitions()) {
      zx_status_t status = DirtyRequest(max_request_pages, page_request->GetLazyDirtyRequest());
      // Since we know we have a page source that traps, and page sources will never succeed
      // synchronously, our dirty request must have 'failed'.
      DEBUG_ASSERT(status != ZX_OK);
      if (status == ZX_ERR_SHOULD_WAIT) {
        page_request->MadeDirtyRequest();
      }
      return zx::error(status);
    }
    // Allocate the page and mark it dirty or clean as previously determined.
    return TargetAllocateCopyPageAsResult(vm_get_zero_page(),
                                          target_page_dirty ? DirtyState::Dirty : DirtyState::Clean,
                                          page_request->GetAnonymous());
  }
  DEBUG_ASSERT(CursorIsEmpty());

  // Generate a read request to populate the content in the owner. Even if this is a write, we still
  // populate content first, then perform any dirty transitions / requests.
  return zx::error(ReadRequest(max_request_pages, page_request->GetReadRequest()));
}

zx::result<VmCowPages::LookupCursor::RequireResult> VmCowPages::LookupCursor::RequireReadPage(
    uint max_request_pages, MultiPageRequest* page_request) {
  DEBUG_ASSERT(page_request);

  // Make sure the cursor is valid.
  EstablishCursor();

  // If there's a page or reference, return it.
  if (CursorIsPage() || CursorIsReference()) {
    if (CursorIsReference()) {
      zx_status_t status = CursorReferenceToPage(page_request->GetAnonymous());
      if (status != ZX_OK) {
        return zx::error(status);
      }
      DEBUG_ASSERT(CursorIsPage());
    }
    return zx::ok(CursorAsResult());
  }

  // Check for zero page options.
  if (CursorIsContentZero()) {
    IncrementCursor();
    return zx::ok(RequireResult{vm_get_zero_page(), false});
  }

  // No available content, need to fetch it from the page source. ReadRequest performs all the
  // requisite asserts to ensure we are not doing this mistakenly.
  return zx::error(ReadRequest(max_request_pages, page_request->GetReadRequest()));
}

zx::result<VmCowPages::LookupCursor> VmCowPages::GetLookupCursorLocked(VmCowRange range) {
  canary_.Assert();
  DEBUG_ASSERT(!is_hidden_locked());
  DEBUG_ASSERT(!range.is_empty());
  DEBUG_ASSERT(range.is_page_aligned());
  DEBUG_ASSERT(life_cycle_ == LifeCycle::Alive);
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());

  if (unlikely(range.offset >= size_ || !range.IsBoundedBy(size_))) {
    return zx::error{ZX_ERR_OUT_OF_RANGE};
  }

  if (discardable_tracker_) {
    discardable_tracker_->assert_cow_pages_locked();
    // This vmo was discarded and has not been locked yet after the discard. Do not return any
    // pages.
    if (discardable_tracker_->WasDiscardedLocked()) {
      return zx::error{ZX_ERR_NOT_FOUND};
    }
  }

  if (is_slice_locked()) {
    return slice_parent_locked().GetLookupCursorLocked(range.OffsetBy(parent_offset_));
  }

  return zx::ok(LookupCursor(this, range));
}

zx_status_t VmCowPages::CommitRangeLocked(VmCowRange range, uint64_t* committed_len,
                                          MultiPageRequest* page_request) {
  canary_.Assert();
  LTRACEF("offset %#" PRIx64 ", len %#" PRIx64 "\n", range.offset, range.len);

  DEBUG_ASSERT(range.is_page_aligned());
  DEBUG_ASSERT(range.IsBoundedBy(size_));
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());

  if (is_slice_locked()) {
    return slice_parent_locked().CommitRangeLocked(range.OffsetBy(parent_offset_), committed_len,
                                                   page_request);
  }

  fbl::RefPtr<PageSource> root_source = GetRootPageSourceLocked();

  // If this vmo has a direct page source, then the source will provide the backing memory. For
  // children that eventually depend on a page source, we skip preallocating memory to avoid
  // potentially overallocating pages if something else touches the vmo while we're blocked on the
  // request. Otherwise we optimize things by preallocating all the pages.
  list_node page_list;
  list_initialize(&page_list);
  if (root_source == nullptr) {
    // make a pass through the list to find out how many pages we need to allocate
    size_t count = range.len / PAGE_SIZE;
    page_list_.ForEveryPageInRange(
        [&count](const auto* p, auto off) {
          if (p->IsPage()) {
            count--;
          }
          return ZX_ERR_NEXT;
        },
        range.offset, range.end());

    if (count == 0) {
      *committed_len = range.len;
      return ZX_OK;
    }

    zx_status_t status = pmm_alloc_pages(count, pmm_alloc_flags_, &page_list);
    // Ignore ZX_ERR_SHOULD_WAIT since the loop below will fall back to a page by page allocation,
    // allowing us to wait for single pages should we need to.
    if (status != ZX_OK && status != ZX_ERR_SHOULD_WAIT) {
      return status;
    }
  }

  auto list_cleanup = fit::defer([&page_list, this]() {
    if (!list_is_empty(&page_list)) {
      AssertHeld(lock_ref());
      // We are freeing pages we got from the PMM and did not end up using, so we do not own them.
      FreePagesLocked(&page_list, /*freeing_owned_pages=*/false);
    }
  });

  const uint64_t start_offset = range.offset;
  const uint64_t end = range.end();
  __UNINITIALIZED auto cursor = GetLookupCursorLocked(range);
  if (cursor.is_error()) {
    return cursor.error_value();
  }
  AssertHeld(cursor->lock_ref());
  // Commit represents an explicit desire to have pages and should not be deduped back to the zero
  // page.
  cursor->DisableZeroFork();
  cursor->GiveAllocList(&page_list);

  zx_status_t status = ZX_OK;
  uint64_t offset = start_offset;
  while (offset < end) {
    __UNINITIALIZED zx::result<VmCowPages::LookupCursor::RequireResult> result =
        cursor->RequireOwnedPage(false, static_cast<uint>((end - offset) / PAGE_SIZE),
                                 page_request);

    if (result.is_error()) {
      status = result.error_value();
      break;
    }
    offset += PAGE_SIZE;
  }
  // Record how much we were able to process.
  *committed_len = offset - start_offset;

  // Clear the alloc list from the cursor and let list_cleanup free any remaining pages.
  cursor->ClearAllocList();

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  return status;
}

zx_status_t VmCowPages::PinRangeLocked(VmCowRange range) {
  canary_.Assert();
  LTRACEF("offset %#" PRIx64 ", len %#" PRIx64 "\n", range.offset, range.len);

  DEBUG_ASSERT(range.is_page_aligned());
  DEBUG_ASSERT(range.IsBoundedBy(size_));

  if (is_slice_locked()) {
    return slice_parent_locked().PinRangeLocked(range.OffsetBy(parent_offset_));
  }

  ever_pinned_ = true;

  // Tracks our expected page offset when iterating to ensure all pages are present.
  uint64_t next_offset = range.offset;

  // Should any errors occur we need to unpin everything.
  auto pin_cleanup = fit::defer([this, offset = range.offset, &next_offset]() {
    if (next_offset > offset) {
      AssertHeld(*lock());
      UnpinLocked(VmCowRange(offset, next_offset - offset));
    }
  });

  zx_status_t status = page_list_.ForEveryPageInRange(
      [this, &next_offset](const VmPageOrMarker* p, uint64_t page_offset) {
        AssertHeld(lock_ref());
        if (page_offset != next_offset || !p->IsPage()) {
          return ZX_ERR_BAD_STATE;
        }
        vm_page_t* page = p->Page();
        DEBUG_ASSERT(page->state() == vm_page_state::OBJECT);
        DEBUG_ASSERT(!page->is_loaned());

        if (page->object.pin_count == VM_PAGE_OBJECT_MAX_PIN_COUNT) {
          return ZX_ERR_UNAVAILABLE;
        }

        page->object.pin_count++;
        if (page->object.pin_count == 1) {
          MoveToPinnedLocked(page, page_offset);
        }

        // Pinning every page in the largest vmo possible as many times as possible can't overflow
        static_assert(VmPageList::MAX_SIZE / PAGE_SIZE < UINT64_MAX / VM_PAGE_OBJECT_MAX_PIN_COUNT);
        next_offset += PAGE_SIZE;
        return ZX_ERR_NEXT;
      },
      range.offset, range.end());

  const uint64_t actual = (next_offset - range.offset) / PAGE_SIZE;
  // Count whatever pages we pinned, in the failure scenario this will get decremented on the unpin.
  pinned_page_count_ += actual;

  if (status == ZX_OK) {
    // If the missing pages were at the end of the range (or the range was empty) then our iteration
    // will have just returned ZX_OK. Perform one final check that we actually pinned the number of
    // pages we expected to.
    const uint64_t expected = range.len / PAGE_SIZE;
    if (actual != expected) {
      status = ZX_ERR_BAD_STATE;
    } else {
      pin_cleanup.cancel();
    }
  }
  return status;
}

zx_status_t VmCowPages::DecommitRangeLocked(VmCowRange range) {
  canary_.Assert();

  // Validate the size and perform our zero-length hot-path check before we recurse
  // up to our top-level ancestor.  Size bounding needs to take place relative
  // to the child the operation was originally targeted against.
  if (!range.IsBoundedBy(size_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // was in range, just zero length
  if (range.is_empty()) {
    return ZX_OK;
  }

  if (is_slice_locked()) {
    return slice_parent_locked().DecommitRangeLocked(range.OffsetBy(parent_offset_));
  }

  // Currently, we can't decommit if the absence of a page doesn't imply zeroes.
  if (parent_ || is_source_preserving_page_content()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // VmObjectPaged::DecommitRange() rejects is_contiguous() VMOs (for now).
  DEBUG_ASSERT(can_decommit());

  // Demand offset and length be correctly aligned to not give surprising user semantics.
  if (!range.is_page_aligned()) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Assume we will free some pages and increment the gen count.
  IncrementHierarchyGenerationCountLocked();

  return UnmapAndFreePagesLocked(range.offset, range.len).status_value();
}

zx::result<uint64_t> VmCowPages::UnmapAndFreePagesLocked(uint64_t offset, uint64_t len) {
  canary_.Assert();

  if (AnyPagesPinnedLocked(offset, len)) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  LTRACEF("start offset %#" PRIx64 ", end %#" PRIx64 "\n", offset, offset + len);

  // We've already trimmed the range in DecommitRangeLocked().
  DEBUG_ASSERT(InRange(offset, len, size_));

  // Verify page alignment.
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len) || (offset + len == size_));

  // DecommitRangeLocked() will call this function only on a VMO with no parent. The only clone
  // types that support OP_DECOMMIT are slices, for which we will recurse up to the root.
  DEBUG_ASSERT(!parent_);

  // unmap all of the pages in this range on all the mapping regions
  RangeChangeUpdateLocked(VmCowRange(offset, len), RangeChangeOp::Unmap);

  list_node_t freed_list;
  list_initialize(&freed_list);
  __UNINITIALIZED BatchPQRemove page_remover(&freed_list);

  page_list_.RemovePages(page_remover.RemovePagesCallback(), offset, offset + len);
  page_remover.Flush();
  if (!list_is_empty(&freed_list)) {
    FreePagesLocked(&freed_list, /*freeing_owned_pages=*/true);
  }

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  return zx::ok(page_remover.freed_count());
}

bool VmCowPages::PageWouldReadZeroLocked(uint64_t page_offset) {
  canary_.Assert();

  DEBUG_ASSERT(IS_PAGE_ALIGNED(page_offset));
  DEBUG_ASSERT(page_offset < size_);
  const VmPageOrMarker* slot = page_list_.Lookup(page_offset);
  if (slot && slot->IsMarker()) {
    // This is already considered zero as there's a marker.
    return true;
  }
  if (is_source_preserving_page_content() &&
      ((slot && slot->IsIntervalZero()) || page_list_.IsOffsetInZeroInterval(page_offset))) {
    // Pages in zero intervals are supplied as zero by the kernel.
    return true;
  }
  // If we don't have a page or reference here we need to check our parent.
  if (!slot || !slot->IsPageOrRef()) {
    VmCowPages* page_owner;
    uint64_t owner_offset;
    if (!FindInitialPageContentLocked(page_offset, &page_owner, &owner_offset, nullptr).current()) {
      // Parent doesn't have a page either, so would also read as zero, assuming no page source.
      return GetRootPageSourceLocked() == nullptr;
    }
  }
  // Content either locally or in our parent, assume it is non-zero and return false.
  return false;
}

zx_status_t VmCowPages::ZeroPagesPreservingContentLocked(uint64_t page_start_base,
                                                         uint64_t page_end_base, bool dirty_track,
                                                         list_node_t* freed_list,
                                                         MultiPageRequest* page_request,
                                                         uint64_t* processed_len_out) {
  // Validate inputs.
  DEBUG_ASSERT(IS_PAGE_ALIGNED(page_start_base) && IS_PAGE_ALIGNED(page_end_base));
  DEBUG_ASSERT(page_end_base <= size_);
  DEBUG_ASSERT(is_source_preserving_page_content());

  // Give us easier names for our range.
  const uint64_t start = page_start_base;
  const uint64_t end = page_end_base;

  if (start == end) {
    return ZX_OK;
  }

  // If we're not asked to dirty track, we will need to drop pages, because if a page is present it
  // is going to be in one of the dirty tracked states (Clean, Dirty, AwaitingClean). So check for
  // any pinned pages first.
  if (!dirty_track && AnyPagesPinnedLocked(start, end - start)) {
    return ZX_ERR_BAD_STATE;
  }

  IncrementHierarchyGenerationCountLocked();

  // Inserting zero intervals can modify the page list such that new nodes are added and deleted.
  // So we cannot safely insert zero intervals while iterating the page list. The pattern we
  // follow here is:
  // 1. Traverse the page list to find a range that can be represented by a zero interval instead.
  // 2. When such a range is found, break out of the traversal, and insert the zero interval.
  // 3. Advance past the zero interval we inserted and resume the traversal from there, until
  // we've covered the entire range.

  // The start offset at which to start the next traversal loop.
  uint64_t next_start_offset = start;
  // Dirty state for zero intervals we insert.
  const VmPageOrMarker::IntervalDirtyState required_state =
      dirty_track ? VmPageOrMarker::IntervalDirtyState::Dirty
                  : VmPageOrMarker::IntervalDirtyState::Untracked;
  do {
    // Track whether we find ourselves in a zero interval.
    bool in_interval = false;
    // The start of the zero interval if we are in one.
    uint64_t interval_start = next_start_offset;
    const uint64_t prev_start_offset = next_start_offset;
    // State tracking information for inserting a new zero interval.
    struct {
      bool add_zero_interval;
      uint64_t start;
      uint64_t end;
      bool replace_page;
      bool overwrite_interval;
    } state = {.add_zero_interval = false};

    zx_status_t status = page_list_.RemovePagesAndIterateGaps(
        [&](VmPageOrMarker* p, uint64_t off) {
          // We cannot have references in pager-backed VMOs.
          DEBUG_ASSERT(!p->IsReference());

          // If this is a page, see if we can remove it and absorb it into a zero interval.
          if (p->IsPage()) {
            AssertHeld(lock_ref());
            if (p->Page()->object.pin_count > 0) {
              DEBUG_ASSERT(dirty_track);
              // Cannot remove this page if it is pinned. Lookup the page and zero it. Looking up
              // ensures that we request dirty transition if needed by the pager.
              LookupCursor cursor(this, VmCowRange(off, PAGE_SIZE));
              AssertHeld(cursor.lock_ref());
              zx::result<LookupCursor::RequireResult> result =
                  cursor.RequireOwnedPage(true, 1, page_request);
              if (result.is_error()) {
                return result.error_value();
              }
              DEBUG_ASSERT(result->page == p->Page());
              // Zero the page we looked up.
              ZeroPage(result->page->paddr());
              *processed_len_out += PAGE_SIZE;
              next_start_offset = off + PAGE_SIZE;
              return ZX_ERR_NEXT;
            }
            // Break out of the traversal. We can release the page and add a zero interval
            // instead.
            state = {.add_zero_interval = true,
                     .start = off,
                     .end = off,
                     .replace_page = true,
                     .overwrite_interval = false};
            return ZX_ERR_STOP;
          }

          // Otherwise this is a marker or zero interval, in which case we already have zeroes, but
          // we might need to change the dirty state.
          DEBUG_ASSERT(p->IsMarker() || p->IsIntervalZero());
          if (p->IsIntervalStart()) {
            // Track the interval start so we know how much to add to processed_len_out later.
            interval_start = off;
            in_interval = true;
            if (p->GetZeroIntervalDirtyState() != required_state) {
              // If we find the matching end, we will update state.end with the correct offset.
              // Do not terminate the traversal yet.
              state = {.add_zero_interval = true,
                       .start = interval_start,
                       .end = UINT64_MAX,
                       .replace_page = false,
                       .overwrite_interval = true};
            }
          } else if (p->IsIntervalEnd()) {
            if (p->GetZeroIntervalDirtyState() != required_state) {
              state = {.add_zero_interval = true,
                       .start = in_interval ? interval_start : UINT64_MAX,
                       .end = off,
                       .replace_page = false,
                       .overwrite_interval = true};
              return ZX_ERR_STOP;
            }
            // Add the range from interval start to end.
            *processed_len_out += (off + PAGE_SIZE - interval_start);
            in_interval = false;
          } else {
            // This is either a single interval slot or a marker. Terminate the traversal to
            // overwrite with a zero interval if:
            //  - this is an interval slot with a different dirty state, OR
            //  - this is a marker and we're asked to not dirty track, since a marker is a clean
            //  zero page.
            if (p->IsMarker() && !dirty_track) {
              // Release the marker so that it can be replaced by a gap by the traversal loop first,
              // where the new zero interval will then be added.
              *p = VmPageOrMarker::Empty();
            }
            if (p->IsEmpty() ||
                (p->IsIntervalSlot() && p->GetZeroIntervalDirtyState() != required_state)) {
              state = {.add_zero_interval = true,
                       .start = off,
                       .end = off,
                       .replace_page = false,
                       .overwrite_interval = p->IsIntervalSlot()};
              return ZX_ERR_STOP;
            }
            *processed_len_out += PAGE_SIZE;
          }
          next_start_offset = off + PAGE_SIZE;
          return ZX_ERR_NEXT;
        },
        [&](uint64_t gap_start, uint64_t gap_end) {
          AssertHeld(lock_ref());
          // This gap will be replaced with a zero interval. Invalidate any read requests in this
          // range. Since we have just validated that this is a gap in the page list we can directly
          // call OnPagesSupplied, instead of iterating through the gaps using
          // InvalidateReadRequestsLocked
          page_source_->OnPagesSupplied(gap_start, gap_end - gap_start);
          // We have found a new zero interval to insert. Break out of the traversal.
          state = {.add_zero_interval = true,
                   .start = gap_start,
                   .end = gap_end - PAGE_SIZE,
                   .replace_page = false,
                   .overwrite_interval = false};
          return ZX_ERR_STOP;
        },
        next_start_offset, end);
    // Bubble up any errors from LookupCursor.
    if (status != ZX_OK) {
      return status;
    }

    // Add any new zero interval.
    if (state.add_zero_interval) {
      if (state.replace_page) {
        DEBUG_ASSERT(state.start == state.end);
        vm_page_t* page = page_list_.ReplacePageWithZeroInterval(state.start, required_state);
        DEBUG_ASSERT(page->object.pin_count == 0);
        pmm_page_queues()->Remove(page);
        DEBUG_ASSERT(!list_in_list(&page->queue_node));
        list_add_tail(freed_list, &page->queue_node);
      } else if (state.overwrite_interval) {
        uint64_t old_start = state.start;
        uint64_t old_end = state.end;
        if (state.start == UINT64_MAX) {
          state.start = next_start_offset;
        }
        if (state.end == UINT64_MAX) {
          state.end = end - PAGE_SIZE;
        }
        status = page_list_.OverwriteZeroInterval(old_start, old_end, state.start, state.end,
                                                  required_state);
      } else {
        status = page_list_.AddZeroInterval(state.start, state.end + PAGE_SIZE, required_state);
      }
      if (status != ZX_OK) {
        DEBUG_ASSERT(status == ZX_ERR_NO_MEMORY);
        return status;
      }
      *processed_len_out += (state.end - state.start + PAGE_SIZE);
      next_start_offset = state.end + PAGE_SIZE;
    } else {
      // Handle the last partial interval. Or the case where we did not advance next_start_offset at
      // all, which can only happen if the range fell entirely inside an interval.
      if (in_interval || next_start_offset == prev_start_offset) {
        // If the range fell entirely inside an interval, verify that it was indeed a zero interval.
        DEBUG_ASSERT(next_start_offset != prev_start_offset ||
                     page_list_.IsOffsetInZeroInterval(next_start_offset));
        // If entirely inside an interval, we have one of two possibilities:
        //  (1) The interval is already in required_state in which case we don't need to do
        //  anything.
        //  (2) The interval is not in required_state. We do not expect this case in practice, so
        //  instead of splitting up a zero interval in the middle just to change its dirty state,
        //  claim that we processed the range.
        *processed_len_out += (end - interval_start);
        next_start_offset = end;
      }
    }
    // Ensure we're making progress.
    DEBUG_ASSERT(next_start_offset > prev_start_offset);
  } while (next_start_offset < end);

  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  return ZX_OK;
}

zx_status_t VmCowPages::ZeroPagesLocked(uint64_t page_start_base, uint64_t page_end_base,
                                        bool dirty_track, MultiPageRequest* page_request,
                                        uint64_t* zeroed_len_out) {
  canary_.Assert();

  DEBUG_ASSERT(page_start_base <= page_end_base);
  DEBUG_ASSERT(page_end_base <= size_);
  DEBUG_ASSERT(IS_PAGE_ALIGNED(page_start_base));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(page_end_base));
  ASSERT(zeroed_len_out);

  // Forward any operations on slices up to the original non slice parent.
  if (is_slice_locked()) {
    return slice_parent_locked().ZeroPagesLocked(page_start_base + parent_offset_,
                                                 page_end_base + parent_offset_, dirty_track,
                                                 page_request, zeroed_len_out);
  }

  // This function tries to zero pages as optimally as possible for most cases, so we attempt
  // increasingly expensive actions only if certain preconditions do not allow us to perform the
  // cheaper action. Broadly speaking, the sequence of actions that are attempted are as follows.
  //  1) Try to decommit the entire range at once if the VMO allows it.
  //  2) Otherwise, try to decommit each page if the VMO allows it and doing so doesn't expose
  //  content in the parent (if any) that shouldn't be visible.
  //  3) Otherwise, if this is a child VMO and there is no committed page yet, allocate a zero page.
  //  4) Otherwise, look up the page, faulting it in if necessary, and zero the page. If the page
  //  source needs to supply or dirty track the page, a page request is initialized and we return
  //  early with ZX_ERR_SHOULD_WAIT. The caller is expected to wait on the page request, and then
  //  retry. On the retry, we should be able to look up the page successfully and zero it.

  // First try and do the more efficient decommit. We prefer/ decommit as it performs work in the
  // order of the number of committed pages, instead of work in the order of size of the range. An
  // error from DecommitRangeLocked indicates that the VMO is not of a form that decommit can safely
  // be performed without exposing data that we shouldn't between children and parents, but no
  // actual state will have been changed. Should decommit succeed we are done, otherwise we will
  // have to handle each offset individually.
  //
  // Zeroing doesn't decommit pages of contiguous VMOs.
  if (can_decommit_zero_pages_locked()) {
    zx_status_t status =
        DecommitRangeLocked(VmCowRange(page_start_base, page_end_base - page_start_base));
    if (status == ZX_OK) {
      *zeroed_len_out = page_end_base - page_start_base;
      return ZX_OK;
    }

    // Unmap any page that is touched by this range in any of our, or our childrens, mapping
    // regions. We do this on the assumption we are going to be able to free pages either completely
    // or by turning them into markers and it's more efficient to unmap once in bulk here.
    RangeChangeUpdateLocked(VmCowRange(page_start_base, page_end_base - page_start_base),
                            RangeChangeOp::Unmap);
  }

  // We stack-own loaned pages from when they're removed until they're freed.
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  // Pages removed from this object are put into freed_list, while pages removed from any ancestor
  // are put into ancestor_freed_list. This is so that freeing of both the lists can be handled
  // correctly, by passing the correct value for freeing_owned_pages in the call to
  // FreePagesLocked().
  list_node_t freed_list;
  list_initialize(&freed_list);
  list_node_t ancestor_freed_list;
  list_initialize(&ancestor_freed_list);

  // See also free_any_pages below, which intentionally frees incrementally.
  auto auto_free = fit::defer([this, &freed_list, &ancestor_freed_list]() {
    AssertHeld(lock_ref());
    if (!list_is_empty(&freed_list)) {
      FreePagesLocked(&freed_list, /*freeing_owned_pages=*/true);
    }
    if (!list_is_empty(&ancestor_freed_list)) {
      FreePagesLocked(&ancestor_freed_list, /*freeing_owned_pages=*/false);
    }
  });

  // Ideally we just collect up pages and hand them over to the pmm all at the end, but if we need
  // to allocate any pages then we would like to ensure that we do not cause total memory to peak
  // higher due to squirreling these pages away.
  auto free_any_pages = [this, &freed_list, &ancestor_freed_list] {
    AssertHeld(lock_ref());
    if (!list_is_empty(&freed_list)) {
      FreePagesLocked(&freed_list, /*freeing_owned_pages=*/true);
    }
    if (!list_is_empty(&ancestor_freed_list)) {
      FreePagesLocked(&ancestor_freed_list, /*freeing_owned_pages=*/false);
    }
  };

  // Give us easier names for our range.
  const uint64_t start = page_start_base;
  const uint64_t end = page_end_base;

  // If the VMO is directly backed by a page source that preserves content, it should be the root
  // VMO of the hierarchy.
  DEBUG_ASSERT(!is_source_preserving_page_content() || !parent_);

  // If the page source preserves content, we can perform efficient zeroing by inserting dirty zero
  // intervals. Handle this case separately.
  if (is_source_preserving_page_content()) {
    return ZeroPagesPreservingContentLocked(start, end, dirty_track, &freed_list, page_request,
                                            zeroed_len_out);
  }
  // dirty_track has no meaning for VMOs without page sources that preserve content, so ignore it
  // for the remainder of the function.

  // Increment the gen count early as it's possible to fail part way through and this function
  // doesn't unroll its actions. If we were able to successfully decommit pages above,
  // DecommitRangeLocked would have incremented the gen count already, so we can do this after the
  // decommit attempt.
  //
  // Zeroing pages of a contiguous VMO doesn't commit or decommit any pages currently, but we
  // increment the generation count anyway in case that changes in future, and to keep the tests
  // more consistent.
  IncrementHierarchyGenerationCountLocked();

  // Helper lambda to determine if this VMO can see parent contents at offset, or if a length is
  // specified as well in the range [offset, offset + length).
  auto can_see_parent = [this](uint64_t offset, uint64_t length = PAGE_SIZE) TA_REQ(lock()) {
    if (!parent_) {
      return false;
    }
    return offset < parent_limit_ && offset + length <= parent_limit_;
  };

  // This is a lambda as it only makes sense to talk about parent mutability when we have a parent
  // for the offset being considered.
  auto parent_immutable = [can_see_parent, this](uint64_t offset) TA_REQ(lock()) {
    // TODO(johngro): remove this explicit unused-capture warning suppression
    // when https://bugs.llvm.org/show_bug.cgi?id=35450 gets fixed.
    (void)can_see_parent;  // used only in DEBUG_ASSERT
    DEBUG_ASSERT(can_see_parent(offset));
    return parent_locked().is_hidden_locked();
  };

  // Finding the initial page content is expensive, but we only need to call it under certain
  // circumstances scattered in the code below. The lambda get_initial_page_content() will lazily
  // fetch and cache the details. This avoids us calling it when we don't need to, or calling it
  // more than once.
  struct InitialPageContent {
    bool inited = false;
    VmCowPages* page_owner;
    uint64_t owner_offset;
    uint64_t cached_offset;
    VmPageOrMarkerRef page_or_marker;
  } initial_content_;
  auto get_initial_page_content = [&initial_content_, can_see_parent, this](uint64_t offset)
                                      TA_REQ(lock()) -> const InitialPageContent& {
    // TODO(johngro): remove this explicit unused-capture warning suppression
    // when https://bugs.llvm.org/show_bug.cgi?id=35450 gets fixed.
    (void)can_see_parent;  // used only in DEBUG_ASSERT

    // If there is no cached page content or if we're looking up a different offset from the cached
    // one, perform the lookup.
    if (!initial_content_.inited || offset != initial_content_.cached_offset) {
      DEBUG_ASSERT(can_see_parent(offset));
      VmPageOrMarkerRef page_or_marker =
          FindInitialPageContentLocked(offset, &initial_content_.page_owner,
                                       &initial_content_.owner_offset, nullptr)
              .current();
      // We only care about the parent having a 'true' vm_page for content. If the parent has a
      // marker then it's as if the parent has no content since that's a zero page anyway, which is
      // what we are trying to achieve.
      initial_content_.page_or_marker = page_or_marker;
      initial_content_.inited = true;
      initial_content_.cached_offset = offset;
    }
    DEBUG_ASSERT(offset == initial_content_.cached_offset);
    return initial_content_;
  };

  // Helper lambda to determine if parent has content at the specified offset.
  auto parent_has_content = [get_initial_page_content](uint64_t offset) TA_REQ(lock()) {
    const VmPageOrMarkerRef& page_or_marker = get_initial_page_content(offset).page_or_marker;
    return page_or_marker && page_or_marker->IsPageOrRef();
  };

  // In the ideal case we can zero by making there be an Empty slot in our page list. This is true
  // when we're not specifically avoiding decommit on zero and there is nothing pinned.
  //
  // Note that this lambda is only checking for pre-conditions in *this* VMO which allow us to
  // represent zeros with an empty slot. We will combine this check with additional checks for
  // contents visible through the parent, if applicable.
  auto can_decommit_slot = [this](const VmPageOrMarker* slot, uint64_t offset) TA_REQ(lock()) {
    if (!can_decommit_zero_pages_locked() ||
        (slot && slot->IsPage() && slot->Page()->object.pin_count > 0)) {
      return false;
    }
    DEBUG_ASSERT(!is_source_preserving_page_content());
    return true;
  };

  // Like can_decommit_slot but for a range.
  auto can_decommit_slots_in_range = [this](uint64_t offset, uint64_t length) TA_REQ(lock()) {
    if (!can_decommit_zero_pages_locked() || AnyPagesPinnedLocked(offset, length)) {
      return false;
    }
    DEBUG_ASSERT(!is_source_preserving_page_content());
    return true;
  };

  // Helper lambda to zero the slot at offset either by inserting a marker or by zeroing the actual
  // page as applicable. The return codes match those expected for VmPageList traversal.
  auto zero_slot = [&](VmPageOrMarker* slot, uint64_t offset) TA_REQ(lock()) {
    // Ideally we will use a marker, but we can only do this if we can point to a committed page
    // to justify the allocation of the marker (i.e. we cannot allocate infinite markers with no
    // committed pages). A committed page in this case exists if the parent has any content.
    // Otherwise, we'll need to zero an actual page.
    if (!can_decommit_slot(slot, offset) || !parent_has_content(offset)) {
      // We might allocate a new page below. Free any pages we've accumulated first.
      free_any_pages();

      // If we're here because of !parent_has_content() and slot doesn't have a page, we can simply
      // allocate a zero page to replace the empty slot. Otherwise, we'll have to look up the page
      // and zero it.
      //
      // We could technically fall through to GetLookupCursorLocked even for an empty slot and let
      // RequirePage allocate a new page and zero it, but we want to avoid having to redundantly
      // zero a newly forked zero page.
      if (!slot && can_see_parent(offset) && !parent_has_content(offset)) {
        // We could only have ended up here if the parent was mutable or if there is a pager-backed
        // root, otherwise we should have been able to treat an empty slot as zero (decommit a
        // committed page) and return early above.
        DEBUG_ASSERT(!parent_immutable(offset) || is_root_source_user_pager_backed_locked());
        // We will try to insert a new zero page below. Note that at this point we know that this is
        // not a contiguous VMO (which cannot have arbitrary zero pages inserted into it). We
        // checked for can_see_parent just now and contiguous VMOs do not support (non-slice)
        // clones. Besides, if the slot was empty we should have moved on when we found the gap in
        // the page list traversal as the contiguous page source zeroes supplied pages by default.
        DEBUG_ASSERT(!is_source_supplying_specific_physical_pages());

        // Allocate a new page, it will be zeroed in the process.
        vm_page_t* p;
        // Do not pass our freed_list here as this takes an |alloc_list| list to allocate from.
        zx_status_t status =
            AllocateCopyPage(vm_get_zero_page_paddr(), nullptr, page_request->GetAnonymous(), &p);
        if (status != ZX_OK) {
          return status;
        }
        auto result = AddPageLocked(offset, VmPageOrMarker::Page(p), CanOverwriteContent::Zero,
                                    /*do_range_update=*/false);
        // Absent bugs, AddPageLocked() can only return ZX_ERR_NO_MEMORY.
        if (result.is_error()) {
          ASSERT(result.status_value() == ZX_ERR_NO_MEMORY);
        }
        DEBUG_ASSERT(!result->IsPageOrRef());
        return ZX_ERR_NEXT;
      }

      // Lookup the page which will potentially fault it in via the page source. Zeroing is
      // equivalent to a VMO write with zeros, so simulate a write fault.
      zx::result<VmCowPages::LookupCursor> cursor =
          GetLookupCursorLocked(VmCowRange(offset, PAGE_SIZE));
      if (cursor.is_error()) {
        return cursor.error_value();
      }
      AssertHeld(cursor->lock_ref());
      auto result = cursor->RequirePage(true, 1, page_request);
      if (result.is_error()) {
        return result.error_value();
      }
      ZeroPage(result->page->paddr());
      return ZX_ERR_NEXT;
    }

    DEBUG_ASSERT(parent_ && parent_has_content(offset));
    // Validate we can insert our own pages/content.
    DEBUG_ASSERT(!is_source_supplying_specific_physical_pages());

    // We are able to insert a marker, but if our page content is from a hidden owner we need to
    // perform slightly more complex cow forking.
    const InitialPageContent& content = get_initial_page_content(offset);
    AssertHeld(content.page_owner->lock_ref());
    if (!slot && content.page_owner->is_hidden_locked()) {
      free_any_pages();
      // TODO(https://fxbug.dev/42138396): This could be more optimal since unlike a regular cow
      // clone, we are not going to actually need to read the target page we are cloning, and hence
      // it does not actually need to get converted.
      if (content.page_or_marker->IsReference()) {
        zx_status_t result = content.page_owner->ReplaceReferenceWithPageLocked(
            content.page_or_marker, content.owner_offset, page_request->GetAnonymous());
        if (result != ZX_OK) {
          return result;
        }
      }
      zx_status_t result = CloneCowPageAsZeroLocked(
          offset, &ancestor_freed_list, content.page_owner, content.page_or_marker->Page(),
          content.owner_offset, page_request->GetAnonymous());
      if (result != ZX_OK) {
        return result;
      }
      return ZX_ERR_NEXT;
    }

    // Remove any page that could be hanging around in the slot and replace it with a marker.
    auto result = AddPageLocked(offset, VmPageOrMarker::Marker(), CanOverwriteContent::NonZero,
                                /*do_range_update=*/false);
    // Absent bugs, AddPageLocked() can only return ZX_ERR_NO_MEMORY.
    if (result.is_error()) {
      ASSERT(result.status_value() == ZX_ERR_NO_MEMORY);
      return result.status_value();
    }
    VmPageOrMarker& released_page = *result;
    // Free the old page.
    if (released_page.IsPage()) {
      vm_page_t* page = released_page.ReleasePage();
      DEBUG_ASSERT(page->object.pin_count == 0);
      // Already handled pager backed VMOs previously, and loaned pages cannot appear in anonymous
      // VMOs.
      DEBUG_ASSERT(!page->is_loaned());
      pmm_page_queues()->Remove(page);
      DEBUG_ASSERT(!list_in_list(&page->queue_node));
      list_add_tail(&freed_list, &page->queue_node);
    } else if (released_page.IsReference()) {
      FreeReference(released_page.ReleaseReference());
    }
    return ZX_ERR_NEXT;
  };

  *zeroed_len_out = 0;
  // Main page list traversal loop to remove any existing pages / markers, zero existing pages, and
  // also insert any new markers / zero pages in gaps as applicable. We use the VmPageList traversal
  // helper here instead of iterating over each offset in the range so we can efficiently skip over
  // gaps if possible.
  zx_status_t status = page_list_.RemovePagesAndIterateGaps(
      [&](VmPageOrMarker* slot, uint64_t offset) {
        AssertHeld(lock_ref());

        // We don't expect intervals in non pager-backed VMOs.
        DEBUG_ASSERT(!slot->IsInterval());

        // Contiguous VMOs cannot have markers.
        DEBUG_ASSERT(!direct_source_supplies_zero_pages() || !slot->IsMarker());

        // First see if we can simply get done with an empty slot in the page list. This VMO should
        // allow decommitting a page at this offset when zeroing. Additionally, one of the following
        // conditions should hold w.r.t. to the parent:
        //  * This offset does not relate to our parent, or we don't have a parent.
        //  * This offset does relate to our parent, but our parent is immutable, currently
        //  zero at this offset and there is no pager-backed root VMO.
        if (can_decommit_slot(slot, offset) &&
            (!can_see_parent(offset) || (parent_immutable(offset) && !parent_has_content(offset) &&
                                         !is_root_source_user_pager_backed_locked()))) {
          if (slot->IsPage()) {
            vm_page_t* page = slot->ReleasePage();
            // Already handled pager backed VMOs previously, and loaned pages cannot appear in
            // anonymous VMOs.
            DEBUG_ASSERT(!page->is_loaned());
            pmm_page_queues()->Remove(page);
            DEBUG_ASSERT(!list_in_list(&page->queue_node));
            list_add_tail(&freed_list, &page->queue_node);
          } else if (slot->IsReference()) {
            FreeReference(slot->ReleaseReference());
          } else {
            // If this is a marker, simply make the slot empty.
            *slot = VmPageOrMarker::Empty();
          }
          // We successfully zeroed this offset. Move on to the next offset.
          *zeroed_len_out += PAGE_SIZE;
          return ZX_ERR_NEXT;
        }

        // If there's already a marker then we can avoid any second guessing and leave the marker
        // alone.
        if (slot->IsMarker()) {
          *zeroed_len_out += PAGE_SIZE;
          return ZX_ERR_NEXT;
        }

        // The only time we would reach here and *not* have a parent is if we could not decommit a
        // page at this offset when zeroing.
        DEBUG_ASSERT(!can_decommit_slot(slot, offset) || parent_);

        // Now we know that we need to do something active to make this zero, either through a
        // marker or a page.
        zx_status_t status = zero_slot(slot, offset);
        if (status == ZX_ERR_NEXT) {
          // If we were able to successfully zero this slot, move on to the next offset.
          *zeroed_len_out += PAGE_SIZE;
        }
        return status;
      },
      [&](uint64_t gap_start, uint64_t gap_end) {
        AssertHeld(lock_ref());
        if (direct_source_supplies_zero_pages()) {
          // Already logically zero - don't commit pages to back the zeroes if they're not already
          // committed.  This is important for contiguous VMOs, as we don't use markers for
          // contiguous VMOs, and allocating a page below to hold zeroes would not be asking the
          // page_source_ for the proper physical page. This prevents allocating an arbitrary
          // physical page to back the zeroes.
          *zeroed_len_out += (gap_end - gap_start);
          return ZX_ERR_NEXT;
        }

        // If empty slots imply zeroes, and the gap does not see parent contents, we already have
        // zeroes.
        if (can_decommit_slots_in_range(gap_start, gap_end - gap_start) &&
            !can_see_parent(gap_start, gap_end - gap_start)) {
          *zeroed_len_out += (gap_end - gap_start);
          return ZX_ERR_NEXT;
        }

        // Otherwise fall back to examining each offset in the gap to determine the action to
        // perform.
        for (uint64_t offset = gap_start; offset < gap_end;
             offset += PAGE_SIZE, *zeroed_len_out += PAGE_SIZE) {
          // First see if we can simply get done with an empty slot in the page list. This VMO
          // should allow decommitting a page at this offset when zeroing. Additionally, one of the
          // following conditions should hold w.r.t. to the parent:
          //  * This offset does not relate to our parent, or we don't have a parent.
          //  * This offset does relate to our parent, but our parent is immutable, currently
          //  zero at this offset and there is no pager-backed root VMO.
          if (can_decommit_slot(nullptr, offset) &&
              (!can_see_parent(offset) ||
               (parent_immutable(offset) && !parent_has_content(offset) &&
                !is_root_source_user_pager_backed_locked()))) {
            continue;
          }

          // The only time we would reach here and *not* have a parent is if we could not decommit a
          // page at this offset when zeroing.
          DEBUG_ASSERT(!can_decommit_slot(nullptr, offset) || parent_);

          // Now we know that we need to do something active to make this zero, either through a
          // marker or a page.
          zx_status_t status = zero_slot(nullptr, offset);
          if (status != ZX_ERR_NEXT) {
            return status;
          }
        }

        return ZX_ERR_NEXT;
      },
      start, end);

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  return status;
}

void VmCowPages::MoveToPinnedLocked(vm_page_t* page, uint64_t offset) {
  pmm_page_queues()->MoveToWired(page);
}

void VmCowPages::MoveToNotPinnedLocked(vm_page_t* page, uint64_t offset) {
  PageQueues* pq = pmm_page_queues();
  if (is_source_preserving_page_content()) {
    DEBUG_ASSERT(is_page_dirty_tracked(page));
    // We can only move Clean pages to the pager backed queues as they track age information for
    // eviction; only Clean pages can be evicted. Pages in AwaitingClean and Dirty are protected
    // from eviction in the Dirty queue.
    if (is_page_clean(page)) {
      if (high_priority_count_ != 0) {
        // If this VMO is high priority then do not place in the pager backed queue as that is
        // reclaimable, place in the high priority queue instead.
        pq->MoveToHighPriority(page);
      } else {
        pq->MoveToReclaim(page);
      }
    } else {
      DEBUG_ASSERT(!page->is_loaned());
      pq->MoveToPagerBackedDirty(page);
    }
  } else {
    // Place pages from contiguous VMOs in the wired queue, as they are notionally pinned until the
    // owner explicitly releases them.
    if (can_decommit_zero_pages_locked()) {
      if (high_priority_count_ != 0 && !pq->ReclaimIsOnlyPagerBacked()) {
        // If anonymous pages are reclaimable, and this VMO is high priority, then places our pages
        // in the high priority queue instead of the anonymous one to avoid reclamation.
        pq->MoveToHighPriority(page);
      } else if (is_discardable()) {
        pq->MoveToReclaim(page);
      } else {
        pq->MoveToAnonymous(page);
      }
    } else {
      pq->MoveToWired(page);
    }
  }
}

void VmCowPages::SetNotPinnedLocked(vm_page_t* page, uint64_t offset) {
  PageQueues* pq = pmm_page_queues();
  if (is_source_preserving_page_content()) {
    DEBUG_ASSERT(is_page_dirty_tracked(page));
    // We can only move Clean pages to the pager backed queues as they track age information for
    // eviction; only Clean pages can be evicted. Pages in AwaitingClean and Dirty are protected
    // from eviction in the Dirty queue.
    if (is_page_clean(page)) {
      if (high_priority_count_ != 0) {
        // If this VMO is high priority then do not place in the pager backed queue as that is
        // reclaimable, place in the high priority queue instead.
        pq->SetHighPriority(page, this, offset);
      } else {
        pq->SetReclaim(page, this, offset);
      }
    } else {
      DEBUG_ASSERT(!page->is_loaned());
      pq->SetPagerBackedDirty(page, this, offset);
    }
  } else {
    // Place pages from contiguous VMOs in the wired queue, as they are notionally pinned until the
    // owner explicitly releases them.
    if (can_decommit_zero_pages_locked()) {
      if (high_priority_count_ != 0 && !pq->ReclaimIsOnlyPagerBacked()) {
        // If anonymous pages are reclaimable, and this VMO is high priority, then places our pages
        // in the high priority queue instead of the anonymous one to avoid reclamation.
        pq->SetHighPriority(page, this, offset);
      } else if (is_discardable()) {
        pq->SetReclaim(page, this, offset);
      } else {
        pq->SetAnonymous(page, this, offset);
      }
    } else {
      pq->SetWired(page, this, offset);
    }
  }
}

void VmCowPages::PromoteRangeForReclamationLocked(VmCowRange range) {
  canary_.Assert();

  // Hints only apply to pager backed VMOs.
  if (!can_root_source_evict_locked()) {
    return;
  }
  // Zero lengths have no work to do.
  if (range.is_empty()) {
    return;
  }

  // Walk up the tree to get to the root parent. A raw pointer is fine as we're holding the lock and
  // won't drop it in this function.
  // We need the root to check if the pages are owned by the root below. Hints only apply to pages
  // in the root that are visible to this child, not to pages the child might have forked.
  const VmCowPages* const root = GetRootLocked();

  uint64_t start_offset = ROUNDDOWN(range.offset, PAGE_SIZE);
  uint64_t end_offset = ROUNDUP(range.end(), PAGE_SIZE);

  __UNINITIALIZED zx::result<VmCowPages::LookupCursor> cursor =
      GetLookupCursorLocked(VmCowRange(start_offset, end_offset - start_offset));
  if (cursor.is_error()) {
    return;
  }
  // Do not consider pages accessed as the goal is reclaim them, not consider them used.
  cursor->DisableMarkAccessed();
  AssertHeld(cursor->lock_ref());
  while (start_offset < end_offset) {
    // Lookup the page if it exists, but do not let it get allocated or say we are writing to it.
    // On success or failure this causes the cursor to go to the next offset.
    vm_page_t* page = cursor->MaybePage(false);
    if (page) {
      // Check to see if the page is owned by the root VMO. Hints only apply to the root.
      // Don't move a pinned page or a dirty page to the DontNeed queue.
      // Note that this does not unset the always_need bit if it has been previously set. The
      // always_need hint is sticky.
      if (page->object.get_object() == root && page->object.pin_count == 0 && is_page_clean(page)) {
        pmm_page_queues()->MoveToReclaimDontNeed(page);
        vm_vmo_dont_need.Add(1);
      }
    }
    // Can't really do anything in case an error is encountered while looking up the page. Simply
    // ignore it and move on to the next page. Hints are best effort anyway.
    start_offset += PAGE_SIZE;
  }
}

zx_status_t VmCowPages::ProtectRangeFromReclamationLocked(VmCowRange range, bool set_always_need,
                                                          bool ignore_errors,
                                                          Guard<CriticalMutex>* guard) {
  canary_.Assert();

  // Hints only apply to pager backed VMOs.
  if (!can_root_source_evict_locked()) {
    return ZX_OK;
  }
  // Zero lengths have no work to do.
  if (range.is_empty()) {
    return ZX_OK;
  }

  uint64_t cur_offset = ROUNDDOWN(range.offset, PAGE_SIZE);
  uint64_t end_offset = ROUNDUP(range.end(), PAGE_SIZE);

  __UNINITIALIZED MultiPageRequest page_request;
  __UNINITIALIZED zx::result<VmCowPages::LookupCursor> cursor =
      GetLookupCursorLocked(VmCowRange(cur_offset, end_offset - cur_offset));
  // Track the validity of the cursor as we would like to efficiently look up runs where possible,
  // but due to both errors and lock drops will need to acquire new cursors on occasion.
  bool cursor_valid = true;
  for (; cur_offset < end_offset; cur_offset += PAGE_SIZE) {
    const uint64_t remaining = end_offset - cur_offset;
    if (!cursor_valid) {
      cursor = GetLookupCursorLocked(VmCowRange(cur_offset, remaining));
      if (cursor.is_error()) {
        return cursor.status_value();
      }
      cursor_valid = true;
    }
    AssertHeld(cursor->lock_ref());
    // Lookup the page, this will fault in the page from the parent if neccessary, but will not
    // allocate pages directly in this if it is a child.
    auto result =
        cursor->RequirePage(false, static_cast<uint>(remaining / PAGE_SIZE), &page_request);
    zx_status_t status = result.status_value();
    if (status == ZX_OK) {
      // If we reached here, we successfully found a page at the current offset.
      vm_page_t* page = result->page;

      // The root might have gone away when the lock was dropped while waiting above. Compute the
      // root again and check if we still have a page source backing it before applying the hint.
      if (!can_root_source_evict_locked()) {
        // Hinting is not applicable anymore. No more pages to hint.
        return ZX_OK;
      }

      // Check to see if the page is owned by the root VMO. Hints only apply to the root.
      VmCowPages* owner = reinterpret_cast<VmCowPages*>(page->object.get_object());
      if (owner != GetRootLocked()) {
        // Hinting is not applicable to this page, but it might apply to following ones.
        continue;
      }

      // If the page is loaned, replace it with a non-loaned page. Loaned pages are reclaimed by
      // eviction, and hinted pages should not be evicted.
      if (page->is_loaned()) {
        DEBUG_ASSERT(is_page_clean(page));
        AssertHeld(owner->lock_ref());
        status =
            owner->ReplacePageLocked(page, page->object.get_page_offset(),
                                     /*with_loaned=*/false, &page, page_request.GetAnonymous());
        // Let the status fall through below to have success, waiting and errors handled.
      }

      if (status == ZX_OK) {
        DEBUG_ASSERT(!page->is_loaned());
        if (set_always_need) {
          page->object.always_need = 1;
          vm_vmo_always_need.Add(1);
          // Nothing more to do beyond marking the page always_need true. The lookup must have
          // already marked the page accessed, moving it to the head of the first page queue.
        }
        continue;
      }
    }
    // There was either an error in the original require page, or in processing what was looked up.
    // Either way when go back around in the loop we are going to need a new cursor.
    cursor_valid = false;

    if (status == ZX_ERR_SHOULD_WAIT) {
      guard->CallUnlocked([&status, &page_request]() { status = page_request.Wait(); });

      // The size might have changed since we dropped the lock. Adjust the range if required.
      if (cur_offset >= size_locked()) {
        // No more pages to hint.
        return ZX_OK;
      }
      // Shrink the range if required. Proceed with hinting on the remaining pages in the range;
      // we've already hinted on the preceding pages, so just go on ahead instead of returning an
      // error. The range was valid at the time we started hinting.
      if (end_offset > size_locked()) {
        end_offset = size_locked();
      }

      // If the wait succeeded, cur_offset will now have a backing page, so we need to try the
      // same offset again. Move back a page so the loop increment keeps us at the same offset. In
      // case of failure, simply continue on to the next page, as hints are best effort only.
      if (status == ZX_OK) {
        cur_offset -= PAGE_SIZE;
        continue;
      }
    }
    // Should only get here if an error was encountered, check if we should ignore or return it.
    DEBUG_ASSERT(status != ZX_OK);
    if (!ignore_errors) {
      return status;
    }
  }
  return ZX_OK;
}

zx_status_t VmCowPages::DecompressInRangeLocked(VmCowRange range, Guard<CriticalMutex>* guard) {
  canary_.Assert();

  if (range.is_empty()) {
    return ZX_OK;
  }

  DEBUG_ASSERT(range.IsBoundedBy(size_));
  uint64_t cur_offset = ROUNDDOWN(range.offset, PAGE_SIZE);
  uint64_t end_offset = ROUNDUP(range.end(), PAGE_SIZE);

  zx_status_t status;
  do {
    __UNINITIALIZED AnonymousPageRequest page_request;
    status = ForEveryOwnedMutableHierarchyPageInRangeLocked(
        [&cur_offset, &page_request](VmPageOrMarker* p, VmCowPages* owner, uint64_t this_offset,
                                     uint64_t owner_offset) {
          if (!p->IsReference()) {
            return ZX_ERR_NEXT;
          }
          AssertHeld(owner->lock_ref());
          zx_status_t status = owner->ReplaceReferenceWithPageLocked(VmPageOrMarkerRef(p),
                                                                     owner_offset, &page_request);
          if (status == ZX_OK) {
            cur_offset = this_offset + PAGE_SIZE;
            return ZX_ERR_NEXT;
          }
          return status;
        },
        cur_offset, end_offset - cur_offset);
    if (status == ZX_OK) {
      return ZX_OK;
    }
    if (status == ZX_ERR_SHOULD_WAIT) {
      guard->CallUnlocked([&page_request, &status]() { status = page_request.Wait(); });
    }
  } while (status == ZX_OK);
  return status;
}

int64_t VmCowPages::ChangeSingleHighPriorityCountLocked(int64_t delta) {
  const bool was_zero = high_priority_count_ == 0;
  high_priority_count_ += delta;
  DEBUG_ASSERT(high_priority_count_ >= 0);
  const bool is_zero = high_priority_count_ == 0;
  // Any change to or from zero means we need to add or remove a count from our parent (if we have
  // one) and potentially move pages in the page queues.
  if (is_zero && !was_zero) {
    delta = -1;
  } else if (was_zero && !is_zero) {
    delta = 1;
  } else {
    delta = 0;
  }
  if (delta != 0) {
    // If we moved to or from zero then update every page into the correct page queue for tracking.
    // MoveToNotPinnedLocked will check the high_priority_count_, which has already been updated, so
    // can just call that on every page.
    page_list_.ForEveryPage([this](const VmPageOrMarker* page_or_marker, uint64_t offset) {
      if (page_or_marker->IsPage()) {
        vm_page_t* page = page_or_marker->Page();
        if (page->object.pin_count == 0) {
          AssertHeld(lock_ref());
          MoveToNotPinnedLocked(page, offset);
        }
      }
      return ZX_ERR_NEXT;
    });
  }
  vm_vmo_high_priority.Add(delta);
  return delta;
}

void VmCowPages::ChangeHighPriorityCountLocked(int64_t delta) {
  canary_.Assert();

  VmCowPages* cur = this;
  AssertHeld(cur->lock_ref());
  // Any change to or from zero requires updating a count in the parent, so we need to walk up the
  // parent chain as long as a transition is happening.
  while (cur && delta != 0) {
    delta = cur->ChangeSingleHighPriorityCountLocked(delta);
    cur = cur->parent_.get();
  }
}

void VmCowPages::UnpinLocked(VmCowRange range) {
  canary_.Assert();

  // verify that the range is within the object
  ASSERT(range.IsBoundedBy(size_));
  // forbid zero length unpins as zero length pins return errors.
  ASSERT(!range.is_empty());

  if (is_slice_locked()) {
    return slice_parent_locked().UnpinLocked(range.OffsetBy(parent_offset_));
  }

  const uint64_t start_page_offset = ROUNDDOWN(range.offset, PAGE_SIZE);
  const uint64_t end_page_offset = ROUNDUP(range.end(), PAGE_SIZE);

#if (DEBUG_ASSERT_IMPLEMENTED)
  // For any pages that have their pin count transition to 0, i.e. become unpinned, we want to
  // perform a range change op. For efficiency track contiguous ranges.
  uint64_t completely_unpin_start = 0;
  uint64_t completely_unpin_len = 0;
#endif

  uint64_t unpin_count = 0;
  zx_status_t status = page_list_.ForEveryPageAndGapInRange(
      [&](const auto* page, uint64_t off) {
        AssertHeld(lock_ref());
        // Only real pages can be pinned.
        ASSERT(page->IsPage());

        vm_page_t* p = page->Page();
        ASSERT(p->object.pin_count > 0);
        p->object.pin_count--;
        if (p->object.pin_count == 0) {
          MoveToNotPinnedLocked(p, range.offset);
#if (DEBUG_ASSERT_IMPLEMENTED)
          // Check if the current range can be extended.
          if (completely_unpin_start + completely_unpin_len == off) {
            completely_unpin_len += PAGE_SIZE;
          } else {
            // Complete any existing range and then start again at this offset.
            if (completely_unpin_len > 0) {
              RangeChangeUpdateLocked(VmCowRange(completely_unpin_start, completely_unpin_len),
                                      RangeChangeOp::DebugUnpin);
            }
            completely_unpin_start = off;
            completely_unpin_len = PAGE_SIZE;
          }
#endif
        }
        ++unpin_count;
        return ZX_ERR_NEXT;
      },
      [](uint64_t gap_start, uint64_t gap_end) { return ZX_ERR_NOT_FOUND; }, start_page_offset,
      end_page_offset);
  ASSERT_MSG(status == ZX_OK, "Tried to unpin an uncommitted page");

  // Possible that we were entirely inside a spare interval without any committed pages, in which
  // case neither the page nor gap callback would have triggered, and the assert above would
  // succeed. This is still an error though and can catch this, and any other mistakes, by ensuring
  // we found and decremented the pin counts from the exact expected number of pages.
  ASSERT(unpin_count == (end_page_offset - start_page_offset) / PAGE_SIZE);

#if (DEBUG_ASSERT_IMPLEMENTED)
  // Check any leftover range.
  if (completely_unpin_len > 0) {
    RangeChangeUpdateLocked(VmCowRange(completely_unpin_start, completely_unpin_len),
                            RangeChangeOp::DebugUnpin);
  }
#endif

  bool overflow = sub_overflow(pinned_page_count_, unpin_count, &pinned_page_count_);
  ASSERT(!overflow);

  return;
}

bool VmCowPages::DebugIsRangePinnedLocked(VmCowRange range) {
  canary_.Assert();
  DEBUG_ASSERT(range.is_page_aligned());

  uint64_t pinned_count = 0;
  page_list_.ForEveryPageInRange(
      [&pinned_count](const auto* p, uint64_t off) {
        if (p->IsPage() && p->Page()->object.pin_count > 0) {
          pinned_count++;
          return ZX_ERR_NEXT;
        }
        return ZX_ERR_STOP;
      },
      range.offset, range.end());
  return pinned_count == range.len / PAGE_SIZE;
}

bool VmCowPages::AnyPagesPinnedLocked(uint64_t offset, size_t len) {
  canary_.Assert();
  DEBUG_ASSERT(lock_ref().lock().IsHeld());
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len));

  const uint64_t start_page_offset = offset;
  const uint64_t end_page_offset = offset + len;

  if (pinned_page_count_ == 0) {
    return false;
  }

  bool found_pinned = false;
  page_list_.ForEveryPageInRange(
      [&found_pinned, start_page_offset, end_page_offset](const auto* p, uint64_t off) {
        DEBUG_ASSERT(off >= start_page_offset && off < end_page_offset);
        if (p->IsPage() && p->Page()->object.pin_count > 0) {
          found_pinned = true;
          return ZX_ERR_STOP;
        }
        return ZX_ERR_NEXT;
      },
      start_page_offset, end_page_offset);

  return found_pinned;
}

void VmCowPages::InvalidateReadRequestsLocked(uint64_t offset, uint64_t len) {
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len));
  DEBUG_ASSERT(InRange(offset, len, size_));

  DEBUG_ASSERT(page_source_);

  const uint64_t start = offset;
  const uint64_t end = offset + len;

  zx_status_t status = page_list_.ForEveryPageAndGapInRange(
      [](const auto* p, uint64_t off) { return ZX_ERR_NEXT; },
      [this](uint64_t gap_start, uint64_t gap_end) {
        page_source_->OnPagesSupplied(gap_start, gap_end - gap_start);
        return ZX_ERR_NEXT;
      },
      start, end);
  DEBUG_ASSERT(status == ZX_OK);
}

void VmCowPages::InvalidateDirtyRequestsLocked(uint64_t offset, uint64_t len) {
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len));
  DEBUG_ASSERT(InRange(offset, len, size_));

  DEBUG_ASSERT(is_source_preserving_page_content());
  DEBUG_ASSERT(page_source_->ShouldTrapDirtyTransitions());

  const uint64_t start = offset;
  const uint64_t end = offset + len;

  zx_status_t status = page_list_.ForEveryPageAndContiguousRunInRange(
      [](const VmPageOrMarker* p, uint64_t off) {
        // A marker is a clean zero page and might have an outstanding DIRTY request.
        if (p->IsMarker()) {
          return true;
        }
        // An interval is an uncommitted zero page and might have an outstanding DIRTY request
        // irrespective of dirty state.
        if (p->IsIntervalZero()) {
          return true;
        }
        // Although a reference is implied to be clean, VMO backed by a page source should never
        // have references.
        DEBUG_ASSERT(!p->IsReference());

        vm_page_t* page = p->Page();
        DEBUG_ASSERT(is_page_dirty_tracked(page));

        // A page that is not Dirty already might have an outstanding DIRTY request.
        if (!is_page_dirty(page)) {
          return true;
        }
        // Otherwise the page should already be Dirty.
        DEBUG_ASSERT(is_page_dirty(page));
        return false;
      },
      [](const VmPageOrMarker* p, uint64_t off) {
        // Nothing to update for the page as we're not actually marking it Dirty.
        return ZX_ERR_NEXT;
      },
      [this](uint64_t start, uint64_t end, bool unused) {
        // Resolve any DIRTY requests in this contiguous range.
        page_source_->OnPagesDirtied(start, end - start);
        return ZX_ERR_NEXT;
      },
      start, end);
  // We don't expect an error from the traversal.
  DEBUG_ASSERT(status == ZX_OK);

  // Now resolve DIRTY requests for any gaps. After request generation, pages could either
  // have been evicted, or zero intervals written back, leading to gaps. So it is possible for gaps
  // to have outstanding DIRTY requests.
  status = page_list_.ForEveryPageAndGapInRange(
      [](const VmPageOrMarker* p, uint64_t off) {
        // Nothing to do for pages. We already handled them above.
        return ZX_ERR_NEXT;
      },
      [this](uint64_t gap_start, uint64_t gap_end) {
        // Resolve any DIRTY requests in this gap.
        page_source_->OnPagesDirtied(gap_start, gap_end - gap_start);
        return ZX_ERR_NEXT;
      },
      start, end);
  // We don't expect an error from the traversal.
  DEBUG_ASSERT(status == ZX_OK);
}

zx_status_t VmCowPages::ResizeLocked(uint64_t s) {
  canary_.Assert();

  LTRACEF("vmcp %p, size %" PRIu64 "\n", this, s);

  // make sure everything is aligned before we get started
  DEBUG_ASSERT(IS_PAGE_ALIGNED(size_));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(s));
  DEBUG_ASSERT(!is_slice_locked());

  // We stack-own loaned pages from removal until freed.
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  // see if we're shrinking or expanding the vmo
  if (s < size_) {
    // shrinking
    const uint64_t start = s;
    const uint64_t end = size_;
    const uint64_t len = end - start;

    // bail if there are any pinned pages in the range we're trimming
    if (AnyPagesPinnedLocked(start, len)) {
      return ZX_ERR_BAD_STATE;
    }

    // unmap all of the pages in this range on all the mapping regions
    RangeChangeUpdateLocked(VmCowRange(start, len), RangeChangeOp::Unmap);

    // Resolve any outstanding page requests tracked by the page source that are now out-of-bounds.
    if (page_source_) {
      // Tell the page source that any non-resident pages that are now out-of-bounds
      // were supplied, to ensure that any reads of those pages get woken up.
      InvalidateReadRequestsLocked(start, len);

      // If DIRTY requests are supported, also tell the page source that any non-Dirty pages that
      // are now out-of-bounds were dirtied (without actually dirtying them), to ensure that any
      // threads blocked on DIRTY requests for those pages get woken up.
      if (is_source_preserving_page_content() && page_source_->ShouldTrapDirtyTransitions()) {
        InvalidateDirtyRequestsLocked(start, len);
      }
    }

    // If pager-backed and the new size falls partway in an interval, we will need to clip the
    // interval.
    if (is_source_preserving_page_content()) {
      // Check if the first populated slot we find in the now-invalid range is an interval end.
      uint64_t interval_end = UINT64_MAX;
      zx_status_t status = page_list_.ForEveryPageInRange(
          [&interval_end](const VmPageOrMarker* p, uint64_t off) {
            if (p->IsIntervalEnd()) {
              interval_end = off;
            }
            // We found the first populated slot. Stop the traversal.
            return ZX_ERR_STOP;
          },
          start, size_);
      DEBUG_ASSERT(status == ZX_OK);

      if (interval_end != UINT64_MAX) {
        status = page_list_.ClipIntervalEnd(interval_end, interval_end - start + PAGE_SIZE);
        if (status != ZX_OK) {
          DEBUG_ASSERT(status == ZX_ERR_NO_MEMORY);
          return status;
        }
      }
    }

    // We might need to free pages from an ancestor and/or this object.
    list_node_t freed_list;
    list_initialize(&freed_list);
    __UNINITIALIZED BatchPQRemove page_remover(&freed_list);

    // Clip the parent limit and release any pages, if any, in this node or the parents.
    //
    // It should never exceed this node's size, either the current size (which is `end`) or the new
    // size (which is `start`).
    DEBUG_ASSERT(parent_limit_ <= end);

    ReleaseOwnedPagesLocked(start);

    // If the tail of a parent disappears, the children shouldn't be able to see that region again,
    // even if the parent is later reenlarged. So update the children's parent limits.
    //
    // A child's parent limit will also limit that child's descendants' views into this node, so
    // this method only needs to touch the direct children.
    for (auto& child : children_list_) {
      AssertHeld(child.lock_ref());

      child.parent_limit_ = ClampedLimit(child.parent_offset_, child.parent_limit_, start);
    }

  } else if (s > size_) {
    uint64_t temp;
    // Check that this VMOs new size would not cause it to overflow if projected onto the root.
    bool overflow = add_overflow(root_parent_offset_, s, &temp);
    if (overflow) {
      return ZX_ERR_INVALID_ARGS;
    }
    // expanding
    // figure the starting and ending page offset that is affected
    const uint64_t start = size_;
    const uint64_t end = s;
    const uint64_t len = end - start;

    // inform all our children or mapping that there's new bits
    RangeChangeUpdateLocked(VmCowRange(start, len), RangeChangeOp::Unmap);

    // If pager-backed, need to insert a dirty zero interval beyond the old size.
    if (is_source_preserving_page_content()) {
      zx_status_t status =
          page_list_.AddZeroInterval(start, end, VmPageOrMarker::IntervalDirtyState::Dirty);
      if (status != ZX_OK) {
        DEBUG_ASSERT(status == ZX_ERR_NO_MEMORY);
        return status;
      }
    }
  }

  // save bytewise size
  size_ = s;

  IncrementHierarchyGenerationCountLocked();

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  return ZX_OK;
}

zx_status_t VmCowPages::LookupLocked(VmCowRange range, VmObject::LookupFunction lookup_fn) {
  canary_.Assert();
  if (unlikely(range.is_empty())) {
    return ZX_ERR_INVALID_ARGS;
  }

  // verify that the range is within the object
  if (unlikely(!range.IsBoundedBy(size_))) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (is_slice_locked()) {
    return slice_parent_locked().LookupLocked(
        range.OffsetBy(parent_offset_),
        [&lookup_fn, parent_offset = parent_offset_](uint64_t offset, paddr_t pa) {
          // Need to undo the parent_offset before forwarding to the lookup_fn, who is ignorant of
          // slices.
          return lookup_fn(offset - parent_offset, pa);
        });
  }

  const uint64_t start_page_offset = ROUNDDOWN(range.offset, PAGE_SIZE);
  const uint64_t end_page_offset = ROUNDUP(range.end(), PAGE_SIZE);

  return page_list_.ForEveryPageInRange(
      [&lookup_fn](const auto* p, uint64_t off) {
        if (!p->IsPage()) {
          // Skip non pages.
          return ZX_ERR_NEXT;
        }
        paddr_t pa = p->Page()->paddr();
        return lookup_fn(off, pa);
      },
      start_page_offset, end_page_offset);
}

zx_status_t VmCowPages::LookupReadableLocked(VmCowRange range, LookupReadableFunction lookup_fn) {
  canary_.Assert();
  if (unlikely(range.is_empty())) {
    return ZX_ERR_INVALID_ARGS;
  }

  // verify that the range is within the object
  if (unlikely(!range.IsBoundedBy(size_))) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (is_slice_locked()) {
    return slice_parent_locked().LookupReadableLocked(
        range.OffsetBy(parent_offset_),
        [&lookup_fn, parent_offset = parent_offset_](uint64_t offset, paddr_t pa) {
          // Need to undo the parent_offset before forwarding to the lookup_fn, who is ignorant of
          // slices.
          return lookup_fn(offset - parent_offset, pa);
        });
  }

  uint64_t current_page_offset = ROUNDDOWN(range.offset, PAGE_SIZE);
  const uint64_t end_page_offset = ROUNDUP(range.end(), PAGE_SIZE);

  while (current_page_offset != end_page_offset) {
    // Attempt to process any pages we have first. Skip over anything that's not a page since the
    // lookup_fn only applies to actual pages.
    zx_status_t status = page_list_.ForEveryPageInRange(
        [&lookup_fn, &current_page_offset](const VmPageOrMarker* page_or_marker, uint64_t offset) {
          // The offset can advance ahead if we encounter gaps or sparse intervals.
          if (offset != current_page_offset) {
            if (!page_or_marker->IsIntervalEnd()) {
              // There was a gap before this offset. End the traversal.
              return ZX_ERR_STOP;
            }
            // Otherwise, we can advance our cursor to the interval end.
            offset = current_page_offset;
          }
          DEBUG_ASSERT(offset == current_page_offset);
          current_page_offset = offset + PAGE_SIZE;
          if (!page_or_marker->IsPage()) {
            return ZX_ERR_NEXT;
          }
          return lookup_fn(offset, page_or_marker->Page()->paddr());
        },
        current_page_offset, end_page_offset);

    // Check if we've processed the whole range.
    if (current_page_offset == end_page_offset) {
      break;
    }

    // See if any of our parents have the content.
    VmCowPages* owner = nullptr;
    uint64_t owner_offset = 0;
    uint64_t owner_length = end_page_offset - current_page_offset;

    // We do not care about the return value, all we are interested in is the populated out
    // variables that we pass in.
    //
    // Note that page intervals are only supported in root VMOs, so if we ended the page list
    // traversal above partway into an interval, we will be able to continue the traversal over the
    // rest of the interval after this call - since we're the root, we will be the owner and the
    // owner length won't be clipped.
    FindInitialPageContentLocked(current_page_offset, &owner, &owner_offset, &owner_length)
        .current();

    // This should always get filled out.
    DEBUG_ASSERT(owner_length > 0);
    DEBUG_ASSERT(owner);

    // Iterate over any potential content.
    AssertHeld(owner->lock_ref());
    status = owner->page_list_.ForEveryPageInRange(
        [&lookup_fn, current_page_offset, owner_offset](const VmPageOrMarker* page_or_marker,
                                                        uint64_t offset) {
          if (!page_or_marker->IsPage()) {
            return ZX_ERR_NEXT;
          }
          return lookup_fn(offset - owner_offset + current_page_offset,
                           page_or_marker->Page()->paddr());
        },
        owner_offset, owner_offset + owner_length);
    if (status != ZX_OK || status != ZX_ERR_NEXT) {
      return status;
    }

    current_page_offset += owner_length;
  }
  return ZX_OK;
}

zx_status_t VmCowPages::TakePagesWithParentLocked(uint64_t offset, uint64_t len,
                                                  VmPageSpliceList* pages, uint64_t* taken_len,
                                                  MultiPageRequest* page_request) {
  DEBUG_ASSERT(parent_);

  // Set up a cursor that will help us take pages from the parent.
  const uint64_t end = offset + len;
  uint64_t position = offset;
  auto cursor = GetLookupCursorLocked(VmCowRange(offset, len));
  if (cursor.is_error()) {
    return cursor.error_value();
  }
  AssertHeld(cursor->lock_ref());

  VmCompression* compression = Pmm::Node().GetPageCompression();

  // This loop attempts to take pages from the VMO one page at a time. For each page, it:
  // 1. Allocates a zero page to replace the existing page.
  // 2. Takes ownership of the page.
  // 3. Replaces the existing page with the zero page.
  // 4. Adds the existing page to the splice list.
  // We perform this operation page-by-page to ensure that we can always make forward progress.
  // For example, if we tried to take ownership of the entire range of pages but encounter a
  // ZX_ERR_SHOULD_WAIT, we would need to drop the lock, wait on the page request, and then attempt
  // to take ownership of all of the pages again. On highly contended VMOs, this could lead to a
  // situation in which we get stuck in this loop and no forward progress is made.
  zx_status_t status = ZX_OK;
  uint64_t new_pages_len = 0;
  while (position < end) {
    // Allocate a zero page to replace the content at position.
    // TODO(https://fxbug.dev/42076904): Inserting a full zero page is inefficient. We should
    // replace this logic with something a bit more efficient; this could mean using the same logic
    // that `ZeroPages` uses and insert markers, or generalizing the concept of intervals and using
    // those instead.
    vm_page_t* p;
    status = AllocateCopyPage(vm_get_zero_page_paddr(), nullptr, page_request->GetAnonymous(), &p);
    if (status != ZX_OK) {
      break;
    }
    VmPageOrMarker zeroed_out_page = VmPageOrMarker::Page(p);
    VmPageOrMarker* zero_page_ptr = &zeroed_out_page;
    auto free_zeroed_page = fit::defer([zero_page_ptr, this] {
      // If the zeroed out page is not incorporated into this VMO, free it.
      if (!zero_page_ptr->IsEmpty()) {
        vm_page_t* p = zero_page_ptr->ReleasePage();
        AssertHeld(lock_ref());
        // The zero page is not part of any VMO at this point, so it should not be in a page queue.
        FreePageLocked(p, false);
      }
    });

    {
      // Once we have a zero page ready to go, require an owned page at the current position.
      auto result = cursor->RequireOwnedPage(true, static_cast<uint>((end - position) / PAGE_SIZE),
                                             page_request);
      if (result.is_error()) {
        status = result.error_value();
        break;
      }
    }

    // Replace the content at `position` with the zeroed out page.
    auto result = AddPageLocked(position, ktl::move(zeroed_out_page), CanOverwriteContent::NonZero,
                                /*do_range_update=*/false);
    if (result.is_error()) {
      // Absent bugs, AddPageLocked() can only return ZX_ERR_NO_MEMORY.
      DEBUG_ASSERT(result.status_value() == ZX_ERR_NO_MEMORY);
      break;
    }
    VmPageOrMarker& content = *result;
    new_pages_len += PAGE_SIZE;
    ASSERT(!content.IsInterval());

    // Before adding the content to the splice list, we need to make sure that it:
    // 1. Is not in any page queues if it is a page.
    // 2. Is not a temporary reference.
    if (content.IsPage()) {
      DEBUG_ASSERT(content.Page()->object.pin_count == 0);
      // Cannot be taking pages from a pager backed VMO, hence cannot be taking a loaned page.
      DEBUG_ASSERT(!content.Page()->is_loaned());
      pmm_page_queues()->Remove(content.Page());
    } else if (content.IsReference()) {
      // A regular reference we can move, a temporary reference we need to turn back into
      // its page so we can move it. To determine if we have a temporary reference we can
      // just attempt to move it, and if it was a temporary reference we will get a page
      // returned.
      if (auto maybe_page = MaybeDecompressReference(compression, content.Reference())) {
        // Don't insert the page in the page queues, since we're trying to remove the pages.
        VmPageOrMarker::ReferenceValue ref = content.SwapReferenceForPage(*maybe_page);
        ASSERT(compression->IsTempReference(ref));
      }
    }

    // Add the content to the splice list.
    status = pages->Append(ktl::move(content));
    if (status == ZX_ERR_NO_MEMORY) {
      break;
    }
    DEBUG_ASSERT(status == ZX_OK);
    position += PAGE_SIZE;
    *taken_len += PAGE_SIZE;
  }

  if (new_pages_len) {
    RangeChangeUpdateLocked(VmCowRange(offset, new_pages_len), RangeChangeOp::Unmap);
  }

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());

  // We need to finalize the splice page list as soon as we know that we will not be adding pages
  // to it. This is true in any case that does not return ZX_ERR_SHOULD_WAIT.
  if (status != ZX_ERR_SHOULD_WAIT) {
    pages->Finalize();
  }

  return status;
}

zx_status_t VmCowPages::TakePagesLocked(uint64_t offset, uint64_t len, VmPageSpliceList* pages,
                                        uint64_t* taken_len, MultiPageRequest* page_request) {
  canary_.Assert();

  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len));

  if (!InRange(offset, len, size_)) {
    pages->Finalize();
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (page_source_) {
    pages->Finalize();
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (AnyPagesPinnedLocked(offset, len)) {
    pages->Finalize();
    return ZX_ERR_BAD_STATE;
  }

  // If this is a child slice, propagate the operation to the parent.
  if (is_slice_locked()) {
    return slice_parent_locked().TakePagesLocked(offset + parent_offset_, len, pages, taken_len,
                                                 page_request);
  }

  // Now that all early checks are done, increment the gen count since we're going to remove pages.
  IncrementHierarchyGenerationCountLocked();

  // If this is a child of any other kind, we need to handle it specially.
  if (parent_) {
    return TakePagesWithParentLocked(offset, len, pages, taken_len, page_request);
  }

  VmCompression* compression = Pmm::Node().GetPageCompression();
  bool found_page = false;
  page_list_.ForEveryPageInRangeMutable(
      [&compression, &found_page](VmPageOrMarkerRef p, uint64_t off) {
        found_page = true;
        // Splice lists do not support page intervals.
        ASSERT(!p->IsInterval());
        if (p->IsPage()) {
          DEBUG_ASSERT(p->Page()->object.pin_count == 0);
          // Cannot be taking pages from a pager backed VMO, hence cannot be taking a loaned page.
          DEBUG_ASSERT(!p->Page()->is_loaned());
          pmm_page_queues()->Remove(p->Page());
        } else if (p->IsReference()) {
          // A regular reference we can move are permitted in the VmPageSpliceList, it is up to the
          // receiver of the pages to reject or otherwise deal with them. A temporary reference we
          // need to turn back into its page so we can move it.
          if (auto maybe_page = MaybeDecompressReference(compression, p->Reference())) {
            // Don't insert the page in the page queues, since we're trying to remove the pages,
            // just update the page list reader for TakePages below.
            VmPageOrMarker::ReferenceValue ref = p.SwapReferenceForPage(*maybe_page);
            ASSERT(compression->IsTempReference(ref));
          }
        }
        return ZX_ERR_NEXT;
      },
      offset, offset + len);

  // If we did not find any pages, we could either be entirely inside a gap or an interval. Make
  // sure we're not inside an interval; checking a single offset for membership should suffice.
  ASSERT(found_page || !page_list_.IsOffsetInZeroInterval(offset));

  // In the very likely case that the given VmPageSpliceList is empty, we can use the TakePages
  // method to efficiently move the contents into the splice list. This is likely to be the case
  // because a non-empty splice list can only ever be encountered when we are taking pages from a
  // VMO whose parent is concurrently closed. In this case, we have to append to the splice list
  // one VmPageOrMarker at a time.
  if (likely(pages->IsEmpty())) {
    *pages = page_list_.TakePages(offset, len);
  } else {
    for (uint64_t position = offset; position < offset + len; position += PAGE_SIZE) {
      VmPageOrMarker content = page_list_.RemoveContent(position);
      pages->Append(ktl::move(content));
    }
    pages->Finalize();
  }

  *taken_len = len;
  RangeChangeUpdateLocked(VmCowRange(offset, len), RangeChangeOp::Unmap);

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());

  return ZX_OK;
}

zx_status_t VmCowPages::SupplyPages(uint64_t offset, uint64_t len, VmPageSpliceList* pages,
                                    SupplyOptions options, uint64_t* supplied_len,
                                    MultiPageRequest* page_request) {
  canary_.Assert();
  Guard<CriticalMutex> guard{lock()};
  return SupplyPagesLocked(offset, len, pages, options, supplied_len, page_request);
}

zx_status_t VmCowPages::SupplyPagesLocked(uint64_t offset, uint64_t len, VmPageSpliceList* pages,
                                          SupplyOptions options, uint64_t* supplied_len,
                                          MultiPageRequest* page_request) {
  canary_.Assert();

  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(len));
  DEBUG_ASSERT(supplied_len);
  ASSERT(options != SupplyOptions::PagerSupply || page_source_);

  if (!InRange(offset, len, size_)) {
    *supplied_len = 0;
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (options == SupplyOptions::TransferData) {
    if (page_source_) {
      return ZX_ERR_NOT_SUPPORTED;
    }
    if (AnyPagesPinnedLocked(offset, len)) {
      return ZX_ERR_BAD_STATE;
    }
  }

  if (page_source_ && page_source_->is_detached()) {
    return ZX_ERR_BAD_STATE;
  }

  // If this is a child slice, propagate the operation to the parent.
  if (is_slice_locked()) {
    return slice_parent_locked().SupplyPagesLocked(offset + parent_offset_, len, pages, options,
                                                   supplied_len, page_request);
  }

  // If this VMO has a parent, we need to make sure we take ownership of all of the pages in the
  // input range.
  // TODO(https://fxbug.dev/42076904): This is suboptimal, as we take ownership of a page just to
  // free it immediately when we replace it with the supplied page.
  if (parent_) {
    const uint64_t end = offset + len;
    uint64_t position = offset;
    auto cursor = GetLookupCursorLocked(VmCowRange(offset, len));
    if (cursor.is_error()) {
      return cursor.error_value();
    }
    AssertHeld(cursor->lock_ref());
    while (position < end) {
      auto result = cursor->RequireOwnedPage(true, static_cast<uint>((end - position) / PAGE_SIZE),
                                             page_request);
      if (result.is_error()) {
        return result.error_value();
      }
      position += PAGE_SIZE;
    }
  }

  // It is possible that we fail to insert pages below and we increment the gen count needlessly,
  // but the user is certainly expecting it to succeed.
  IncrementHierarchyGenerationCountLocked();

  const uint64_t start = offset;
  const uint64_t end = offset + len;

  // We stack-own loaned pages below from allocation for page replacement to AddPageLocked().
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  list_node freed_list;
  list_initialize(&freed_list);

  // [new_pages_start, new_pages_start + new_pages_len) tracks the current run of
  // consecutive new pages added to this vmo.
  uint64_t new_pages_start = offset;
  uint64_t new_pages_len = 0;
  zx_status_t status = ZX_OK;
  [[maybe_unused]] uint64_t initial_list_position = pages->Position();
  while (!pages->IsProcessed()) {
    // With a PageSource only Pages are supported, so convert any refs to real pages.
    // We do this without popping a page from the splice list as `MakePageFromReference` may return
    // ZX_ERR_SHOULD_WAIT. This could lead the caller to wait on the page request and call
    // `SupplyPagesLocked` again, at which point it would expect the operation to continue at the
    // exact same page.
    VmPageOrMarkerRef src_page_ref = pages->PeekReference();
    // The src_page_ref can be null if the head of the page list is not a reference or if the page
    // list is empty.
    if (src_page_ref) {
      DEBUG_ASSERT(src_page_ref->IsReference());
      status = MakePageFromReference(src_page_ref, page_request->GetAnonymous());
      if (status != ZX_OK) {
        break;
      }
    }
    VmPageOrMarker src_page = pages->Pop();
    DEBUG_ASSERT(!src_page.IsReference());

    // The pager API does not allow the source VMO of supply pages to have a page source, so we can
    // assume that any empty pages are zeroes and insert explicit markers here. We need to insert
    // explicit markers to actually resolve the pager fault.
    if (src_page.IsEmpty()) {
      src_page = VmPageOrMarker::Marker();
    }

    // A newly supplied page starts off as Clean.
    if (src_page.IsPage() && is_source_preserving_page_content()) {
      UpdateDirtyStateLocked(src_page.Page(), offset, DirtyState::Clean,
                             /*is_pending_add=*/true);
    }

    // Defer individual range updates so we can do them in blocks.
    const CanOverwriteContent overwrite_policy = options == SupplyOptions::TransferData
                                                     ? CanOverwriteContent::NonZero
                                                     : CanOverwriteContent::None;
    auto page_transaction = BeginAddPageLocked(offset, overwrite_policy);
    if (page_transaction.is_error()) {
      // Unable to insert anything at this slot, cleanup any existing src_page and handle a
      // completed run.
      if (src_page.IsPageOrRef()) {
        DEBUG_ASSERT(src_page.IsPage());
        vm_page_t* page = src_page.ReleasePage();
        DEBUG_ASSERT(!list_in_list(&page->queue_node));
        list_add_tail(&freed_list, &page->queue_node);
      }

      if (likely(page_transaction.status_value() == ZX_ERR_ALREADY_EXISTS)) {
        // We hit the end of a run of absent pages, so notify the page source
        // of any new pages that were added and reset the tracking variables.
        if (new_pages_len) {
          RangeChangeUpdateLocked(VmCowRange(new_pages_start, new_pages_len), RangeChangeOp::Unmap);
          if (page_source_) {
            page_source_->OnPagesSupplied(new_pages_start, new_pages_len);
          }
        }
        new_pages_start = offset + PAGE_SIZE;
        new_pages_len = 0;
        offset += PAGE_SIZE;
        continue;
      } else {
        // Only cause for this should be an out of memory from the kernel heap when attempting to
        // allocate a page list node.
        status = page_transaction.status_value();
        ASSERT(status == ZX_ERR_NO_MEMORY);
        break;
      }
    }
    VmPageOrMarker old_page;
    if (options != SupplyOptions::PhysicalPageProvider && can_borrow_locked() &&
        src_page.IsPage() &&
        pmm_physical_page_borrowing_config()->is_borrowing_in_supplypages_enabled()) {
      // Assert some things we implicitly know are true (currently).  We can avoid explicitly
      // checking these in the if condition for now.
      DEBUG_ASSERT(!is_source_supplying_specific_physical_pages());
      DEBUG_ASSERT(!src_page.Page()->is_loaned());
      // Try to replace src_page with a loaned page.  We allocate the loaned page one page at a time
      // to avoid failing the allocation due to asking for more loaned pages than there are free
      // loaned pages.
      vm_page_t* new_page = nullptr;
      zx_status_t alloc_status = AllocLoanedPage(&new_page);
      // If we got a loaned page, replace the page in src_page, else just continue with src_page
      // unmodified since pmm has no more loaned free pages or
      // !is_borrowing_in_supplypages_enabled().
      if (alloc_status == ZX_OK) {
        CopyPageForReplacementLocked(new_page, src_page.Page());
        vm_page_t* free_page = src_page.ReleasePage();
        list_add_tail(&freed_list, &free_page->queue_node);
        old_page = CompleteAddPageLocked(*page_transaction, VmPageOrMarker::Page(new_page),
                                         /*do_range_update=*/false);
      } else {
        old_page = CompleteAddPageLocked(*page_transaction, ktl::move(src_page),
                                         /*do_range_update=*/false);
      }
    } else if (options == SupplyOptions::PhysicalPageProvider) {
      // When being called from the physical page provider, we need to call InitializeVmPage(),
      // which AddNewPageLocked() will do.
      // We only want to populate offsets that have true absence of content, so do not overwrite
      // anything in the page list.
      old_page = CompleteAddNewPageLocked(*page_transaction, src_page.Page(),
                                          /*zero=*/false, /*do_range_update=*/false);
      // The page was successfully added, but we still have a copy in the src_page, so we need to
      // release it, however need to store the result in a temporary as we are required to use the
      // result of ReleasePage.
      [[maybe_unused]] vm_page_t* unused = src_page.ReleasePage();
    } else {
      // When not being called from the physical page provider, we don't need InitializeVmPage(),
      // so we use AddPageLocked().
      // We only want to populate offsets that have true absence of content, so do not overwrite
      // anything in the page list.
      old_page = CompleteAddPageLocked(*page_transaction, ktl::move(src_page),
                                       /*do_range_update=*/false);
    }
    // If the content overwrite policy was None, the old page should be empty.
    DEBUG_ASSERT(overwrite_policy != CanOverwriteContent::None || old_page.IsEmpty());
    // Clean up the old_page if necessary. The action taken is different depending on the state of
    // old_page:
    // 1. Page: If old_page is backed by an actual page, remove it from the page queues and free
    //          the page.
    // 2. Reference: If old_page is a reference, free the reference.
    // 3. Interval: We should not be overwriting data in a pager-backed VMO, so assert that
    //              old_page is not an interval.
    // 4. Marker: There are no resources to free here, so do nothing.
    if (old_page.IsPage()) {
      vm_page_t* released_page = old_page.ReleasePage();
      // We do not overwrite content in pager backed VMOs, the only place where loaned pages can be,
      // so any old page must never have been loaned.
      DEBUG_ASSERT(!released_page->is_loaned());
      pmm_page_queues()->Remove(released_page);
      DEBUG_ASSERT(!list_in_list(&released_page->queue_node));
      list_add_tail(&freed_list, &released_page->queue_node);
    } else if (old_page.IsReference()) {
      FreeReference(old_page.ReleaseReference());
    } else {
      DEBUG_ASSERT(!old_page.IsInterval());
    }
    new_pages_len += PAGE_SIZE;
    DEBUG_ASSERT(new_pages_start + new_pages_len <= end);

    offset += PAGE_SIZE;
  }
  // Unless there was an error and we exited the loop early, then there should have been the correct
  // number of pages in the splice list.
  DEBUG_ASSERT(offset == end || status != ZX_OK);
  if (new_pages_len) {
    RangeChangeUpdateLocked(VmCowRange(new_pages_start, new_pages_len), RangeChangeOp::Unmap);
    if (page_source_) {
      page_source_->OnPagesSupplied(new_pages_start, new_pages_len);
    }
  }

  if (!list_is_empty(&freed_list)) {
    // Even though we did not insert these pages successfully, we had logical ownership of them.
    FreePagesLocked(&freed_list, /*freeing_owned_pages=*/true);
  }

  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());

  *supplied_len = offset - start;
  // In the case of ZX_OK or ZX_ERR_SHOULD_WAIT we should have supplied exactly as many pages as we
  // processed. In any other case the value is undefined.
  DEBUG_ASSERT(((pages->Position() - initial_list_position) == *supplied_len) ||
               (status != ZX_OK && status != ZX_ERR_SHOULD_WAIT));
  return status;
}

// This is a transient operation used only to fail currently outstanding page requests. It does not
// alter the state of the VMO, or any pages that might have already been populated within the
// specified range.
//
// If certain pages in this range are populated, we must have done so via a previous SupplyPages()
// call that succeeded. So it might be fine for clients to continue accessing them, despite the
// larger range having failed.
//
// TODO(rashaeqbal): If we support a more permanent failure mode in the future, we will need to free
// populated pages in the specified range, and possibly detach the VMO from the page source.
zx_status_t VmCowPages::FailPageRequestsLocked(VmCowRange range, zx_status_t error_status) {
  canary_.Assert();

  DEBUG_ASSERT(range.is_page_aligned());

  ASSERT(page_source_);

  if (!PageSource::IsValidInternalFailureCode(error_status)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!range.IsBoundedBy(size_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (page_source_->is_detached()) {
    return ZX_ERR_BAD_STATE;
  }

  page_source_->OnPagesFailed(range.offset, range.len, error_status);
  return ZX_OK;
}

zx_status_t VmCowPages::DirtyPagesLocked(VmCowRange range, list_node_t* alloc_list,
                                         AnonymousPageRequest* page_request) {
  canary_.Assert();

  DEBUG_ASSERT(range.is_page_aligned());

  ASSERT(page_source_);

  if (!page_source_->ShouldTrapDirtyTransitions()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  DEBUG_ASSERT(is_source_preserving_page_content());

  const uint64_t start_offset = range.offset;
  const uint64_t end_offset = range.end();

  if (start_offset > size_locked()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // Overflow check.
  if (end_offset < start_offset) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // After the above checks, the page source has tried to respond correctly to a range of dirty
  // requests, so the kernel should resolve those outstanding dirty requests, even in the failure
  // case. From a returned error, the page source currently has no ability to detect which ranges
  // caused the error, so the kernel should either completely succeed or fail the request instead of
  // holding onto a partial outstanding request that will block pager progress.
  auto invalidate_requests_on_error = fit::defer([this, len = range.len, start_offset] {
    AssertHeld(lock_ref());
    DEBUG_ASSERT(size_locked() >= start_offset);

    uint64_t invalidate_len = ktl::min(size_locked() - start_offset, len);
    InvalidateDirtyRequestsLocked(start_offset, invalidate_len);
  });

  // The page source may have tried to mark a larger range than necessary as dirty. Invalidate the
  // requests and return an error.
  if (end_offset > size_locked()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (page_source_->is_detached()) {
    return ZX_ERR_BAD_STATE;
  }

  // If any of the pages in the range are zero page markers (Clean zero pages), they need to be
  // forked in order to be dirtied (written to). Find the number of such pages that need to be
  // allocated. We also need to allocate zero pages to replace sparse zero intervals.
  size_t zero_pages_count = 0;
  // This tracks the beginning of an interval that falls in the specified range. Since we might
  // start partway inside an interval, this is initialized to start_offset so that we only consider
  // the portion of the interval inside the range. If we did not start inside an interval, we will
  // end up reinitializing this when we do find an interval start, before this value is used, so it
  // is safe to initialize to start_offset in all cases.
  uint64_t interval_start = start_offset;
  // This tracks whether we saw an interval start sentinel in the traversal, but have not yet
  // encountered a matching interval end sentinel. Should we end the traversal partway in an
  // interval, we will need to handle the portion of the interval between the interval start and the
  // end of the specified range.
  bool unmatched_interval_start = false;
  bool found_page_or_gap = false;
  zx_status_t status = page_list_.ForEveryPageAndGapInRange(
      [&zero_pages_count, &interval_start, &unmatched_interval_start, &found_page_or_gap](
          const VmPageOrMarker* p, uint64_t off) {
        found_page_or_gap = true;
        if (p->IsMarker()) {
          zero_pages_count++;
          return ZX_ERR_NEXT;
        }
        if (p->IsIntervalZero()) {
          if (p->IsIntervalStart()) {
            interval_start = off;
            unmatched_interval_start = true;
          } else if (p->IsIntervalEnd()) {
            zero_pages_count += (off - interval_start + PAGE_SIZE) / PAGE_SIZE;
            unmatched_interval_start = false;
          } else {
            DEBUG_ASSERT(p->IsIntervalSlot());
            zero_pages_count++;
          }
          return ZX_ERR_NEXT;
        }
        // Pager-backed VMOs cannot have compressed references, so the only other type is a page.
        DEBUG_ASSERT(p->IsPage());
        return ZX_ERR_NEXT;
      },
      [&found_page_or_gap](uint64_t start, uint64_t end) {
        found_page_or_gap = true;
        // A gap indicates a page that has not been supplied yet. It will need to be supplied
        // first. Although we will never generate a DIRTY request for absent pages in the first
        // place, it is still possible for a clean page to get evicted after the DIRTY request was
        // generated. It is also possible for a dirty zero interval to have been written back such
        // that we have an old DIRTY request for the interval.
        //
        // Spuriously resolve the DIRTY page request, and let the waiter(s) retry looking up the
        // page, which will generate a READ request first to supply the missing page.
        return ZX_ERR_NOT_FOUND;
      },
      start_offset, end_offset);

  if (status != ZX_OK) {
    return status;
  }

  // Handle the last interval or if we did not enter the traversal callbacks at all.
  if (unmatched_interval_start || !found_page_or_gap) {
    DEBUG_ASSERT(found_page_or_gap || interval_start == start_offset);
    zero_pages_count += (end_offset - interval_start) / PAGE_SIZE;
  }

  // If we have found any zero pages to populate, then we need to allocate and transition them to
  // the dirty state.
  if (zero_pages_count > 0) {
    // Allocate the number of zero pages required upfront, so that we can fail the call early if the
    // page allocation fails. First determine how many pages we still need to allocate, based on the
    // number of existing pages in the list.
    uint64_t alloc_list_len = list_length(alloc_list);
    zero_pages_count = zero_pages_count > alloc_list_len ? zero_pages_count - alloc_list_len : 0;

    // First try to allocate all the pages at once. This is an optimization and avoids repeated
    // calls to the PMM to allocate single pages. If the PMM returns ZX_ERR_SHOULD_WAIT, fall back
    // to allocating one page at a time below, giving reclamation strategies a better chance to
    // catch up with incoming allocation requests.
    status = pmm_alloc_pages(zero_pages_count, pmm_alloc_flags_, alloc_list);
    if (status == ZX_OK) {
      // All requested pages allocated.
      zero_pages_count = 0;
    } else {
      if (status != ZX_ERR_SHOULD_WAIT) {
        return status;
      }

      // Fall back to allocating a single page at a time. We want to do this before we can start
      // inserting pages into the page list, to avoid rolling back any pages we inserted but could
      // not dirty in case we fail partway after having inserted some pages into the page list.
      // Rolling back like this can lead to a livelock where we are constantly allocating some
      // pages, freeing them, waiting on the page_request, and then repeating.
      //
      // If allocations do fail partway here, we will have accumulated the allocated pages in
      // alloc_list, so we will be able to reuse them on a subsequent call to DirtyPagesLocked. This
      // ensures we are making forward progress across successive calls.
      while (zero_pages_count > 0) {
        vm_page_t* new_page;
        // We will initialize this page later when passing it to AddNewPageLocked
        status = AllocUninitializedPage(&new_page, page_request);
        // If single page allocation fails, bubble up the failure.
        if (status != ZX_OK) {
          // If propagating up ZX_ERR_SHOULD_WAIT do not consider this an error that requires
          // invalidating the dirty request as we are going to retry it.
          if (status == ZX_ERR_SHOULD_WAIT) {
            invalidate_requests_on_error.cancel();
          }
          return status;
        }
        list_add_tail(alloc_list, &new_page->queue_node);
        zero_pages_count--;
      }
    }
    DEBUG_ASSERT(zero_pages_count == 0);

    // We have to mark all the requested pages Dirty *atomically*. The user pager might be tracking
    // filesystem space reservations based on the success / failure of this call. So if we fail
    // partway, the user pager might think that no pages in the specified range have been dirtied,
    // which would be incorrect. If there are any conditions that would cause us to fail, evaluate
    // those before actually adding the pages, so that we can return the failure early before
    // starting to mark pages Dirty.
    //
    // Install page slots for all the intervals we'll be adding zero pages in. Page insertion will
    // only proceed once we've allocated all the slots without any errors.
    // Populating slots will alter the page list. So break out of the traversal upon finding an
    // interval, populate slots in it, and then resume the traversal after the interval.
    uint64_t next_start_offset = start_offset;
    do {
      struct {
        bool found_interval;
        uint64_t start;
        uint64_t end;
      } state = {.found_interval = false, .start = 0, .end = 0};
      status = page_list_.ForEveryPageAndContiguousRunInRange(
          [](const VmPageOrMarker* p, uint64_t off) {
            return p->IsIntervalStart() || p->IsIntervalEnd();
          },
          [](const VmPageOrMarker* p, uint64_t off) {
            DEBUG_ASSERT(p->IsIntervalZero());
            return ZX_ERR_NEXT;
          },
          [&state](uint64_t start, uint64_t end, bool is_interval) {
            DEBUG_ASSERT(is_interval);
            state = {.found_interval = true, .start = start, .end = end};
            return ZX_ERR_STOP;
          },
          next_start_offset, end_offset);
      DEBUG_ASSERT(status == ZX_OK);

      // No intervals remain.
      if (!state.found_interval) {
        break;
      }
      // Ensure we're making forward progress.
      DEBUG_ASSERT(state.end - state.start >= PAGE_SIZE);
      zx_status_t st = page_list_.PopulateSlotsInInterval(state.start, state.end);
      if (st != ZX_OK) {
        DEBUG_ASSERT(st == ZX_ERR_NO_MEMORY);
        // Before returning, we need to undo any slots we might have populated in intervals we
        // previously encountered. This is a rare error case and can be inefficient.
        for (uint64_t off = start_offset; off < state.start; off += PAGE_SIZE) {
          auto slot = page_list_.Lookup(off);
          if (slot) {
            // If this is an interval slot, return it. Note that even though we did populate all
            // slots until this point, not all will remain slots in this for-loop. When returning
            // slots, they can merge with intervals both before and after, so it's possible that the
            // next slot we were expecting has already been consumed.
            if (slot->IsIntervalSlot()) {
              page_list_.ReturnIntervalSlot(off);
            }
          }
        }
        return st;
      }
      next_start_offset = state.end;
    } while (next_start_offset < end_offset);

    // All operations from this point on must succeed so we can atomically mark pages dirty.

    // Increment the generation count as we're going to be inserting new pages.
    IncrementHierarchyGenerationCountLocked();

    // Install newly allocated pages in place of the zero page markers and interval sentinels. Start
    // with clean zero pages even for the intervals, so that the dirty transition logic below can
    // uniformly transition them to dirty along with pager supplied pages.
    status = page_list_.ForEveryPageInRange(
        [this, &alloc_list](const VmPageOrMarker* p, uint64_t off) {
          if (p->IsMarker() || p->IsIntervalSlot()) {
            DEBUG_ASSERT(!list_is_empty(alloc_list));
            AssertHeld(lock_ref());

            // AddNewPageLocked will also zero the page and update any mappings.
            //
            // TODO(rashaeqbal): Depending on how often we end up forking zero markers, we might
            // want to pass do_range_udpate = false, and defer updates until later, so we can
            // perform a single batch update.
            zx_status_t status =
                AddNewPageLocked(off, list_remove_head_type(alloc_list, vm_page, queue_node),
                                 CanOverwriteContent::Zero, nullptr);
            // AddNewPageLocked will not fail with ZX_ERR_ALREADY_EXISTS as we can overwrite
            // markers and interval slots since they are zero, nor with ZX_ERR_NO_MEMORY as we don't
            // need to allocate a new slot in the page list, we're simply replacing its content.
            ASSERT(status == ZX_OK);
          }
          return ZX_ERR_NEXT;
        },
        start_offset, end_offset);

    // We don't expect an error from the traversal.
    DEBUG_ASSERT(status == ZX_OK);
  }

  status = page_list_.ForEveryPageAndContiguousRunInRange(
      [](const VmPageOrMarker* p, uint64_t off) {
        DEBUG_ASSERT(!p->IsReference());
        if (p->IsPage()) {
          vm_page_t* page = p->Page();
          DEBUG_ASSERT(is_page_dirty_tracked(page));
          DEBUG_ASSERT(is_page_clean(page) || !page->is_loaned());
          return !is_page_dirty(page);
        }
        return false;
      },
      [this](const VmPageOrMarker* p, uint64_t off) {
        DEBUG_ASSERT(p->IsPage());
        vm_page_t* page = p->Page();
        DEBUG_ASSERT(is_page_dirty_tracked(page));
        DEBUG_ASSERT(!is_page_dirty(page));
        AssertHeld(lock_ref());
        UpdateDirtyStateLocked(page, off, DirtyState::Dirty);
        return ZX_ERR_NEXT;
      },
      [this](uint64_t start, uint64_t end, bool unused) {
        page_source_->OnPagesDirtied(start, end - start);
        return ZX_ERR_NEXT;
      },
      start_offset, end_offset);
  // We don't expect a failure from the traversal.
  DEBUG_ASSERT(status == ZX_OK);

  // All pages have been dirtied successfully, so cancel the cleanup on error.
  invalidate_requests_on_error.cancel();

  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  return status;
}

zx_status_t VmCowPages::EnumerateDirtyRangesLocked(VmCowRange range,
                                                   DirtyRangeEnumerateFunction&& dirty_range_fn) {
  canary_.Assert();

  // Dirty pages are only tracked if the page source preserves content.
  if (!is_source_preserving_page_content()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!range.IsBoundedBy(size_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  const uint64_t start_offset = ROUNDDOWN(range.offset, PAGE_SIZE);
  const uint64_t end_offset = ROUNDUP(range.end(), PAGE_SIZE);

  zx_status_t status = page_list_.ForEveryPageAndContiguousRunInRange(
      [](const VmPageOrMarker* p, uint64_t off) {
        // Enumerate both AwaitingClean and Dirty pages, i.e. anything that is not Clean.
        // AwaitingClean pages are "dirty" too for the purposes of this enumeration, since their
        // modified contents are still in the process of being written back.
        if (p->IsPage()) {
          vm_page_t* page = p->Page();
          DEBUG_ASSERT(is_page_dirty_tracked(page));
          DEBUG_ASSERT(is_page_clean(page) || !page->is_loaned());
          return !is_page_clean(page);
        }
        // Enumerate any dirty zero intervals.
        if (p->IsIntervalZero()) {
          // For now we do not support clean intervals.
          DEBUG_ASSERT(!p->IsZeroIntervalClean());
          return p->IsZeroIntervalDirty();
        }
        // Pager-backed VMOs cannot have compressed references, so the only other type is a marker.
        DEBUG_ASSERT(p->IsMarker());
        return false;
      },
      [](const VmPageOrMarker* p, uint64_t off) {
        if (p->IsPage()) {
          vm_page_t* page = p->Page();
          DEBUG_ASSERT(is_page_dirty_tracked(page));
          DEBUG_ASSERT(!is_page_clean(page));
          DEBUG_ASSERT(!page->is_loaned());
          DEBUG_ASSERT(page->object.get_page_offset() == off);
        } else if (p->IsIntervalZero()) {
          DEBUG_ASSERT(p->IsZeroIntervalDirty());
        }
        return ZX_ERR_NEXT;
      },
      [&dirty_range_fn](uint64_t start, uint64_t end, bool is_interval) {
        // Zero intervals are enumerated as zero ranges.
        return dirty_range_fn(start, end - start, /*range_is_zero=*/is_interval);
      },
      start_offset, end_offset);

  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  return status;
}

zx_status_t VmCowPages::WritebackBeginLocked(VmCowRange range, bool is_zero_range) {
  canary_.Assert();

  DEBUG_ASSERT(range.is_page_aligned());

  ASSERT(page_source_);

  if (!range.IsBoundedBy(size_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!is_source_preserving_page_content()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const uint64_t start_offset = range.offset;
  const uint64_t end_offset = range.end();
  // We only need to consider transitioning committed pages if the caller has specified that this is
  // not a zero range. For a zero range, we cannot start cleaning any pages because the caller has
  // expressed intent to write back zeros in this range; any pages we clean might get evicted and
  // incorrectly supplied again as zero pages, leading to data loss.
  //
  // When querying dirty ranges, zero page intervals are indicated as dirty zero ranges. So it's
  // perfectly reasonable for the user pager to write back these zero ranges efficiently without
  // having to read the actual contents of the range, which would read zeroes anyway. There can
  // exist a race however, where the user pager has just discovered a dirty zero range, and before
  // it starts writing it out, an actual page gets dirtied in that range. Consider the following
  // example that demonstrates the race:
  //  1. The zero interval [5, 10) is indicated as a dirty zero range when the user pager queries
  //  dirty ranges.
  //  2. A write comes in for page 7 and it is marked Dirty. The interval is split up into two: [5,
  //  7) and [8, 10).
  //  3. The user pager prepares to write the range [5, 10) with WritebackBegin.
  //  4. Both the intervals as well as page 7 are marked AwaitingClean.
  //  5. The user pager still thinks that [5, 10) is zero and writes back zeroes for the range.
  //  6. The user pager does a WritebackEnd on [5, 10), and page 7 gets marked Clean.
  //  7. At some point in the future, page 7 gets evicted. The data on page 7 (which was prematurely
  //  marked Clean) is now lost.
  //
  // This race occurred because there was a mismatch between what the user pager and the kernel
  // think the contents of the range being written back are. The user pager intended to mark only
  // zero ranges clean, not actual pages. The is_zero_range flag captures this intent, so that the
  // kernel does not incorrectly clean actual committed pages. Committed dirty pages will be
  // returned as actual dirty pages (not dirty zero ranges) on a subsequent call to query dirty
  // ranges, and can be cleaned then.

  auto interval_start = VmPageOrMarkerRef(nullptr);
  uint64_t interval_start_off;
  zx_status_t status = page_list_.ForEveryPageInRangeMutable(
      [is_zero_range, &interval_start, &interval_start_off, this](VmPageOrMarkerRef p,
                                                                  uint64_t off) {
        // VMOs with a page source should never have references.
        DEBUG_ASSERT(!p->IsReference());
        // If the page is pinned we have to leave it Dirty in case it is still being written to
        // via DMA. The VM system will be unaware of these writes, and so we choose to be
        // conservative here and might end up with pinned pages being left dirty for longer, until
        // a writeback is attempted after the unpin.
        // If the caller indicates that they're only cleaning zero pages, any committed pages need
        // to be left dirty.
        if (p->IsPage() && (p->Page()->object.pin_count > 0 || is_zero_range)) {
          return ZX_ERR_NEXT;
        }
        // Transition pages from Dirty to AwaitingClean.
        if (p->IsPage() && is_page_dirty(p->Page())) {
          AssertHeld(lock_ref());
          UpdateDirtyStateLocked(p->Page(), off, DirtyState::AwaitingClean);
          return ZX_ERR_NEXT;
        }
        // Transition dirty zero intervals to AwaitingClean.
        if (p->IsIntervalZero()) {
          if (!p->IsZeroIntervalDirty()) {
            // The only other state we support is Untracked.
            DEBUG_ASSERT(p->IsZeroIntervalUntracked());
            return ZX_ERR_NEXT;
          }
          if (p->IsIntervalStart() || p->IsIntervalSlot()) {
            // Start tracking a dirty interval. It will only transition once the end is encountered.
            DEBUG_ASSERT(!interval_start);
            interval_start = p;
            interval_start_off = off;
          }
          if (p->IsIntervalEnd() || p->IsIntervalSlot()) {
            // Now that we've encountered the end, the entire interval can be transitioned to
            // AwaitingClean. This is done by setting the AwaitingCleanLength of the start sentinel.
            // TODO: If the writeback began partway into the interval, try to coalesce the start's
            // awaiting clean length with the range being cleaned here if it immediately follows.
            if (interval_start) {
              // Set the new AwaitingClean length to the max of the old value and the new one.
              // See comments in WritebackEndLocked for an explanation.
              const uint64_t old_len = interval_start->GetZeroIntervalAwaitingCleanLength();
              interval_start.SetZeroIntervalAwaitingCleanLength(
                  ktl::max(off - interval_start_off + PAGE_SIZE, old_len));
            }
            // Reset the interval start so we can track a new one later.
            interval_start = VmPageOrMarkerRef(nullptr);
          }
          return ZX_ERR_NEXT;
        }
        // This was either a marker (which is already clean), or a non-Dirty page.
        DEBUG_ASSERT(p->IsMarker() || !is_page_dirty(p->Page()));
        return ZX_ERR_NEXT;
      },
      start_offset, end_offset);
  // We don't expect a failure from the traversal.
  DEBUG_ASSERT(status == ZX_OK);

  // Process the last partial interval.
  if (interval_start) {
    DEBUG_ASSERT(interval_start->IsIntervalStart());
    const uint64_t old_len = interval_start->GetZeroIntervalAwaitingCleanLength();
    interval_start.SetZeroIntervalAwaitingCleanLength(
        ktl::max(end_offset - interval_start_off, old_len));
  }

  // Set any mappings for this range to read-only, so that a permission fault is triggered the next
  // time the page is written to in order for us to track it as dirty. This might cover more pages
  // than the Dirty pages found in the page list traversal above, but we choose to do this once for
  // the entire range instead of per page; pages in the AwaitingClean and Clean states will already
  // have their write permission removed, so this is a no-op for them.
  RangeChangeUpdateLocked(VmCowRange(start_offset, end_offset - start_offset),
                          RangeChangeOp::RemoveWrite);

  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  return ZX_OK;
}

zx_status_t VmCowPages::WritebackEndLocked(VmCowRange range) {
  canary_.Assert();

  DEBUG_ASSERT(range.is_page_aligned());

  ASSERT(page_source_);

  if (!range.IsBoundedBy(size_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!is_source_preserving_page_content()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // We might end up removing / clipping zero intervals, so update the generation count.
  IncrementHierarchyGenerationCountLocked();

  const uint64_t start_offset = range.offset;
  const uint64_t end_offset = range.end();

  // Mark any AwaitingClean pages Clean. Remove AwaitingClean intervals that can be fully cleaned,
  // otherwise clip the interval start removing the part that has been cleaned. Note that deleting
  // an interval start is delayed until the corresponding end is encountered, and to ensure safe
  // continued traversal, the start should always be released before the end, i.e. in the expected
  // forward traversal order for RemovePages.
  VmPageOrMarker* interval_start = nullptr;
  uint64_t interval_start_off;
  // This tracks the end offset until which all zero intervals can be marked clean. This is a
  // running counter that is maintained across multiple zero intervals. Each time we encounter
  // a new interval start, we take the max of the existing value and the AwaitingCleanLength of the
  // new interval. This is because when zero intervals are truncated at the end or split, their
  // AwaitingCleanLength does not get updated, even if it's larger than the current interval length.
  // This is an optimization to avoid having to potentially walk to another node to find the
  // relevant start to update. The reason it is safe to leave the AwaitingCleanLength unchanged is
  // that it should be possible to apply the AwaitingCleanLength to any new zero intervals that get
  // added later beyond the truncated interval. The user pager has indicated its intent to write a
  // range as zeros, so until the point that it actually completes the writeback, it doesn't matter
  // if zero intervals are removed and re-added, as long as they fall in the range that was
  // initially indicated as being written back as zeros.
  uint64_t interval_awaiting_clean_end = start_offset;
  page_list_.RemovePages(
      [&interval_start, &interval_start_off, &interval_awaiting_clean_end, this](VmPageOrMarker* p,
                                                                                 uint64_t off) {
        // VMOs with a page source should never have references.
        DEBUG_ASSERT(!p->IsReference());
        // Transition pages from AwaitingClean to Clean.
        if (p->IsPage() && is_page_awaiting_clean(p->Page())) {
          AssertHeld(lock_ref());
          UpdateDirtyStateLocked(p->Page(), off, DirtyState::Clean);
          return ZX_ERR_NEXT;
        }
        // Handle zero intervals.
        if (p->IsIntervalZero()) {
          if (!p->IsZeroIntervalDirty()) {
            // The only other state we support is Untracked.
            DEBUG_ASSERT(p->IsZeroIntervalUntracked());
            return ZX_ERR_NEXT;
          }
          if (p->IsIntervalStart() || p->IsIntervalSlot()) {
            DEBUG_ASSERT(!interval_start);
            // Start tracking an interval.
            interval_start = p;
            interval_start_off = off;
            // See if we can advance interval_awaiting_clean_end to include the AwaitingCleanLength
            // of this interval.
            interval_awaiting_clean_end = ktl::max(interval_awaiting_clean_end,
                                                   off + p->GetZeroIntervalAwaitingCleanLength());
          }
          if (p->IsIntervalEnd() || p->IsIntervalSlot()) {
            // Can only transition the end if we saw the corresponding start.
            if (interval_start) {
              AssertHeld(lock_ref());
              if (off < interval_awaiting_clean_end) {
                // The entire interval is clean, so can remove it.
                if (interval_start_off != off) {
                  *interval_start = VmPageOrMarker::Empty();
                  // Return the start slot as it could have come from an earlier page list node.
                  // If the start slot came from the same node, we know that we still have a
                  // non-empty slot in that node (the current interval end we're looking at), and so
                  // the current node cannot be freed up, making it safe to continue traversal. The
                  // interval start should always be released before the end, which is consistent
                  // with forward traversal done by RemovePages.
                  page_list_.ReturnEmptySlot(interval_start_off);
                }
                // This empty slot with be returned by the RemovePages iterator.
                *p = VmPageOrMarker::Empty();
              } else {
                // The entire interval cannot be marked clean. Move forward the start by awaiting
                // clean length, which will also set the AwaitingCleanLength for the resulting
                // interval.
                // Ignore any errors. Cleaning is best effort. If this fails, the interval will
                // remain as is and get retried on another writeback attempt.
                page_list_.ClipIntervalStart(interval_start_off,
                                             interval_awaiting_clean_end - interval_start_off);
              }
              // Either way, the interval start tracking needs to be reset.
              interval_start = nullptr;
            }
          }
          return ZX_ERR_NEXT;
        }
        // This was either a marker (which is already clean), or a non-AwaitingClean page.
        DEBUG_ASSERT(p->IsMarker() || !is_page_awaiting_clean(p->Page()));
        return ZX_ERR_NEXT;
      },
      start_offset, end_offset);

  // Handle the last partial interval.
  if (interval_start) {
    // Ignore any errors. Cleaning is best effort. If this fails, the interval will remain as is and
    // get retried on another writeback attempt.
    page_list_.ClipIntervalStart(
        interval_start_off, ktl::min(interval_awaiting_clean_end, end_offset) - interval_start_off);
  }

  VMO_VALIDATION_ASSERT(DebugValidateZeroIntervalsLocked());
  return ZX_OK;
}

const VmCowPages* VmCowPages::GetRootLocked() const {
  auto cow_pages = this;
  AssertHeld(cow_pages->lock_ref());
  while (cow_pages->parent_) {
    cow_pages = cow_pages->parent_.get();
    // We just checked that this is not null in the loop conditional.
    DEBUG_ASSERT(cow_pages);
  }
  DEBUG_ASSERT(cow_pages);
  return cow_pages;
}

fbl::RefPtr<VmCowPages> VmCowPages::DebugGetParent() {
  canary_.Assert();

  Guard<CriticalMutex> guard{lock()};
  return parent_;
}

fbl::RefPtr<PageSource> VmCowPages::GetRootPageSourceLocked() const {
  auto root = GetRootLocked();
  // The root will never be null. It will either point to a valid parent, or |this| if there's no
  // parent.
  DEBUG_ASSERT(root);
  return root->page_source_;
}

void VmCowPages::DetachSourceLocked() {
  canary_.Assert();

  DEBUG_ASSERT(page_source_);
  page_source_->Detach();

  // We stack-own loaned pages from RemovePages() to FreePagesLocked().
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  list_node_t freed_list;
  list_initialize(&freed_list);

  // We would like to remove all committed pages so that all future page faults on this VMO and its
  // clones can fail in a deterministic manner. However, if the page source is preserving content
  // (is a userpager), we need to hold on to un-Clean (Dirty and AwaitingClean pages) so that they
  // can be written back by the page source. If the page source is not preserving content, its pages
  // will not be dirty tracked to begin with i.e. their dirty state will be Untracked, so we will
  // end up removing all pages.

  // We should only be removing pages from the root VMO.
  DEBUG_ASSERT(!parent_);

  // Even though we might end up removing only a subset of the pages, unmap them all at once as an
  // optimization. Only the userpager is expected to access (dirty) pages beyond this point, in
  // order to write back their contents, where the cost of the writeback is presumably much larger
  // than page faults to update hardware page table mappings for resident pages.
  RangeChangeUpdateLocked(VmCowRange(0, size_), RangeChangeOp::Unmap);

  __UNINITIALIZED BatchPQRemove page_remover(&freed_list);

  // Remove all clean (or untracked) pages.
  // TODO(rashaeqbal): Pages that linger after this will be written back and marked clean at some
  // point, and will age through the pager-backed queues and eventually get evicted. We could
  // adopt an eager approach instead, and decommit those pages as soon as they get marked clean.
  // If we do that, we could also extend the eager approach to supply_pages, where pages get
  // decommitted on supply, i.e. the supply is a no-op.
  page_list_.RemovePages(
      [&page_remover](VmPageOrMarker* p, uint64_t off) {
        // A marker is a clean zero page. Replace it with an empty slot.
        if (p->IsMarker()) {
          *p = VmPageOrMarker::Empty();
          return ZX_ERR_NEXT;
        }

        // Zero intervals are dirty so they cannot be removed.
        if (p->IsIntervalZero()) {
          // TODO: Remove clean intervals once they are supported.
          DEBUG_ASSERT(!p->IsZeroIntervalClean());
          return ZX_ERR_NEXT;
        }

        // VMOs with a page source cannot have references.
        DEBUG_ASSERT(p->IsPage());

        // We cannot remove the page if it is dirty-tracked but not clean.
        if (is_page_dirty_tracked(p->Page()) && !is_page_clean(p->Page())) {
          DEBUG_ASSERT(!p->Page()->is_loaned());
          return ZX_ERR_NEXT;
        }

        // This is a page that we're going to remove; we don't expect it to be pinned.
        DEBUG_ASSERT(p->Page()->object.pin_count == 0);

        page_remover.Push(p->ReleasePage());
        return ZX_ERR_NEXT;
      },
      0, size_);

  page_remover.Flush();
  FreePagesLocked(&freed_list, /*freeing_owned_pages=*/true);

  IncrementHierarchyGenerationCountLocked();
}

void VmCowPages::RangeChangeUpdateFromParentLocked(const uint64_t offset, const uint64_t len,
                                                   RangeChangeList* list) {
  canary_.Assert();

  LTRACEF("offset %#" PRIx64 " len %#" PRIx64 " p_offset %#" PRIx64 " size_ %#" PRIx64 "\n", offset,
          len, parent_offset_, size_);

  // our parent is notifying that a range of theirs changed, see where it intersects
  // with our offset into the parent and pass it on
  uint64_t offset_new;
  uint64_t len_new;
  if (!GetIntersect(parent_offset_, size_, offset, len, &offset_new, &len_new)) {
    return;
  }

  // if they intersect with us, then by definition the new offset must be >= parent_offset_
  DEBUG_ASSERT(offset_new >= parent_offset_);

  // subtract our offset
  offset_new -= parent_offset_;

  // verify that it's still within range of us
  DEBUG_ASSERT(offset_new + len_new <= size_);

  LTRACEF("new offset %#" PRIx64 " new len %#" PRIx64 "\n", offset_new, len_new);

  // Check if there are any gaps in this range where we would actually see the parent.
  uint64_t first_gap_start = UINT64_MAX;
  uint64_t last_gap_end = 0;
  page_list_.ForEveryPageAndGapInRange(
      [](auto page, uint64_t offset) {
        // For anything in the page list we know we do not see the parent for this offset, so
        // regardless of what it is just keep looking for a gap. Additionally any children that we
        // have will see this content instead of our parents, and so we know it is also safe to skip
        // them as well.
        return ZX_ERR_NEXT;
      },
      [&first_gap_start, &last_gap_end](uint64_t start, uint64_t end) {
        first_gap_start = ktl::min(first_gap_start, start);
        last_gap_end = ktl::max(last_gap_end, end);
        return ZX_ERR_NEXT;
      },
      offset_new, offset_new + len_new);

  if (first_gap_start >= last_gap_end) {
    // Entire range was traversed and no gaps found. Neither us, nor our children, can see the
    // parents content for this range and so we can skip the range update.
    vm_vmo_range_update_from_parent_skipped.Add(1);
    return;
  }

  // Construct a new, potentially smaller, range that covers the gaps. This will still result in
  // potentially processing pages that are locally covered, but are limited to a single range here.
  offset_new = first_gap_start;
  len_new = last_gap_end - first_gap_start;
  vm_vmo_range_update_from_parent_performed.Add(1);

  // pass it on. to prevent unbounded recursion we package up our desired offset and len and add
  // ourselves to the list. UpdateRangeLocked will then get called on it later.
  range_change_range_ = VmCowRange(offset_new, len_new);
  list->push_front(this);
}

void VmCowPages::RangeChangeUpdateListLocked(RangeChangeList* list, RangeChangeOp op) {
  while (!list->is_empty()) {
    VmCowPages* object = list->pop_front();
    AssertHeld(object->lock_ref());

    // Check if there is an associated backlink, and if so pass the operation over.
    if (object->paged_ref_) {
      AssertHeld(object->paged_ref_->lock_ref());
      object->paged_ref_->RangeChangeUpdateLocked(object->range_change_range_, op);
    }

    // inform all our children this as well, so they can inform their mappings
    for (auto& child : object->children_list_) {
      AssertHeld(child.lock_ref());
      child.RangeChangeUpdateFromParentLocked(object->range_change_range_.offset,
                                              object->range_change_range_.len, list);
    }
  }
}

void VmCowPages::RangeChangeUpdateLocked(VmCowRange range, RangeChangeOp op) {
  canary_.Assert();

  if (range.is_empty()) {
    return;
  }

  // If we have no children then we can avoid building a processing a list and just directly
  // process any referenced paged_ref_.
  if (children_list_len_ == 0) {
    if (paged_ref_) {
      AssertHeld(paged_ref_->lock_ref());
      paged_ref_->RangeChangeUpdateLocked(range, op);
    }
    return;
  }

  RangeChangeList list;
  this->range_change_range_ = range;
  list.push_front(this);
  RangeChangeUpdateListLocked(&list, op);
}

void VmCowPages::ReclaimPageForEviction(vm_page_t* page, uint64_t offset) {
  canary_.Assert();

  Guard<CriticalMutex> guard{lock()};

  // Check this page is still a part of this VMO.
  const VmPageOrMarker* page_or_marker = page_list_.Lookup(offset);
  if (!page_or_marker || !page_or_marker->IsPage() || page_or_marker->Page() != page) {
    return;
  }

  // We shouldn't have been asked to evict a pinned page.
  ASSERT(page->object.pin_count == 0);

  // Ignore any hints, we were asked directly to evict.
  ReclaimPageForEvictionLocked(page, offset, EvictionHintAction::Ignore);
}

VmCowPages::ReclaimCounts VmCowPages::ReclaimPageForEvictionLocked(vm_page_t* page, uint64_t offset,
                                                                   EvictionHintAction hint_action) {
  // Without a page source to bring the page back in we cannot even think about eviction.
  if (!can_evict()) {
    return ReclaimCounts{};
  }

  // We can assume this page is in the VMO.
#if (DEBUG_ASSERT_IMPLEMENTED)
  {
    const VmPageOrMarker* page_or_marker = page_list_.Lookup(offset);
    DEBUG_ASSERT(page_or_marker);
    DEBUG_ASSERT(page_or_marker->IsPage());
    DEBUG_ASSERT(page_or_marker->Page() == page);
  }
#endif

  DEBUG_ASSERT(is_page_dirty_tracked(page));

  // We cannot evict the page unless it is clean. If the page is dirty, it will already have been
  // moved to the dirty page queue.
  if (!is_page_clean(page)) {
    DEBUG_ASSERT(!page->is_loaned());
    return ReclaimCounts{};
  }

  // Do not evict if the |always_need| hint is set, unless we are told to ignore the eviction hint.
  if (page->object.always_need == 1 && hint_action == EvictionHintAction::Follow) {
    DEBUG_ASSERT(!page->is_loaned());
    // We still need to move the page from the tail of the LRU page queue(s) so that the eviction
    // loop can make progress. Since this page is always needed, move it out of the way and into the
    // MRU queue. Do this here while we hold the lock, instead of at the callsite.
    //
    // TODO(rashaeqbal): Since we're essentially simulating an access here, this page may not
    // qualify for eviction if we do decide to override the hint soon after (i.e. if an OOM follows
    // shortly after). Investigate adding a separate queue once we have some more data around hints
    // usage. A possible approach might involve moving to a separate queue when we skip the page for
    // eviction. Pages move out of said queue when accessed, and continue aging as other pages.
    // Pages in the queue are considered for eviction pre-OOM, but ignored otherwise.
    pmm_page_queues()->MarkAccessed(page);
    vm_vmo_always_need_skipped_reclaim.Add(1);
    return ReclaimCounts{};
  }

  // Remove any mappings to this page before we remove it.
  RangeChangeUpdateLocked(VmCowRange(offset, PAGE_SIZE), RangeChangeOp::Unmap);

  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;
  // Use RemovePage over just writing to page_or_marker so that the page list has the opportunity
  // to release any now empty intermediate nodes.
  vm_page_t* p = page_list_.RemoveContent(offset).ReleasePage();
  DEBUG_ASSERT(p == page);
  pmm_page_queues()->Remove(page);
  const bool loaned = page->is_loaned();
  FreePageLocked(page, true);

  reclamation_event_count_++;
  IncrementHierarchyGenerationCountLocked();
  VMO_VALIDATION_ASSERT(DebugValidateHierarchyLocked());
  VMO_FRUGAL_VALIDATION_ASSERT(DebugValidateVmoPageBorrowingLocked());
  return ReclaimCounts{
      .evicted_non_loaned = loaned ? 0u : 1u,
      .evicted_loaned = loaned ? 1u : 0u,
  };
}

VmCowPages::ReclaimCounts VmCowPages::ReclaimPageForCompressionLocked(vm_page_t* page,
                                                                      uint64_t offset,
                                                                      VmCompressor* compressor,
                                                                      Guard<CriticalMutex>& guard) {
  DEBUG_ASSERT(compressor);
  DEBUG_ASSERT(!page_source_);
  ASSERT(page->object.pin_count == 0);
  DEBUG_ASSERT(!page->is_loaned());
  DEBUG_ASSERT(!discardable_tracker_);
  DEBUG_ASSERT(can_decommit_zero_pages_locked());
  if (paged_ref_) {
    AssertHeld(paged_ref_->lock_ref());
    if ((paged_ref_->GetMappingCachePolicyLocked() & ZX_CACHE_POLICY_MASK) !=
        ZX_CACHE_POLICY_CACHED) {
      // Cannot compress uncached mappings. To avoid this page remaining in the reclamation list we
      // simulate an access.
      pmm_page_queues()->MarkAccessed(page);
      return ReclaimCounts{};
    }
  }

  // Track whether we should tell the caller we reclaimed a page or not.
  bool reclaimed = false;

  // Use a sub-scope as the page_or_marker will become invalid as we will drop the lock later.
  {
    VmPageOrMarkerRef page_or_marker = page_list_.LookupMutable(offset);
    DEBUG_ASSERT(page_or_marker);
    DEBUG_ASSERT(page_or_marker->IsPage());
    DEBUG_ASSERT(page_or_marker->Page() == page);

    RangeChangeUpdateLocked(VmCowRange(offset, PAGE_SIZE), RangeChangeOp::Unmap);

    // Start compression of the page by swapping the page list to contain the temporary reference.
    // Ensure the compression system is aware of the page's current share_count so it can track any
    // changes we make to that value while compression is running.
    VmPageOrMarker::ReferenceValue temp_ref = compressor->Start(
        VmCompressor::PageAndMetadata{.page = page, .metadata = page->object.share_count});
    [[maybe_unused]] vm_page_t* compress_page = page_or_marker.SwapPageForReference(temp_ref);
    DEBUG_ASSERT(compress_page == page);
  }
  pmm_page_queues()->Remove(page);
  // Going to drop the lock so need to indicate that we've modified the hierarchy by putting in the
  // temporary reference.
  IncrementHierarchyGenerationCountLocked();

  // We now stack own the page (and guarantee to the compressor that it will not be modified) and
  // the VMO owns the temporary reference. We can safely drop the VMO lock and perform the
  // compression step.
  guard.CallUnlocked([compressor] { compressor->Compress(); });

  // Retrieve the result of compression now that we hold the VMO lock again.
  VmCompressor::CompressResult compression_result = compressor->TakeCompressionResult();

  // We hold the VMO lock again and need to reclaim the temporary reference. Either the
  // temporary reference is still installed, and since we hold the VMO lock we now own both the
  // temp reference and the place, or the temporary reference got replaced, in which case it no
  // longer exists and is not referring to page and so we own page.
  //
  // Determining what state we are in just requires re-looking up the slot and see if the temporary
  // reference we installed is still there.
  auto [slot, is_in_interval] =
      page_list_.LookupOrAllocate(offset, VmPageList::IntervalHandling::NoIntervals);
  DEBUG_ASSERT(!is_in_interval);
  if (slot && slot->IsReference() && compressor->IsTempReference(slot->Reference())) {
    // Slot still holds the original reference; need to replace it with the result of compression.
    VmPageOrMarker::ReferenceValue old_ref{0};
    if (const VmPageOrMarker::ReferenceValue* ref =
            ktl::get_if<VmPageOrMarker::ReferenceValue>(&compression_result)) {
      // Compression succeeded, put the new reference in.
      // When compression succeeded, the |compressor| internally copied the page's metadata from
      // the temp reference to the new reference so we don't need to manually copy it here.
      old_ref = VmPageOrMarkerRef(slot).SwapReferenceForReference(*ref);
      reclamation_event_count_++;
      reclaimed = true;
    } else if (VmCompressor::FailTag* fail =
                   ktl::get_if<VmCompressor::FailTag>(&compression_result)) {
      // Compression failed, put the page back in the slot.
      // The |compressor| doesn't know how to update the |page| with any changes we made to its
      // metadata while compression was running, so we need to manually copy the metadata over to
      // the page's share_count here.
      DEBUG_ASSERT(page == fail->src_page.page);
      page->object.share_count = fail->src_page.metadata;
      old_ref = VmPageOrMarkerRef(slot).SwapReferenceForPage(page);
      // TODO(https://fxbug.dev/42138396): Placing in a queue and then moving it is inefficient, but
      // avoids needing to reason about whether reclamation could be manually attempted on pages
      // that might otherwise not end up in the reclaimable queues.
      SetNotPinnedLocked(page, offset);
      // TODO(https://fxbug.dev/42138396): Marking this page as failing reclamation will prevent it
      // from ever being tried again. As compression might succeed if the contents changes, we
      // should consider moving the page out of this queue if it is modified.
      pmm_page_queues()->CompressFailed(page);
      // Page stays owned by the VMO.
      page = nullptr;
    } else {
      ASSERT(ktl::holds_alternative<VmCompressor::ZeroTag>(compression_result));
      old_ref = slot->ReleaseReference();
      // Check if we can clear the slot, or if we need to insert a marker. Unlike the full zero
      // pages this simply needs to check if there's any visible content above us, and then if there
      // isn't if the root is immutable or not (i.e. if it has a page source).
      VmCowPages* page_owner;
      uint64_t owner_offset;
      if (!FindInitialPageContentLocked(offset, &page_owner, &owner_offset, nullptr).current() &&
          !page_owner->page_source_) {
        *slot = VmPageOrMarker::Empty();
        page_list_.ReturnEmptySlot(offset);
        vm_vmo_compression_zero_slot.Add(1);
      } else {
        *slot = VmPageOrMarker::Marker();
        vm_vmo_compression_marker.Add(1);
      }
      reclamation_event_count_++;
      reclaimed = true;
    }
    // Temporary reference has been replaced, can return it to the compressor.
    compressor->ReturnTempReference(old_ref);
    // Have done a modification.
    IncrementHierarchyGenerationCountLocked();
  } else {
    // The temporary reference is no longer there. We know nothing else about the state of the VMO
    // at this point and will just free any compression result and exit.
    if (const VmPageOrMarker::ReferenceValue* ref =
            ktl::get_if<VmPageOrMarker::ReferenceValue>(&compression_result)) {
      compressor->Free(*ref);
    }
    // If the slot is allocated, but empty, then make sure we properly return it.
    if (slot && slot->IsEmpty()) {
      page_list_.ReturnEmptySlot(offset);
    }
    // In this case we are still going to free the page, but it doesn't count as a reclamation as
    // there is now something new in the slot we were trying to free.
  }
  // One way or another the temporary reference has been returned, and so we can finalize.
  compressor->Finalize();

  if (page) {
    FreePageLocked(page, true);
    page = nullptr;
  }

  return VmCowPages::ReclaimCounts{.compressed = reclaimed ? 1u : 0u};
}

VmCowPages::ReclaimCounts VmCowPages::ReclaimPage(vm_page_t* page, uint64_t offset,
                                                  EvictionHintAction hint_action,
                                                  VmCompressor* compressor) {
  canary_.Assert();

  Guard<CriticalMutex> guard{lock()};
  // Check this page is still a part of this VMO.
  const VmPageOrMarker* page_or_marker = page_list_.Lookup(offset);
  if (!page_or_marker || !page_or_marker->IsPage() || page_or_marker->Page() != page) {
    return ReclaimCounts{};
  }

  // Pinned pages could be in use by DMA so we cannot safely reclaim them.
  if (page->object.pin_count != 0) {
    return ReclaimCounts{};
  }

  if (high_priority_count_ != 0) {
    // Not allowed to reclaim. To avoid this page remaining in a reclamation list we simulate an
    // access.
    pmm_page_queues()->MarkAccessed(page);
    return ReclaimCounts{};
  }

  // See if we can reclaim by eviction.
  if (can_evict()) {
    return ReclaimPageForEvictionLocked(page, offset, hint_action);
  } else if (compressor && !page_source_ && !discardable_tracker_) {
    return ReclaimPageForCompressionLocked(page, offset, compressor, guard);
  } else if (discardable_tracker_) {
    // On any errors touch the page so we stop trying to reclaim it. In particular for discardable
    // reclamation attempts, if the page we are passing is not the first page in the discardable
    // VMO then the discard will fail, so touching it will stop us from continuously trying to
    // trigger a discard with it.
    auto result = ReclaimDiscardableLocked(page, offset);
    if (result.is_error()) {
      pmm_page_queues()->MarkAccessed(page);
      vm_vmo_discardable_failed_reclaim.Add(1);
      return ReclaimCounts{};
    }
    return ReclaimCounts{.discarded = *result};
  }
  // No other reclamation strategies, so to avoid this page remaining in a reclamation list we
  // simulate an access. Do not want to place it in the ReclaimFailed queue since our failure was
  // not based on page contents.
  pmm_page_queues()->MarkAccessed(page);
  // Keep a count as having no reclamation strategy is probably a sign of miss-configuration.
  vm_vmo_no_reclamation_strategy.Add(1);
  return ReclaimCounts{};
}

void VmCowPages::SwapPageLocked(uint64_t offset, vm_page_t* old_page, vm_page_t* new_page) {
  DEBUG_ASSERT(!old_page->object.pin_count);
  DEBUG_ASSERT(new_page->state() == vm_page_state::OBJECT);

  // unmap before removing old page
  RangeChangeUpdateLocked(VmCowRange(offset, PAGE_SIZE), RangeChangeOp::Unmap);

  // The caller is required to have provided us a page that exists, so this can never fail.
  VmPageOrMarkerRef p = page_list_.LookupMutable(offset);
  DEBUG_ASSERT(p);
  DEBUG_ASSERT(p->IsPage());

  CopyPageForReplacementLocked(new_page, old_page);

  // Add replacement page in place of old page.
  __UNINITIALIZED auto result = BeginAddPageWithSlotLocked(offset, p, CanOverwriteContent::NonZero);
  // Absent bugs, BeginAddPageWithSlotLocked() can only return ZX_ERR_NO_MEMORY, but that failure
  // can only occur if page_list_ had to allocate.  Here, page_list_ hasn't yet had a chance to
  // clean up any internal structures, so BeginAddPageWithSlotLocked() didn't need to allocate, so
  // we know that BeginAddPageWithSlotLocked() will succeed.
  DEBUG_ASSERT(result.is_ok());
  VmPageOrMarker released_page =
      CompleteAddPageLocked(*result, VmPageOrMarker::Page(new_page), /*do_range_update=*/false);
  // The page released was the old page.
  DEBUG_ASSERT(released_page.IsPage() && released_page.Page() == old_page);
  // Need to take the page out of |released_page| to avoid a [[nodiscard]] error. Since we just
  // checked that this matches the target page, which is now owned by the caller, this is not
  // leaking.
  [[maybe_unused]] vm_page_t* released = released_page.ReleasePage();
}

zx_status_t VmCowPages::ReplacePagesWithNonLoanedLocked(VmCowRange range,
                                                        AnonymousPageRequest* page_request,
                                                        uint64_t* non_loaned_len) {
  canary_.Assert();

  DEBUG_ASSERT(range.is_page_aligned());
  DEBUG_ASSERT(range.IsBoundedBy(size_));
  DEBUG_ASSERT(non_loaned_len);

  if (is_slice_locked()) {
    return slice_parent_locked().ReplacePagesWithNonLoanedLocked(range.OffsetBy(parent_offset_),
                                                                 page_request, non_loaned_len);
  }

  *non_loaned_len = 0;
  bool found_page_or_gap = false;
  zx_status_t status = page_list_.ForEveryPageAndGapInRange(
      [page_request, non_loaned_len, &found_page_or_gap, this](const VmPageOrMarker* p,
                                                               uint64_t off) {
        found_page_or_gap = true;
        // We only expect committed pages in the specified range.
        if (p->IsMarker() || p->IsReference() || p->IsInterval()) {
          return ZX_ERR_BAD_STATE;
        }
        vm_page_t* page = p->Page();
        // If the page is loaned, replace is with a non-loaned page.
        if (page->is_loaned()) {
          AssertHeld(lock_ref());
          // A loaned page could only have been clean.
          DEBUG_ASSERT(!is_page_dirty_tracked(page) || is_page_clean(page));
          DEBUG_ASSERT(page_request);
          zx_status_t status =
              ReplacePageLocked(page, off, /*with_loaned=*/false, &page, page_request);
          if (status == ZX_ERR_SHOULD_WAIT) {
            return status;
          }
          if (status != ZX_OK) {
            return ZX_ERR_BAD_STATE;
          }
        }
        DEBUG_ASSERT(!page->is_loaned());
        *non_loaned_len += PAGE_SIZE;
        return ZX_ERR_NEXT;
      },
      [&found_page_or_gap](uint64_t start, uint64_t end) {
        found_page_or_gap = true;
        // We only expect committed pages in the specified range.
        return ZX_ERR_BAD_STATE;
      },
      range.offset, range.end());

  if (status != ZX_OK) {
    return status;
  }

  // If we did not find a page or a gap, the entire range fell inside an interval. We only expect
  // committed pages in the range.
  if (!found_page_or_gap) {
    return ZX_ERR_BAD_STATE;
  }

  return ZX_OK;
}

zx_status_t VmCowPages::ReplacePageWithLoaned(vm_page_t* before_page, uint64_t offset) {
  canary_.Assert();

  Guard<CriticalMutex> guard{lock()};
  return ReplacePageLocked(before_page, offset, true, nullptr, nullptr);
}

zx_status_t VmCowPages::ReplacePageLocked(vm_page_t* before_page, uint64_t offset, bool with_loaned,
                                          vm_page_t** after_page,
                                          AnonymousPageRequest* page_request) {
  // If not replacing with loaned it is required that a page_request be provided.
  DEBUG_ASSERT(with_loaned || page_request);

  const VmPageOrMarker* p = page_list_.Lookup(offset);
  if (!p) {
    return ZX_ERR_NOT_FOUND;
  }
  if (!p->IsPage()) {
    return ZX_ERR_NOT_FOUND;
  }
  vm_page_t* old_page = p->Page();
  if (old_page != before_page) {
    return ZX_ERR_NOT_FOUND;
  }
  DEBUG_ASSERT(old_page != vm_get_zero_page());
  if (old_page->object.pin_count != 0) {
    DEBUG_ASSERT(!old_page->is_loaned());
    return ZX_ERR_BAD_STATE;
  }
  if (old_page->object.always_need) {
    DEBUG_ASSERT(!old_page->is_loaned());
    return ZX_ERR_BAD_STATE;
  }

  // We stack-own a loaned page from pmm_alloc_page() to SwapPageLocked() OR from SwapPageLocked()
  // until FreePageLocked().
  __UNINITIALIZED StackOwnedLoanedPagesInterval raii_interval;

  vm_page_t* new_page = nullptr;
  zx_status_t status = ZX_OK;
  if (with_loaned) {
    if (!can_borrow_locked()) {
      return ZX_ERR_NOT_SUPPORTED;
    }
    if (is_page_dirty_tracked(old_page) && !is_page_clean(old_page)) {
      return ZX_ERR_BAD_STATE;
    }
    status = AllocLoanedPage(&new_page);
  } else {
    status = AllocPage(&new_page, page_request);
  }
  if (status != ZX_OK) {
    return status;
  }

  SwapPageLocked(offset, old_page, new_page);
  pmm_page_queues()->Remove(old_page);
  FreePageLocked(old_page, /*freeing_owned_page=*/true);
  if (after_page) {
    *after_page = new_page;
  }

  // We've changed a page in the page list. Update the generation count.
  IncrementHierarchyGenerationCountLocked();

  return ZX_OK;
}

bool VmCowPages::DebugValidateHierarchyLocked() const TA_REQ(lock()) {
  canary_.Assert();

  const VmCowPages* cur = this;
  AssertHeld(cur->lock_ref());
  const VmCowPages* parent_most = cur;
  do {
    if (!cur->DebugValidatePageSharingLocked()) {
      return false;
    }
    cur = cur->parent_.get();
    if (cur) {
      parent_most = cur;
    }
  } while (cur);
  // Iterate whole hierarchy; the iteration order doesn't matter.  Since there are cases with
  // >2 children, in-order isn't well defined, so we choose pre-order, but post-order would also
  // be fine.
  AssertHeld(parent_most->lock_ref());
  zx_status_t status =
      parent_most->DebugForEachDescendant([this](const VmCowPages* cur, int depth) {
        AssertHeld(cur->lock_ref());
        if (!cur->DebugValidateBacklinksLocked()) {
          dprintf(INFO, "cur: %p this: %p\n", cur, this);
          return ZX_ERR_BAD_STATE;
        }
        return ZX_OK;
      });
  return status == ZX_OK;
}

bool VmCowPages::DebugValidatePageSharingLocked() const {
  canary_.Assert();

  // Visible nodes should never contain shared pages.
  if (!is_hidden_locked()) {
    zx_status_t status =
        page_list_.ForEveryPage([this](const VmPageOrMarker* page, uint64_t offset) {
          if (!page->IsPageOrRef()) {
            return ZX_ERR_NEXT;
          }
          AssertHeld(lock_ref());

          const uint32_t share_count = GetShareCount(page);
          if (share_count != 0) {
            if (page->IsPage()) {
              printf("Found shared page in visible node %p (page %p) (off %#" PRIx64
                     ") (share %" PRIu32 "), but expected it to be private\n",
                     this, page->Page(), offset, share_count);
            } else {
              printf("Found shared reference in visible node %p (off %#" PRIx64 ") (share %" PRIu32
                     "), but expected it to be private\n",
                     this, offset, share_count);
            }
            DumpLocked(1, true);
            return ZX_ERR_BAD_STATE;
          }

          return ZX_ERR_NEXT;
        });

    // Nothing else to check for visible nodes
    return status == ZX_OK;
  }

  // Hidden nodes should share their pages with the correct number of visible nodes.
  DEBUG_ASSERT(is_hidden_locked());
  DEBUG_ASSERT(!children_list_.is_empty());  // Hidden nodes must always have children
  zx_status_t status = page_list_.ForEveryPage([this](const VmPageOrMarker* page, uint64_t offset) {
    if (!page->IsPageOrRef()) {
      return ZX_ERR_NEXT;
    }
    AssertHeld(lock_ref());

    const uint32_t share_count = GetShareCount(page);
    const VmCowPages* cur = &children_list_.front();
    uint64_t offset_in_parent = offset;
    uint32_t found_count = 0;
    // For hidden nodes, check that the share counts on their pages and references are correct.
    // For a page with a share count of N, there should be N + 1 visible nodes that can access the
    // page.
    //
    // Walk the subtree rooted at this node. At each visible node we encounter, search back up to
    // see if it can access `page`.
    //
    // We start with cur being an immediate child of 'this', so we can preform subtree traversal
    // until we end up back in 'this'.
    while (cur != this) {
      AssertHeld(cur->lock_ref());
      DEBUG_ASSERT(cur->is_parent_hidden_locked());

      // Check that we can see this page in the parent. Importantly this first checks if
      // |offset_in_parent < cur->parent_offset_| allowing us to safely perform that subtraction
      // from then on.
      if (offset_in_parent < cur->parent_offset_ ||
          offset_in_parent - cur->parent_offset_ >= cur->parent_limit_) {
        // This blank case is used to capture the scenario where current does not see the target
        // offset in the parent, in which case there is no point traversing into the children.
      } else if (cur->is_hidden_locked()) {
        // The children of a hidden node can only access the page if the hidden node isn't
        // covering it with anything, so only walk down if this offset is empty in the hidden node.
        const VmPageOrMarker* l = cur->page_list_.Lookup(offset_in_parent - cur->parent_offset_);
        if (!l || l->IsEmpty()) {
          // Page not found, we need to recurse down into our children.
          DEBUG_ASSERT(!cur->children_list_.is_empty());
          offset_in_parent -= cur->parent_offset_;
          cur = &cur->children_list_.front();
          continue;
        }
      } else {
        // `cur` is a visible node, so search up and see if it has partial ownership over the page.
        cur->ForEveryOwnedHierarchyPageInRangeLocked(
            [&](const VmPageOrMarker* p, const VmCowPages* owner, uint64_t this_offset,
                uint64_t owner_offset) {
              if (p == page) {
                DEBUG_ASSERT(owner == this);
                DEBUG_ASSERT(owner_offset == offset);
                found_count++;
                return ZX_ERR_STOP;
              }

              return ZX_ERR_NEXT;
            },
            offset_in_parent - cur->parent_offset_, PAGE_SIZE);
      }

      // Our next node should be the next available child in some `children_list_`. We will walk up
      // until `cur` is not the last child in its parent's `children_list_`.
      do {
        const VmCowPages* parent = cur->parent_.get();
        AssertHeld(parent->lock_ref());

        // Check for next child after `cur`.
        auto children_iter = parent->children_list_.make_iterator(*cur);
        children_iter++;
        if (children_iter.IsValid()) {
          cur = children_iter.CopyPointer();
          // Parent shouldn't have changed, so `offset_in_parent` doesn't need to.
          AssertHeld(cur->lock_ref());
          DEBUG_ASSERT(cur->parent_.get() == parent);
          break;
        }

        // Otherwise keep walking up.
        cur = parent;
        offset_in_parent += parent->parent_offset_;
        if (cur == this) {
          break;
        }
      } while (1);
    }

    // Ensure we found the page the correct number of times in the subtree.
    if (found_count != share_count + 1) {
      if (page->IsPage()) {
        printf("Found shared page in hidden node %p (page %p) (off %#" PRIx64 ") (share %" PRIu32
               "), but accessible by wrong number of visible nodes %" PRIu32 "\n",
               this, page->Page(), offset, share_count, found_count);
      } else {
        printf("Found shared reference in hidden node %p (off %#" PRIx64 ") (share %" PRIu32
               "), but accessible by wrong number of visible nodes %" PRIu32 "\n",
               this, offset, share_count, found_count);
      }
      DumpLocked(1, true);
      return ZX_ERR_BAD_STATE;
    }

    return ZX_ERR_NEXT;
  });

  return status == ZX_OK;
}

bool VmCowPages::DebugValidateBacklinksLocked() const {
  canary_.Assert();
  bool result = true;
  page_list_.ForEveryPage([this, &result](const auto* p, uint64_t offset) {
    // Markers, references, and intervals don't have backlinks.
    if (p->IsReference() || p->IsMarker() || p->IsInterval()) {
      return ZX_ERR_NEXT;
    }
    vm_page_t* page = p->Page();
    vm_page_state state = page->state();
    if (state != vm_page_state::OBJECT) {
      dprintf(INFO, "unexpected page state: %u\n", static_cast<uint32_t>(state));
      result = false;
      return ZX_ERR_STOP;
    }
    const VmCowPages* object = reinterpret_cast<VmCowPages*>(page->object.get_object());
    if (!object) {
      dprintf(INFO, "missing object\n");
      result = false;
      return ZX_ERR_STOP;
    }
    if (object != this) {
      dprintf(INFO, "incorrect object - object: %p this: %p\n", object, this);
      result = false;
      return ZX_ERR_STOP;
    }
    uint64_t page_offset = page->object.get_page_offset();
    if (page_offset != offset) {
      dprintf(INFO, "incorrect offset - page_offset: %" PRIx64 " offset: %" PRIx64 "\n",
              page_offset, offset);
      result = false;
      return ZX_ERR_STOP;
    }
    return ZX_ERR_NEXT;
  });
  return result;
}

bool VmCowPages::DebugValidateVmoPageBorrowingLocked() const {
  canary_.Assert();
  // Skip checking larger VMOs to avoid slowing things down too much, since the things being
  // verified will typically assert from incorrect behavior on smaller VMOs (and we can always
  // remove this filter if we suspect otherwise).
  if (size_ >= 2 * 1024 * 1024) {
    return true;
  }
  bool result = true;
  page_list_.ForEveryPage([this, &result](const auto* p, uint64_t offset) {
    AssertHeld(lock_ref());
    if (!p->IsPage()) {
      // If we don't have a page, this is either a marker or reference, both of which are not
      // allowed with contiguous VMOs.
      DEBUG_ASSERT(!direct_source_supplies_zero_pages());
      return ZX_ERR_NEXT;
    }
    vm_page_t* page = p->Page();
    if (page->is_loaned()) {
      if (!can_borrow_locked()) {
        dprintf(INFO, "!can_borrow_locked() but page is loaned?? - offset: 0x%" PRIx64 "\n",
                offset);
        result = false;
        return ZX_ERR_STOP;
      }
      if (page->object.pin_count) {
        dprintf(INFO, "pinned page is loaned?? - offset: 0x%" PRIx64 "\n", offset);
        result = false;
        return ZX_ERR_STOP;
      }
      if (page->object.always_need) {
        dprintf(INFO, "always_need page is loaned?? - offset: 0x%" PRIx64 "\n", offset);
        result = false;
        return ZX_ERR_STOP;
      }
      if (is_page_dirty_tracked(page) && !is_page_clean(page)) {
        dprintf(INFO, "!clean page is loaned?? - offset: 0x%" PRIx64 "\n", offset);
        result = false;
        return ZX_ERR_STOP;
      }
    }
    return ZX_ERR_NEXT;
  });
  if (!result) {
    dprintf(INFO, "DebugValidateVmoPageBorrowingLocked() failing - slice: %d\n", is_slice_locked());
  }
  return result;
}

bool VmCowPages::DebugValidateZeroIntervalsLocked() const {
  canary_.Assert();
  bool in_interval = false;
  auto dirty_state = VmPageOrMarker::IntervalDirtyState::Untracked;
  zx_status_t status = page_list_.ForEveryPage(
      [&in_interval, &dirty_state, pager_backed = is_source_preserving_page_content()](
          const VmPageOrMarker* p, uint64_t off) {
        if (!pager_backed) {
          if (p->IsInterval()) {
            dprintf(INFO, "found interval at offset 0x%" PRIx64 " in non pager backed vmo\n", off);
            return ZX_ERR_BAD_STATE;
          }
          return ZX_ERR_NEXT;
        }

        if (p->IsInterval()) {
          DEBUG_ASSERT(p->IsIntervalZero());
          DEBUG_ASSERT(p->IsZeroIntervalDirty() || p->IsZeroIntervalUntracked());
          if (p->IsIntervalStart()) {
            if (in_interval) {
              dprintf(INFO, "interval start at 0x%" PRIx64 " while already in interval\n", off);
              return ZX_ERR_BAD_STATE;
            }
            in_interval = true;
            dirty_state = p->GetZeroIntervalDirtyState();
          } else if (p->IsIntervalEnd()) {
            if (!in_interval) {
              dprintf(INFO, "interval end at 0x%" PRIx64 " while not in interval\n", off);
              return ZX_ERR_BAD_STATE;
            }
            if (p->GetZeroIntervalDirtyState() != dirty_state) {
              dprintf(INFO, "dirty state mismatch - start %lu, end %lu\n", (uint64_t)(dirty_state),
                      (uint64_t)(p->GetZeroIntervalDirtyState()));
              return ZX_ERR_BAD_STATE;
            }
            in_interval = false;
            dirty_state = VmPageOrMarker::IntervalDirtyState::Untracked;
          } else {
            if (in_interval) {
              dprintf(INFO, "interval slot at 0x%" PRIx64 " while already in interval\n", off);
              return ZX_ERR_BAD_STATE;
            }
          }
          return ZX_ERR_NEXT;
        }

        if (p->IsReference()) {
          dprintf(INFO, "found compressed ref at offset 0x%" PRIx64 " in pager backed vmo\n", off);
          return ZX_ERR_BAD_STATE;
        }

        if (p->IsPage() && in_interval) {
          dprintf(INFO, "found page at 0x%" PRIx64 " in interval\n", off);
          return ZX_ERR_BAD_STATE;
        }

        if (p->IsMarker() && in_interval) {
          dprintf(INFO, "found marker at 0x%" PRIx64 " in interval\n", off);
          return ZX_ERR_BAD_STATE;
        }
        return ZX_ERR_NEXT;
      });
  return status == ZX_OK;
}

bool VmCowPages::IsLockRangeValidLocked(VmCowRange range) const {
  return range.offset == 0 && range.len == size_locked();
}

zx_status_t VmCowPages::LockRangeLocked(VmCowRange range, zx_vmo_lock_state_t* lock_state_out) {
  canary_.Assert();
  ASSERT(discardable_tracker_);

  AssertHeld(lock_ref());
  if (!IsLockRangeValidLocked(range)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!lock_state_out) {
    return ZX_ERR_INVALID_ARGS;
  }
  lock_state_out->offset = range.offset;
  lock_state_out->size = range.len;

  discardable_tracker_->assert_cow_pages_locked();

  bool was_discarded = false;
  zx_status_t status =
      discardable_tracker_->LockDiscardableLocked(/*try_lock=*/false, &was_discarded);
  // Locking must succeed if try_lock was false.
  DEBUG_ASSERT(status == ZX_OK);
  lock_state_out->discarded_offset = 0;
  lock_state_out->discarded_size = was_discarded ? size_locked() : 0;

  return status;
}

zx_status_t VmCowPages::TryLockRangeLocked(VmCowRange range) {
  canary_.Assert();
  ASSERT(discardable_tracker_);

  AssertHeld(lock_ref());
  if (!IsLockRangeValidLocked(range)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  discardable_tracker_->assert_cow_pages_locked();
  bool unused;
  return discardable_tracker_->LockDiscardableLocked(/*try_lock=*/true, &unused);
}

zx_status_t VmCowPages::UnlockRangeLocked(VmCowRange range) {
  canary_.Assert();
  ASSERT(discardable_tracker_);

  AssertHeld(lock_ref());
  if (!IsLockRangeValidLocked(range)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  discardable_tracker_->assert_cow_pages_locked();
  zx_status_t status = discardable_tracker_->UnlockDiscardableLocked();
  if (status != ZX_OK) {
    return status;
  }
  if (discardable_tracker_->IsEligibleForReclamationLocked()) {
    // Simulate an access to the first page. We use the first page as the discardable trigger, so by
    // simulating an access we ensure that an unlocked VMO is treated as recently accessed
    // equivalent to all other pages. Touching just the first page, instead of all pages, is an
    // optimization as we can simply ignore any attempts to trigger discard from those other pages.
    page_list_.ForEveryPage([](auto* p, uint64_t offset) {
      // Skip over any markers.
      if (!p->IsPage()) {
        return ZX_ERR_NEXT;
      }
      pmm_page_queues()->MarkAccessed(p->Page());
      return ZX_ERR_STOP;
    });
  }
  return status;
}

uint64_t VmCowPages::DebugGetPageCountLocked() const {
  canary_.Assert();
  uint64_t page_count = 0;
  zx_status_t status = page_list_.ForEveryPage([&page_count](auto* p, uint64_t offset) {
    if (!p->IsPageOrRef()) {
      return ZX_ERR_NEXT;
    }
    ++page_count;
    return ZX_ERR_NEXT;
  });
  // We never stop early in lambda above.
  DEBUG_ASSERT(status == ZX_OK);
  return page_count;
}

bool VmCowPages::DebugIsPage(uint64_t offset) const {
  canary_.Assert();
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  Guard<CriticalMutex> guard{lock()};
  const VmPageOrMarker* p = page_list_.Lookup(offset);
  return p && p->IsPage();
}

bool VmCowPages::DebugIsMarker(uint64_t offset) const {
  canary_.Assert();
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  Guard<CriticalMutex> guard{lock()};
  const VmPageOrMarker* p = page_list_.Lookup(offset);
  return p && p->IsMarker();
}

bool VmCowPages::DebugIsEmpty(uint64_t offset) const {
  canary_.Assert();
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  Guard<CriticalMutex> guard{lock()};
  const VmPageOrMarker* p = page_list_.Lookup(offset);
  return !p || p->IsEmpty();
}

vm_page_t* VmCowPages::DebugGetPage(uint64_t offset) const {
  canary_.Assert();
  Guard<CriticalMutex> guard{lock()};
  return DebugGetPageLocked(offset);
}

vm_page_t* VmCowPages::DebugGetPageLocked(uint64_t offset) const {
  canary_.Assert();
  DEBUG_ASSERT(IS_PAGE_ALIGNED(offset));
  const VmPageOrMarker* p = page_list_.Lookup(offset);
  if (p && p->IsPage()) {
    return p->Page();
  }
  return nullptr;
}

bool VmCowPages::DebugIsHighMemoryPriority() const {
  canary_.Assert();
  Guard<CriticalMutex> guard{lock()};
  return is_high_memory_priority_locked();
}

VmCowPages::DiscardablePageCounts VmCowPages::DebugGetDiscardablePageCounts() const {
  canary_.Assert();
  DiscardablePageCounts counts = {};

  // Not a discardable VMO.
  if (!discardable_tracker_) {
    return counts;
  }

  Guard<CriticalMutex> guard{lock()};

  discardable_tracker_->assert_cow_pages_locked();
  const DiscardableVmoTracker::DiscardableState state =
      discardable_tracker_->discardable_state_locked();
  // This is a discardable VMO but hasn't opted into locking / unlocking yet.
  if (state == DiscardableVmoTracker::DiscardableState::kUnset) {
    return counts;
  }

  uint64_t pages = 0;
  page_list_.ForEveryPage([&pages](const auto* p, uint64_t) {
    // TODO(https://fxbug.dev/42138396) Figure out attribution between pages and references.
    if (p->IsPageOrRef()) {
      ++pages;
    }
    return ZX_ERR_NEXT;
  });

  switch (state) {
    case DiscardableVmoTracker::DiscardableState::kReclaimable:
      counts.unlocked = pages;
      break;
    case DiscardableVmoTracker::DiscardableState::kUnreclaimable:
      counts.locked = pages;
      break;
    case DiscardableVmoTracker::DiscardableState::kDiscarded:
      DEBUG_ASSERT(pages == 0);
      break;
    default:
      break;
  }

  return counts;
}

uint64_t VmCowPages::DiscardPages() {
  canary_.Assert();

  Guard<CriticalMutex> guard{lock()};
  // Discard any errors and overlap a 0 return value for errors.
  return DiscardPagesLocked().value_or(0);
}

zx::result<uint64_t> VmCowPages::DiscardPagesLocked() {
  // Not a discardable VMO.
  if (!discardable_tracker_) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  discardable_tracker_->assert_cow_pages_locked();
  if (!discardable_tracker_->IsEligibleForReclamationLocked()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  // Remove all pages.
  zx::result<uint64_t> result = UnmapAndFreePagesLocked(0, size_);

  if (result.is_ok()) {
    reclamation_event_count_++;
    IncrementHierarchyGenerationCountLocked();

    // Set state to discarded.
    discardable_tracker_->SetDiscardedLocked();
  }
  return result;
}

zx::result<uint64_t> VmCowPages::ReclaimDiscardableLocked(vm_page_t* page, uint64_t offset) {
  DEBUG_ASSERT(discardable_tracker_);
  // Check if this is the first page.
  bool first = false;
  page_list_.ForEveryPage([&first, &offset, &page](auto* p, uint64_t off) {
    if (!p->IsPage()) {
      return ZX_ERR_NEXT;
    }
    first = (p->Page() == page) && off == offset;
    return ZX_ERR_STOP;
  });
  if (!first) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return DiscardPagesLocked();
}

void VmCowPages::CopyPageForReplacementLocked(vm_page_t* dst_page, vm_page_t* src_page) {
  DEBUG_ASSERT(!src_page->object.pin_count);
  void* src = paddr_to_physmap(src_page->paddr());
  DEBUG_ASSERT(src);
  void* dst = paddr_to_physmap(dst_page->paddr());
  DEBUG_ASSERT(dst);
  memcpy(dst, src, PAGE_SIZE);
  if (paged_ref_) {
    AssertHeld(paged_ref_->lock_ref());
    if (paged_ref_->GetMappingCachePolicyLocked() != ARCH_MMU_FLAG_CACHED) {
      arch_clean_invalidate_cache_range((vaddr_t)dst, PAGE_SIZE);
    }
  }
  dst_page->object.share_count = src_page->object.share_count;
  dst_page->object.always_need = src_page->object.always_need;
  DEBUG_ASSERT(!dst_page->object.always_need || (!dst_page->is_loaned() && !src_page->is_loaned()));
  dst_page->object.dirty_state = src_page->object.dirty_state;
}

void VmCowPages::InitializePageCache(uint32_t level) {
  ASSERT(level < LK_INIT_LEVEL_THREADING);

  const size_t reserve_pages = 64;
  zx::result<page_cache::PageCache> result = page_cache::PageCache::Create(reserve_pages);

  ASSERT(result.is_ok());
  page_cache_ = ktl::move(result.value());

  if (gBootOptions->pmm_alloc_random_should_wait) {
    page_cache_.SeedRandomShouldWait();
  }
}

// Initialize the cache after the percpu data structures are initialized.
LK_INIT_HOOK(vm_cow_pages_cache_init, VmCowPages::InitializePageCache, LK_INIT_LEVEL_KERNEL + 1)
