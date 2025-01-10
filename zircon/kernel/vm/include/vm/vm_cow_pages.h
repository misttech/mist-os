// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_VM_COW_PAGES_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_VM_COW_PAGES_H_

#include <assert.h>
#include <lib/page_cache.h>
#include <lib/user_copy/user_ptr.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <stdint.h>
#include <zircon/listnode.h>
#include <zircon/types.h>

#include <fbl/array.h>
#include <fbl/canary.h>
#include <fbl/enum_bits.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/macros.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <vm/compressor.h>
#include <vm/content_size_manager.h>
#include <vm/page_source.h>
#include <vm/physical_page_borrowing_config.h>
#include <vm/pmm.h>
#include <vm/vm.h>
#include <vm/vm_aspace.h>
#include <vm/vm_object.h>
#include <vm/vm_page_list.h>

// Forward declare these so VmCowPages helpers can accept references.
class BatchPQRemove;
class VmObjectPaged;
class DiscardableVmoTracker;

enum class VmCowPagesOptions : uint32_t {
  // Externally-usable flags:
  kNone = 0u,
  kUserPagerBackedRoot = (1u << 0),
  kPreservingPageContentRoot = (1u << 1),

  // With this clear, zeroing a page tries to decommit the page.  With this set, zeroing never
  // decommits the page.  Currently this is only set for contiguous VMOs.
  //
  // TODO(dustingreen): Once we're happy with the reliability of page borrowing, we should be able
  // to relax this restriction.  We may still need to flush zeroes to RAM during reclaim to mitigate
  // a hypothetical client incorrectly assuming that cache-clean status will remain intact while
  // pages aren't pinned, but that mitigation should be sufficient (even assuming such a client) to
  // allow implicit decommit when zeroing or when zero scanning, as long as no clients are doing DMA
  // to/from contiguous while not pinned.
  kCannotDecommitZeroPages = (1u << 2),

  // Internal-only flags:
  kHidden = (1u << 3),

  kInternalOnlyMask = kHidden,
};
FBL_ENABLE_ENUM_BITS(VmCowPagesOptions)

struct VmCowRange {
  uint64_t offset;
  uint64_t len;

  constexpr VmCowRange() : offset(0), len(0) {}
  constexpr VmCowRange(uint64_t offset, uint64_t len) : offset(offset), len(len) {}

  uint64_t end() const { return offset + len; }
  bool is_empty() const { return len == 0; }
  bool is_page_aligned() const { return IS_PAGE_ALIGNED(offset) && IS_PAGE_ALIGNED(len); }

  VmCowRange OffsetBy(uint64_t delta) const { return VmCowRange(offset + delta, len); }
  VmCowRange TrimedFromStart(uint64_t amount) const {
    return VmCowRange(offset + amount, len - amount);
  }
  VmCowRange WithLength(uint64_t new_length) const { return VmCowRange(offset, new_length); }
  bool IsBoundedBy(uint64_t max) const;
};

// Implements a copy-on-write hierarchy of pages in a VmPageList.
// VmCowPages have a life cycle where they start in an Init state to allow them to have
// initialization finished outside the constructor. A VmCowPages in the Init state may be
// destructed, although it is not allowed to have any pages put in it.
// Once transitioned to the Alive state the VmCowPages may generally be used, and must be
// explicitly transitioned to the Dead state prior to being destructed. The explicit transition
// ensures that a VmCowPages does not own any pages whilst in its destructor, and hence while the
// object is unreachable due to having a ref count of 0.
class VmCowPages final : public VmHierarchyBase,
                         public fbl::ContainableBaseClasses<
                             fbl::TaggedDoublyLinkedListable<VmCowPages*, internal::ChildListTag>>,
                         public VmDeferredDeleter<VmCowPages> {
 public:
  static zx_status_t Create(fbl::RefPtr<VmHierarchyState> root_lock, VmCowPagesOptions options,
                            uint32_t pmm_alloc_flags, uint64_t size,
                            ktl::unique_ptr<DiscardableVmoTracker> discardable_tracker,
                            fbl::RefPtr<VmCowPages>* cow_pages);

  static zx_status_t CreateExternal(fbl::RefPtr<PageSource> src, VmCowPagesOptions options,
                                    fbl::RefPtr<VmHierarchyState> root_lock, uint64_t size,
                                    fbl::RefPtr<VmCowPages>* cow_pages);

  // Creates a copy-on-write clone with the desired parameters. This can fail due to various
  // internal states not being correct.
  zx_status_t CreateCloneLocked(CloneType type, bool require_unidirection, VmCowRange range,
                                fbl::RefPtr<VmCowPages>* cow_child) TA_REQ(lock());

  // VmCowPages are initially created in the Init state and need to be transitioned to Alive prior
  // to being used. This is exposed for VmObjectPaged to call after ensuring that creation is
  // successful, i.e. after it can guarantee that it will transition this cow pages to Dead prior to
  // it being destroyed.
  void TransitionToAliveLocked() TA_REQ(lock());

  // Returns the size in bytes of this cow pages range. This will always be a multiple of the page
  // size.
  uint64_t size_locked() const TA_REQ(lock()) { return size_; }

  // Returns whether this cow pages node is ultimately backed by a user pager to fulfill initial
  // content, and not zero pages.  Contiguous VMOs have page_source_ set, but are not pager backed
  // in this sense.
  //
  // This should only be used to report to user mode whether a VMO is user-pager backed, not for any
  // other purpose.
  bool is_root_source_user_pager_backed_locked() const TA_REQ(lock()) {
    canary_.Assert();
    return !!(options_ & VmCowPagesOptions::kUserPagerBackedRoot);
  }

  // Helper function for CowPage cloning methods. Returns any options that should be passed down to
  // the child.
  VmCowPagesOptions inheritable_options_locked() const TA_REQ(lock()) {
    canary_.Assert();
    return VmCowPagesOptions::kNone | (options_ & (VmCowPagesOptions::kUserPagerBackedRoot |
                                                   VmCowPagesOptions::kPreservingPageContentRoot));
  }

  bool is_root_source_preserving_page_content_locked() const TA_REQ(lock()) {
    canary_.Assert();
    return !!(options_ & VmCowPagesOptions::kPreservingPageContentRoot);
  }

  bool is_parent_hidden_locked() const TA_REQ(lock()) {
    return parent_ && parent_locked().is_hidden_locked();
  }

  bool is_discardable() const { return !!discardable_tracker_; }

  bool can_evict() const {
    canary_.Assert();
    return page_source_ && page_source_->properties().is_preserving_page_content;
  }

  bool can_root_source_evict_locked() const TA_REQ(lock()) {
    bool result = is_root_source_preserving_page_content_locked();
    DEBUG_ASSERT(result == is_root_source_user_pager_backed_locked());
    return result;
  }

  // Returns whether this cow pages node is dirty tracked.
  bool is_dirty_tracked_locked() const TA_REQ(lock()) {
    canary_.Assert();
    // Pager-backed VMOs require dirty tracking either if they are directly backed by the pager,
    // i.e. the root VMO.
    return page_source_ && page_source_->properties().is_preserving_page_content;
  }

  // The modified state is only supported for root pager-backed VMOs, and will get queried (and
  // possibly reset) on the next QueryPagerVmoStatsLocked() call. Although the modified state is
  // only tracked for the root VMO.
  void mark_modified_locked() TA_REQ(lock()) {
    if (!is_dirty_tracked_locked()) {
      return;
    }
    DEBUG_ASSERT(is_source_preserving_page_content());
    pager_stats_modified_ = true;
  }

  bool is_high_memory_priority_locked() const TA_REQ(lock()) {
    DEBUG_ASSERT(high_priority_count_ >= 0);
    return high_priority_count_ != 0;
  }

  // See description on |pinned_page_count_| for meaning.
  uint64_t pinned_page_count_locked() const TA_REQ(lock()) { return pinned_page_count_; }

  // Sets the VmObjectPaged backlink for this copy-on-write node.
  // Currently it is assumed that all nodes always have backlinks with the 1:1 hierarchy mapping,
  // unless this is a hidden node.
  void set_paged_backlink_locked(VmObjectPaged* ref) TA_REQ(lock()) { paged_ref_ = ref; }

  VmObjectPaged* get_paged_backlink_locked() const TA_REQ(lock()) { return paged_ref_; }

  uint64_t HeapAllocationBytesLocked() const TA_REQ(lock()) {
    return page_list_.HeapAllocationBytes();
  }

  uint64_t ReclamationEventCountLocked() const TA_REQ(lock()) { return reclamation_event_count_; }

  void DetachSourceLocked() TA_REQ(lock());

  // Resizes the range of this cow pages. |size| must be a multiple of the page size.
  zx_status_t ResizeLocked(uint64_t size) TA_REQ(lock());

  // See VmObject::Lookup
  zx_status_t LookupLocked(VmCowRange range, VmObject::LookupFunction lookup_fn) TA_REQ(lock());

  // Similar to LookupLocked, but enumerate all readable pages in the hierarchy within the requested
  // range. The offset passed to the |lookup_fn| is the offset this page is visible at in this
  // object, even if the page itself is committed in a parent object. The physical addresses given
  // to the lookup_fn should not be retained in any way unless the range has also been pinned by the
  // caller.
  // Ranges of length zero are considered invalid and will return ZX_ERR_INVALID_ARGS. The lookup_fn
  // can terminate iteration early by returning ZX_ERR_STOP.
  using LookupReadableFunction =
      fit::inline_function<zx_status_t(uint64_t offset, paddr_t pa), 4 * sizeof(void*)>;
  zx_status_t LookupReadableLocked(VmCowRange range, LookupReadableFunction lookup_fn)
      TA_REQ(lock());

  // See VmObject::TakePages
  //
  // May return ZX_ERR_SHOULD_WAIT if the |page_request| is filled out and needs waiting on. In this
  // case |taken_len| might be populated with a value less than |len|.
  //
  // |taken_len| is always filled with the amount of |len| that has been processed to allow for
  // gradual progress of calls. Will always be equal to |len| if ZX_OK is returned.
  zx_status_t TakePagesLocked(VmCowRange range, VmPageSpliceList* pages, uint64_t* taken_len,
                              MultiPageRequest* page_request) TA_REQ(lock());

  // See VmObject::SupplyPages
  //
  // May return ZX_ERR_SHOULD_WAIT if the |page_request| is filled out and needs waiting on. In this
  // case |supplied_len| might be populated with a value less than |len|.
  //
  // If ZX_OK is returned then |supplied_len| will always be equal to |len|. For any other error
  // code the value of |supplied_len| is undefined.
  zx_status_t SupplyPagesLocked(VmCowRange range, VmPageSpliceList* pages, SupplyOptions options,
                                uint64_t* supplied_len, MultiPageRequest* page_request)
      TA_REQ(lock());

  zx_status_t SupplyPages(VmCowRange range, VmPageSpliceList* pages, SupplyOptions options,
                          uint64_t* supplied_len, MultiPageRequest* page_request) TA_EXCL(lock());

  // See VmObject::FailPageRequests
  zx_status_t FailPageRequestsLocked(VmCowRange range, zx_status_t error_status) TA_REQ(lock());

  // Used to track dirty_state in the vm_page_t.
  //
  // The transitions between the three states can roughly be summarized as follows:
  // 1. A page starts off as Clean when supplied.
  // 2. A write transitions the page from Clean to Dirty.
  // 3. A writeback_begin moves the Dirty page to AwaitingClean.
  // 4. A writeback_end moves the AwaitingClean page to Clean.
  // 5. A write that comes in while the writeback is in progress (i.e. the page is AwaitingClean)
  // moves the AwaitingClean page back to Dirty.
  enum class DirtyState : uint8_t {
    // The page does not track dirty state. Used for non pager backed pages.
    Untracked = 0,
    // The page is clean, i.e. its contents have not been altered from when the page was supplied.
    Clean,
    // The page's contents have been modified from the time of supply, and should be written back to
    // the page source at some point.
    Dirty,
    // The page still has modified contents, but the page source is in the process of writing back
    // the changes. This is used to ensure that a consistent version is written back, and that any
    // new modifications that happen during the writeback are not lost. The page source will mark
    // pages AwaitingClean before starting any writeback.
    AwaitingClean,
    NumStates,
  };
  // Make sure that the state can be encoded in the vm_page_t's dirty_state field.
  static_assert(static_cast<uint8_t>(DirtyState::NumStates) <= VM_PAGE_OBJECT_MAX_DIRTY_STATES);

  static bool is_page_dirty_tracked(const vm_page_t* page) {
    return DirtyState(page->object.dirty_state) != DirtyState::Untracked;
  }
  static bool is_page_dirty(const vm_page_t* page) {
    return DirtyState(page->object.dirty_state) == DirtyState::Dirty;
  }
  static bool is_page_clean(const vm_page_t* page) {
    return DirtyState(page->object.dirty_state) == DirtyState::Clean;
  }
  static bool is_page_awaiting_clean(const vm_page_t* page) {
    return DirtyState(page->object.dirty_state) == DirtyState::AwaitingClean;
  }

  // See VmObject::DirtyPages. |page_request| is required to support delayed PMM allocations; if
  // ZX_ERR_SHOULD_WAIT is returned the caller should wait on |page_request|. |alloc_list| will hold
  // any pages that were allocated but not used in case of delayed PMM allocations, so that it can
  // be reused across multiple successive calls whilst ensuring forward progress.
  zx_status_t DirtyPagesLocked(VmCowRange range, list_node_t* alloc_list,
                               AnonymousPageRequest* page_request) TA_REQ(lock());

  using DirtyRangeEnumerateFunction = VmObject::DirtyRangeEnumerateFunction;
  // See VmObject::EnumerateDirtyRanges
  zx_status_t EnumerateDirtyRangesLocked(VmCowRange range,
                                         DirtyRangeEnumerateFunction&& dirty_range_fn)
      TA_REQ(lock());

  // Query pager VMO |stats|, and reset them too if |reset| is set to true.
  zx_status_t QueryPagerVmoStatsLocked(bool reset, zx_pager_vmo_stats_t* stats) TA_REQ(lock()) {
    canary_.Assert();
    DEBUG_ASSERT(stats);
    // The modified state should only be set for VMOs directly backed by a pager.
    DEBUG_ASSERT(!pager_stats_modified_ || is_source_preserving_page_content());

    if (!is_source_preserving_page_content()) {
      return ZX_ERR_NOT_SUPPORTED;
    }
    stats->modified = pager_stats_modified_ ? ZX_PAGER_VMO_STATS_MODIFIED : 0;
    if (reset) {
      ResetPagerVmoStatsLocked();
    }
    return ZX_OK;
  }

  void ResetPagerVmoStatsLocked() TA_REQ(lock()) { pager_stats_modified_ = false; }

  // See VmObject::WritebackBegin
  zx_status_t WritebackBeginLocked(VmCowRange range, bool is_zero_range) TA_REQ(lock());

  // See VmObject::WritebackEnd
  zx_status_t WritebackEndLocked(VmCowRange range) TA_REQ(lock());

  // Tries to prepare the range [offset, offset + len) for writing by marking pages dirty or
  // verifying that they are already dirty. It is possible for only some or none of the pages in the
  // range to be dirtied at the end of this call. |dirty_len_out| will return the (page-aligned)
  // length starting at |offset| that contains dirty pages, either already dirty before making the
  // call or dirtied during the call. In other words, the range [offset, offset + dirty_len_out)
  // will be dirty when this call returns, i.e. prepared for the write to proceed, where
  // |dirty_len_out| <= |len|.
  //
  // If the specified range starts with pages that are not already dirty and need to request the
  // page source before transitioning to dirty, a DIRTY page request will be forwarded to the page
  // source. In this case |dirty_len_out| will be set to 0, ZX_ERR_SHOULD_WAIT will be returned and
  // the caller should wait on |page_request|. If no page requests need to be generated, i.e. we
  // could find some pages that are already dirty at the start of the range, or if the VMO does not
  // require dirty transitions to be trapped, ZX_OK is returned.
  //
  // |offset| and |len| should be page-aligned.
  zx_status_t PrepareForWriteLocked(VmCowRange range, LazyPageRequest* page_request,
                                    uint64_t* dirty_len_out) TA_REQ(lock());

  class LookupCursor;
  // See VmObjectPaged::GetLookupCursorLocked
  zx::result<LookupCursor> GetLookupCursorLocked(VmCowRange range) TA_REQ(lock());

  // Controls the type of content that can be overwritten by the Add[New]Page[s]Locked functions.
  enum class CanOverwriteContent : uint8_t {
    // Do not overwrite any kind of content, i.e. only add a page at the slot if there is true
    // absence of content.
    None,
    // Only overwrite slots that represent zeros. In the case of anonymous VMOs, both gaps and zero
    // page markers represent zeros, as the entire VMO is implicitly zero on creation. For pager
    // backed VMOs, zero page markers and zero intervals represent zeros.
    Zero,
    // Overwrite any slots, regardless of the type of content.
    NonZero,
  };
  // Adds an allocated page to this cow pages at the specified offset, can be optionally zeroed and
  // any mappings invalidated. If an error is returned the caller retains ownership of |page|.
  // Offset must be page aligned.
  //
  // |overwrite| controls how the function handles pre-existing content at |offset|. If |overwrite|
  // does not permit replacing the content, ZX_ERR_ALREADY_EXISTS will be returned. If a page is
  // released from the page list as a result of overwriting, it is returned through |released_page|
  // and the caller takes ownership of this page. If the |overwrite| action is such that a page
  // cannot be released, it is valid for the caller to pass in nullptr for |released_page|.
  zx_status_t AddNewPageLocked(uint64_t offset, vm_page_t* page, CanOverwriteContent overwrite,
                               VmPageOrMarker* released_page, bool zero = true,
                               bool do_range_update = true) TA_REQ(lock());

  // Adds a set of pages consecutively starting from the given offset. Regardless of the return
  // result ownership of the pages is taken. Pages are assumed to be in the ALLOC state and can be
  // optionally zeroed before inserting. start_offset must be page aligned.
  //
  // |overwrite| controls how the function handles pre-existing content in the range, however it is
  // not valid to specify the |CanOverwriteContent::NonZero| option, as any pages that would get
  // released as a consequence cannot be returned.
  zx_status_t AddNewPagesLocked(uint64_t start_offset, list_node_t* pages,
                                CanOverwriteContent overwrite, bool zero = true,
                                bool do_range_update = true) TA_REQ(lock());

  // Attempts to release pages in the pages list causing the range to become copy-on-write again.
  // For consistency if there is a parent or a backing page source, such that the range would not
  // explicitly copy-on-write the zero page then this will fail. Use ZeroPagesLocked for an
  // operation that is guaranteed to succeed, but may not release memory.
  zx_status_t DecommitRangeLocked(VmCowRange range) TA_REQ(lock());

  // After successful completion the range of pages will all read as zeros. The mechanism used to
  // achieve this is not guaranteed to decommit, but it will try to.
  // |page_start_base| and |page_end_base| must be page aligned offsets within the range of the
  // object. |dirty_track| specifies whether the range being zeroed subscribes to dirty tracking, if
  // |true| the range will start out as dirty. |dirty_track| only has meaning if the VMO supports
  // dirty tracking, otherwise it is ignored. |zeroed_len_out| will contain the length (in bytes)
  // starting at |page_start_base| that was successfully zeroed.
  //
  // Returns one of the following:
  //  ZX_OK => The whole range was successfully zeroed.
  //  ZX_ERR_SHOULD_WAIT => The caller needs to wait on the |page_request| and then retry the
  //  operation. |zeroed_len_out| will contain the range that was partially zeroed, so the caller
  //  can advance the start offset before retrying.
  //  Any other error code indicates a failure to zero a part of the range or the whole range.
  zx_status_t ZeroPagesLocked(uint64_t page_start_base, uint64_t page_end_base, bool dirty_track,
                              MultiPageRequest* page_request, uint64_t* zeroed_len_out)
      TA_REQ(lock());

  // Attempts to commit a range of pages. This has three kinds of return status
  //  ZX_OK => The whole range was successfully committed and |len| will be written to
  //           |committed_len|
  //  ZX_ERR_SHOULD_WAIT => A partial (potentially 0) range was committed (output in |committed_len|
  //                        and the passed in |page_request| should be waited on before retrying
  //                        the commit operation. The portion that was successfully committed does
  //                        not need to retried.
  //  * => Any other error, the number of pages committed is undefined.
  // The |offset| and |len| are assumed to be page aligned and within the range of |size_|.
  zx_status_t CommitRangeLocked(VmCowRange range, uint64_t* committed_len,
                                MultiPageRequest* page_request) TA_REQ(lock());

  // Increases the pin count of the range of pages given by |offset| and |len|. The full range must
  // already be committed and this either pins all pages in the range, or pins no pages and returns
  // an error. The caller can assume that on success len / PAGE_SIZE pages were pinned.
  // The |offset| and |len| are assumed to be page aligned and within the range of |size_|.
  // All pages in the specified range are assumed to be non-loaned pages, so the caller is expected
  // to replace any loaned pages beforehand if required.
  zx_status_t PinRangeLocked(VmCowRange range) TA_REQ(lock());

  // See VmObject::Unpin
  void UnpinLocked(VmCowRange range) TA_REQ(lock());

  // See VmObject::DebugIsRangePinned
  bool DebugIsRangePinnedLocked(VmCowRange range) TA_REQ(lock());

  // Returns true if a page is not currently committed, and if the offset were to be read from, it
  // would be read as zero. Requested offset must be page aligned and within range.
  bool PageWouldReadZeroLocked(uint64_t page_offset) TA_REQ(lock());

  // see VmObject::GetAttributedMemoryInRange
  using AttributionCounts = VmObject::AttributionCounts;
  AttributionCounts GetAttributedMemoryInRangeLocked(VmCowRange range) const TA_REQ(lock());

  enum class EvictionHintAction : uint8_t {
    Follow,
    Ignore,
  };

  // Asks the VMO to attempt to reclaim the specified page. There are a few possible outcomes:
  // 1. Exactly this page is reclaimed.
  // 2. This page and other pages are reclaimed.
  // 3. Just other pages are reclaimed.
  // 4. No pages are reclaimed.
  // Pages other than the one requested may get reclaimed due to any internal relationships between
  // pages that make it meaningless or difficult to reclaim just the single page in question.
  // In the cases of (3) and (4) there are some guarantees provided:
  // 1. If the |page| was not from this VMO (or not at the specified offset) then nothing about the
  //    |page| or this VMO will be modified.
  // 2. If the |page| is from this VMO and offset (and was not reclaimed) then the page will have
  //    been removed from any candidate reclamation lists (such as the DontNeed pager backed list).
  // The effect of (2) is that the caller can assume in the case of reclamation failure it will not
  // keep finding this page as a reclamation candidate and infinitely retry it.
  // If the |compressor| is non-null then it must have just had |Arm| called on it.
  // |hint_action| indicates whether the |always_need| eviction hint should be respected or ignored.
  //
  // The actual number of pages reclaimed is returned.
  struct ReclaimCounts {
    uint64_t evicted_non_loaned = 0;
    uint64_t evicted_loaned = 0;
    uint64_t discarded = 0;
    uint64_t compressed = 0;

    uint64_t Total() const { return compressed + discarded + evicted_non_loaned + evicted_loaned; }
  };
  ReclaimCounts ReclaimPage(vm_page_t* page, uint64_t offset, EvictionHintAction hint_action,
                            VmCompressor* compressor);

  // If any pages in the specified range are loaned pages, replaces them with non-loaned pages
  // (which requires providing a |page_request|). The specified range should be fully committed
  // before calling this function. If a gap or a marker is encountered, or a loaned page cannot be
  // replaced, returns early with ZX_ERR_BAD_STATE. If the replacement needs to wait on the PMM for
  // allocation, returns ZX_ERR_SHOULD_WAIT, and the caller should wait on the |page_request|.
  // |non_loaned_len| is set to the length (starting at |offset|) that contains only non-loaned
  // pages. |offset| and |len| must be page-aligned.
  zx_status_t ReplacePagesWithNonLoanedLocked(VmCowRange range, AnonymousPageRequest* page_request,
                                              uint64_t* non_loaned_len) TA_REQ(lock());

  // If page is still at offset, replace it with a loaned page.
  zx_status_t ReplacePageWithLoaned(vm_page_t* before_page, uint64_t offset) TA_EXCL(lock());

  // Attempts to dedup the given page at the specified offset with the zero page. The only
  // correctness requirement for this is that `page` must be *some* valid vm_page_t, meaning that
  // all race conditions are handled internally. This function returns false if
  //  * page is either not from this VMO, or not found at the specified offset
  //  * page is pinned
  //  * vmo is uncached
  //  * page is not all zeroes
  // Otherwise 'true' is returned and the page will have been returned to the pmm with a zero page
  // marker put in its place.
  bool DedupZeroPage(vm_page_t* page, uint64_t offset);

  void DumpLocked(uint depth, bool verbose) const TA_REQ(lock());

  // see VmObject::DebugLookupDepth
  uint32_t DebugLookupDepthLocked() const TA_REQ(lock());

  // VMO_VALIDATION
  bool DebugValidatePageSharingLocked() const TA_REQ(lock());
  bool DebugValidateBacklinksLocked() const TA_REQ(lock());
  // Calls DebugValidatePageSharesLocked on this and every parent in the chain, returning true if
  // all return true. Also calls DebugValidateBacklinksLocked on every node in the hierarchy.
  bool DebugValidateHierarchyLocked() const TA_REQ(lock());
  bool DebugValidateZeroIntervalsLocked() const TA_REQ(lock());

  // Walks all the descendants in a preorder traversal. Stops if func returns anything other than
  // ZX_OK.
  template <typename T>
  zx_status_t DebugForEachDescendant(T func) const TA_REQ(lock()) {
    const VmCowPages* stop = parent_.get();
    int depth = 0;
    const VmCowPages* cur = this;
    const VmCowPages* prev = nullptr;
    while (cur != stop) {
      AssertHeld(cur->lock_ref());
      uint32_t children = cur->children_list_len_;
      if (!prev || prev == cur->parent_.get()) {
        // Visit cur
        zx_status_t s = func(cur, static_cast<uint>(depth));
        if (s != ZX_OK) {
          return s;
        }

        if (!children) {
          // no children; move to parent (or nullptr)
          prev = cur;
          cur = cur->parent_.get();
          continue;
        } else {
          // move to first child
          prev = cur;
          cur = &cur->children_list_.front();
          ++depth;
          continue;
        }
      }
      // At this point we know we came up from a child, not down from the parent.
      DEBUG_ASSERT(prev && prev != cur->parent_.get());
      // The children are linked together, so we can move from one child to the next.

      auto iterator = cur->children_list_.make_iterator(*prev);
      ++iterator;
      if (iterator == cur->children_list_.end()) {
        // no more children; move back to parent
        prev = cur;
        cur = cur->parent_.get();
        --depth;
        continue;
      }

      // descend to next child
      prev = cur;
      cur = &(*iterator);
      DEBUG_ASSERT(cur);
    }
    return ZX_OK;
  }

  // VMO_FRUGAL_VALIDATION
  bool DebugValidateVmoPageBorrowingLocked() const TA_REQ(lock());

  // Different operations that RangeChangeUpdate* can perform against any VmMappings that are found.
  enum class RangeChangeOp {
    Unmap,
    // Specialized case of Unmap where the caller is stating that it knows that any pages that might
    // need to be unmapped are all read instances of the shared zero page.
    UnmapZeroPage,
    RemoveWrite,
    // Unpin is not a 'real' operation in that it does not cause any actions, and is simply used as
    // a mechanism to allow the VmCowPages to trigger a search for any kernel mappings that are
    // still referencing an unpinned page.
    DebugUnpin,
  };
  // Apply the specified operation to all mappings in the given range. This is applied to all
  // descendants within the range.
  void RangeChangeUpdateLocked(VmCowRange range, RangeChangeOp op) TA_REQ(lock());

  // Promote pages in the specified range for reclamation under memory pressure. |offset| will be
  // rounded down to the page boundary, and |len| will be rounded up to the page boundary.
  // Currently used only for pager-backed VMOs to move their pages to the end of the
  // pager-backed queue, so that they can be evicted first.
  void PromoteRangeForReclamationLocked(VmCowRange range) TA_REQ(lock());

  // Protect pages in the specified range from reclamation under memory pressure. |offset| will be
  // rounded down to the page boundary, and |len| will be rounded up to the page boundary. Any
  // absent pages in the range will first be committed, and the call will block on the fulfillment
  // of the page request(s), dropping |guard| while waiting (multiple times if multiple pages need
  // to be supplied), and then, if |set_always_need| is true, the |always_need| flag in the pages
  // will be set.
  // If the |ignore_errors| flag is set then any per page errors will be ignored and future pages in
  // the range will still be operated on. If this flag is not set then any kind of error causes an
  // immediate abort.
  zx_status_t ProtectRangeFromReclamationLocked(VmCowRange range, bool set_always_need,
                                                bool ignore_errors, Guard<CriticalMutex>* guard)
      TA_REQ(lock());

  // Ensures any pages in the specified range are not compressed, but does not otherwise commit any
  // pages. In order to handle delayed memory allocations, |guard| may be dropped one or more times.
  zx_status_t DecompressInRangeLocked(VmCowRange range, Guard<CriticalMutex>* guard) TA_REQ(lock());

  // See VmObject::ChangeHighPriorityCountLocked
  void ChangeHighPriorityCountLocked(int64_t delta) TA_REQ(lock());

  zx_status_t LockRangeLocked(VmCowRange range, zx_vmo_lock_state_t* lock_state_out);
  zx_status_t TryLockRangeLocked(VmCowRange range);
  zx_status_t UnlockRangeLocked(VmCowRange range);

  uint64_t DebugGetPageCountLocked() const TA_REQ(lock());
  bool DebugIsPage(uint64_t offset) const;
  bool DebugIsMarker(uint64_t offset) const;
  bool DebugIsEmpty(uint64_t offset) const;
  vm_page_t* DebugGetPage(uint64_t offset) const TA_EXCL(lock());
  vm_page_t* DebugGetPageLocked(uint64_t offset) const TA_REQ(lock());

  // Exposed for testing.
  DiscardableVmoTracker* DebugGetDiscardableTracker() const { return discardable_tracker_.get(); }

  bool DebugIsHighMemoryPriority() const TA_EXCL(lock());

  // Discard all the pages from a discardable vmo in the |kReclaimable| state. If successful, the
  // |discardable_state_| is set to |kDiscarded|. Returns the number of pages discarded.
  uint64_t DiscardPages() TA_EXCL(lock());

  // See DiscardableVmoTracker::DebugDiscardablePageCounts().
  struct DiscardablePageCounts {
    uint64_t locked;
    uint64_t unlocked;
  };
  DiscardablePageCounts DebugGetDiscardablePageCounts() const TA_EXCL(lock());

  // Returns the parent of this cow pages, may be null. Generally the parent should never be
  // directly accessed externally, but this exposed specifically for tests.
  fbl::RefPtr<VmCowPages> DebugGetParent();

  // Initializes the PageCache instance for COW page allocations.
  static void InitializePageCache(uint32_t level);

  // Unlocked wrapper around ReplacePageLocked, exposed for the physical page provider to cancel
  // loans with.
  zx_status_t ReplacePage(vm_page_t* before_page, uint64_t offset, bool with_loaned,
                          vm_page_t** after_page, AnonymousPageRequest* page_request)
      TA_EXCL(lock()) {
    Guard<CriticalMutex> guard{lock()};
    return ReplacePageLocked(before_page, offset, with_loaned, after_page, page_request);
  }

  // Eviction wrapper, unlike ReclaimPage this wrapper can assume it just needs to evict, and has no
  // requirements on updating any reclamation lists. Exposed for the physical page provider to
  // reclaim loaned pages.
  void ReclaimPageForEviction(vm_page_t* page, uint64_t offset);

  // Potentially transitions from Alive->Dead if the cow pages is unreachable (i.e. has no
  // paged_ref_ and no children). Used by the VmObjectPaged when it unlinks the paged_ref_, but
  // prior to dropping the RefPtr, giving the VmCowPages a chance to transition.
  void MaybeDeadTransitionLocked(Guard<CriticalMutex>& guard) TA_REQ(lock());

  // Unlocked helper around MaybeDeadTransitionLocked
  void MaybeDeadTransition();

  // Helper to allocate a new page for the VMO, filling out the page request if necessary.
  zx_status_t AllocPage(vm_page_t** page, AnonymousPageRequest* page_request);

  // Helper to free |pages| to the PMM. |freeing_owned_pages| is set to true to indicate that this
  // object had ownership of |pages|. This could either be true ownership, where the |pages| have
  // been removed from this object's page list, or logical ownership, e.g. when a source page list
  // has been handed over to SupplyPagesLocked(). If |freeing_owned_pages| is true, this function
  // will also try to invoke FreePages() on the backing page source if it supports it.
  //
  // Callers should avoid calling pmm_free() directly from inside VmCowPages, and instead should use
  // this helper.
  void FreePagesLocked(list_node* pages, bool freeing_owned_pages) TA_REQ(lock()) {
    if (!freeing_owned_pages || !is_source_handling_free_locked()) {
      CacheFree(pages);
      return;
    }
    page_source_->FreePages(pages);
  }

  // Helper to free |page| to the PMM. |freeing_owned_page| is set to true to indicate that this
  // object had ownership of |page|. This could either be true ownership, where the |page| has
  // been removed from this object's page list, or logical ownership, e.g. when a source page list
  // has been handed over to SupplyPagesLocked(). If |freeing_owned_pages| is true, this function
  // will also try to invoke FreePages() on the backing page source if it supports it.
  //
  // Callers should avoid calling pmm_free_page() directly from inside VmCowPages, and instead
  // should use this helper.
  void FreePageLocked(vm_page_t* page, bool freeing_owned_page) TA_REQ(lock()) {
    DEBUG_ASSERT(!list_in_list(&page->queue_node));
    if (!freeing_owned_page || !is_source_handling_free_locked()) {
      CacheFree(page);
      return;
    }
    list_node_t list;
    list_initialize(&list);
    list_add_tail(&list, &page->queue_node);
    page_source_->FreePages(&list);
  }

 private:
  // private constructor (use Create...())
  VmCowPages(fbl::RefPtr<VmHierarchyState> root_lock, VmCowPagesOptions options,
             uint32_t pmm_alloc_flags, uint64_t size, fbl::RefPtr<PageSource> page_source,
             ktl::unique_ptr<DiscardableVmoTracker> discardable_tracker);

  ~VmCowPages() override;

  // A private helper that takes pages if this VmCowPages has a parent.
  zx_status_t TakePagesWithParentLocked(VmCowRange range, VmPageSpliceList* pages,
                                        uint64_t* taken_len, MultiPageRequest* page_request)
      TA_REQ(lock());

  friend class fbl::RefPtr<VmCowPages>;

  DISALLOW_COPY_ASSIGN_AND_MOVE(VmCowPages);

  // Transitions from Alive->Dead, freeing pages and cleaning up state. Responsibility of the caller
  // to validate that it is correct to be doing this transition. May drop the lock during its
  // execution.
  void DeadTransition(Guard<CriticalMutex>& guard) TA_REQ(lock());

  bool is_hidden_locked() const TA_REQ(lock()) { return !!(options_ & VmCowPagesOptions::kHidden); }
  bool can_decommit_zero_pages_locked() const TA_REQ(lock()) {
    return !(options_ & VmCowPagesOptions::kCannotDecommitZeroPages);
  }

  // can_borrow_locked() returns true if the VmCowPages is capable of borrowing pages, but whether
  // the VmCowPages should actually borrow pages also depends on a borrowing-site-specific flag that
  // the caller is responsible for checking (in addition to checking can_borrow_locked()).  Only if
  // both are true should the caller actually borrow at the caller's specific potential borrowing
  // site.  For example, see is_borrowing_in_supplypages_enabled() and
  // is_borrowing_on_mru_enabled().
  bool can_borrow_locked() const TA_REQ(lock()) {
    // TODO(dustingreen): Or rashaeqbal@.  We can only borrow while the page is not dirty.
    // Currently we enforce this by checking ShouldTrapDirtyTransitions() below and leaning on the
    // fact that !ShouldTrapDirtyTransitions() dirtying isn't implemented yet.  We currently evict
    // to reclaim instead of replacing the page, and we can't evict a dirty page since the contents
    // would be lost.  Option 1: When a loaned page is about to become dirty, we could replace it
    // with a non-loaned page.  Option 2: When reclaiming a loaned page we could replace instead of
    // evicting (this may be simpler).

    // Currently there needs to be a page source for any borrowing to be possible, due to
    // requirements of a backlink and other assumptions in the VMO code. Returning early here in the
    // absence of a page source simplifies the rest of the logic.
    if (!page_source_) {
      return false;
    }

    bool source_is_suitable = page_source_->properties().is_preserving_page_content;

    // This ensures that if borrowing is globally disabled (no borrowing sites enabled), that we'll
    // return false.  We could delete this bool without damaging correctness, but we want to
    // mitigate a call site that maybe fails to check its call-site-specific settings such as
    // is_borrowing_in_supplypages_enabled().
    //
    // We also don't technically need to check is_any_borrowing_enabled() here since pmm will check
    // also, but by checking here, we minimize the amount of code that will run when
    // !is_any_borrowing_enabled() (in case we have it disabled due to late discovery of a problem
    // with borrowing).
    bool borrowing_is_generally_acceptable =
        pmm_physical_page_borrowing_config()->is_any_borrowing_enabled();
    // Exclude is_latency_sensitive_ to avoid adding latency due to reclaim.
    //
    // Currently we evict instead of replacing a page when reclaiming, so we want to avoid evicting
    // pages that are latency sensitive or are fairly likely to be pinned at some point.
    //
    // We also don't want to borrow a page that might get pinned again since we want to mitigate the
    // possibility of an invalid DMA-after-free.
    bool excluded_from_borrowing_for_latency_reasons = high_priority_count_ != 0 || ever_pinned_;
    // Avoid borrowing and trapping dirty transitions overlapping for now; nothing really stops
    // these from being compatible AFAICT - we're just avoiding overlap of these two things until
    // later.
    bool overlapping_with_other_features = page_source_->ShouldTrapDirtyTransitions();

    return source_is_suitable && borrowing_is_generally_acceptable &&
           !excluded_from_borrowing_for_latency_reasons && !overlapping_with_other_features;
  }

  bool direct_source_supplies_zero_pages() const {
    return page_source_ && !page_source_->properties().is_preserving_page_content;
  }

  bool can_decommit() const {
    return !page_source_ || !page_source_->properties().is_preserving_page_content;
  }

  bool is_cow_clonable_locked() const TA_REQ(lock()) {
    // Copy-on-write clones of pager vmos or their descendants aren't supported as we can't
    // efficiently make an immutable snapshot.
    if (can_root_source_evict_locked()) {
      return false;
    }

    // We also don't support COW clones for contiguous VMOs.
    if (is_source_supplying_specific_physical_pages()) {
      return false;
    }

    return true;
  }

  bool is_snapshot_at_least_on_write_supported() const TA_REQ(lock()) {
    canary_.Assert();

    if (is_parent_hidden_locked()) {
      return false;
    }

    bool result = is_root_source_preserving_page_content_locked();
    DEBUG_ASSERT(result == is_root_source_user_pager_backed_locked());

    return result;
  }

  bool can_snapshot_modified_locked() const TA_REQ(lock()) {
    // Root must be pager-backed.
    if (!is_root_source_user_pager_backed_locked()) {
      return false;
    }

    // We don't support COW clones for contiguous VMOs.
    if (is_source_supplying_specific_physical_pages()) {
      return false;
    }

    // Unless we are the root VMO, we can't snapshot if has children, as it would create an
    // inconsistent hierarchy.
    if (!parent_) {
      return true;
    }
    if (children_list_len_ != 0) {
      return false;
    }

    // Snapshot-modified is currently unsupported for at-least-on-write VMO chains of length >2.
    AssertHeld(parent_->lock_ref());
    if (parent_->parent_ && !is_parent_hidden_locked()) {
      return false;
    }

    return true;
  }

  bool is_source_preserving_page_content() const {
    return page_source_ && page_source_->properties().is_preserving_page_content;
  }

  bool is_source_supplying_specific_physical_pages() const {
    return page_source_ && page_source_->properties().is_providing_specific_physical_pages;
  }

  // See |ForEveryOwnedHierarchyPageInRange|. Each entry given to `T` is constant.
  template <typename T>
  zx_status_t ForEveryOwnedHierarchyPageInRangeLocked(T func, uint64_t offset, uint64_t size) const
      TA_REQ(lock());

  // See |ForEveryOwnedHierarchyPageInRange|. Each entry given to `T` is mutable and `T` may modify
  // it or replace it with an empty entry.
  template <typename T>
  zx_status_t ForEveryOwnedMutableHierarchyPageInRangeLocked(T func, uint64_t offset, uint64_t size)
      TA_REQ(lock());

  // Iterates a range within a visible node, invoking a callback for every `VmPageListEntry` the
  // node owns (fully or partially) in that range.
  //
  // The callback is invoked at most once per each offset within the range, as the node can own at
  // most one entry at each offset. Either:
  //  * The node directly contains the first visible entry at the offset and thus fully owns it.
  //  * A hidden parent contains the first visible entry at the offset and thus the node partially
  //    owns it.
  //  * A visible parent contains the first visible entry at the offset and thus that parent fully
  //    owns it. This method doesn't evaluate such parents and skips the offset.
  //  * There is no visible entry at the offset. This method skips the offset.
  //
  // Prefer using the non-static methods above over invoking this function directly.
  //
  // The caller provides:
  //  * `self`: Node to begin the iteration from. It must be a visible node.
  //  * `func`: Callback function invoked for each non-empty entry.
  //  * `offset`: Offset relative to `self` to begin iterating at.
  //  * `size`: Size of the range to iterate.
  //
  // The type `S` must be implicitly convertible to a `VmCowPages` or a `const VmCowPages`.
  // The type `P` is `const VmPageOrMarker` if `S` is const, otherwise it is `VmPageOrMarker`.
  // The type `T` must be:
  // `zx_status_t(P* p, const VmCowPages* owner, uint64_t self_offset, uint64_t owner_offset)`
  //  The return value controls whether iteration continues:
  //   * `ZX_ERR_NEXT`: Continue iteration or stop with `ZX_OK` if no more entries to iterate.
  //   * `ZX_ERR_STOP`: Stop iteration immediately with `ZX_OK`.
  //   * Any other code: Stop iteration immediately with that error code.
  //
  // Returns `ZX_OK` if no `func` invocations returned any errors, otherwise returns the error code
  // from the failing `func` invocation.
  template <typename S, typename T>
  static zx_status_t ForEveryOwnedHierarchyPageInRange(S* self, T func, uint64_t offset,
                                                       uint64_t size) TA_REQ(self->lock());

  // Walks up the parent tree and returns the root, or |this| if there is no parent.
  const VmCowPages* GetRootLocked() const TA_REQ(lock());

  // Changes a Reference in the provided VmPageOrMarker into a real vm_page_t. The allocated page
  // is assumed to be for this VmCowPages, and so uses the pmm_alloc_flags_, but it is not assumed
  // that the page_or_mark is actually yet in this page_list_, and so the allocated page is not
  // added to the page queues. It is the responsibility of the caller to add to the page queues if
  // the page_or_mark is not stack owned.
  // The |page_request| must be non-null if the |pmm_alloc_flags_| allow for delayed allocation, in
  // which case this may return ZX_ERR_SHOULD_WAIT if the page_request is filled out.
  zx_status_t MakePageFromReference(VmPageOrMarkerRef page_or_mark,
                                    AnonymousPageRequest* page_request);

  // Replaces the Reference in VmPageOrMarker owned by this page_list_ for a real vm_page_t.
  // Unlike MakePageFromReference this updates the page queues to track the newly added page. Use
  // of |page_request| and implications on return value are the same as |MakePageFromReference|.
  zx_status_t ReplaceReferenceWithPageLocked(VmPageOrMarkerRef page_or_mark, uint64_t offset,
                                             AnonymousPageRequest* page_request) TA_REQ(lock());

  zx_status_t AllocateCopyPage(paddr_t parent_paddr, list_node_t* alloc_list,
                               AnonymousPageRequest* request, vm_page_t** clone);

  static zx_status_t CacheAllocPage(uint alloc_flags, vm_page_t** p, paddr_t* pa);
  static void CacheFree(list_node_t* list);
  static void CacheFree(vm_page_t* p);

  // Helper for allocating and initializing a loaned page.
  template <typename F>
  zx::result<vm_page_t*> AllocLoanedPage(F allocated);

  // Helper for allocating a page for this VMO. The allocated page is not yet initialized, and
  // InitializeVmPage must be called on it prior to use. Callers most likely want AllocPage instead,
  // with this method being useful in rare cases where you want to defer initialization till
  // AddNewPagesLocked or similar.
  zx_status_t AllocUninitializedPage(vm_page_t** page, AnonymousPageRequest* page_request);

  // Helper for removing a page from the PageQueues and freeing it (either to the PMM, or the owning
  // page source).
  void RemoveAndFreePageLocked(vm_page_t* page, bool freeing_owned_page) TA_REQ(lock());

  // Helper for removing a page from the page queues and either returning it immediately to the PMM
  // if it's a loaned page, or appending it to the supplied list.
  void RemovePageToListLocked(vm_page_t* page, list_node_t* free_list) TA_REQ(lock());

  // Helper class for managing a two part add page transaction. This object allows adding a page to
  // be split into a check and allocation, which can fail, with the final insertion, which cannot
  // fail.
  class AddPageTransaction {
   public:
    AddPageTransaction(VmPageOrMarkerRef slot, uint64_t offset, CanOverwriteContent overwrite)
        : slot_(slot), offset_(offset), overwrite_(overwrite) {}
    AddPageTransaction(AddPageTransaction&& other)
        : slot_(other.slot_), offset_(other.offset_), overwrite_(other.overwrite_) {
      other.slot_ = VmPageOrMarkerRef();
    }
    ~AddPageTransaction() { DEBUG_ASSERT(!slot_); }
    AddPageTransaction(const AddPageTransaction&) = delete;
    AddPageTransaction& operator=(const AddPageTransaction&) = delete;
    AddPageTransaction& operator=(AddPageTransaction&&) = delete;

    void Cancel(VmPageList& pl);
    VmPageOrMarker Complete(VmPageOrMarker p);
    uint64_t offset() const { return offset_; }
    CanOverwriteContent overwrite() const { return overwrite_; }

   private:
    VmPageOrMarkerRef slot_;
    uint64_t offset_;
    CanOverwriteContent overwrite_;
  };

  // Performs initial checks and slot allocations for inserting a new page at the specified
  // |offset|.
  //
  // |overwrite| controls how the function handles pre-existing content at |offset|. If |overwrite|
  // does not permit replacing the content, ZX_ERR_ALREADY_EXISTS will be returned.
  //
  // On success the returned |AddPageTransaction| *must* be used in a call to either
  // |CompleteAddPageLocked|, |CompleteAddNewPageLocked| or |CancelAddPageLocked|.
  //
  // No other |page_list_| operations should be performed until the transaction is complete, and
  // starting or ending a transaction should be assumed to invalidate any page_list_ iterators or
  // slots that have been looked up unless the caller explicitly knows otherwise.
  [[nodiscard]] zx::result<AddPageTransaction> BeginAddPageLocked(uint64_t offset,
                                                                  CanOverwriteContent overwrite)
      TA_REQ(lock());

  // Similar to |BeginAddPageLocked| except only the |overwrite| checks are performed and a ready to
  // use slot is provided by the caller. It is a requirement by the caller to ensure that |slot|:
  //  * Is for |offset| in this |page_list_|
  //  * Does not fall in the middle of an interval.
  // All other requirements of |BeginAddPageLocked| otherwise apply.
  [[nodiscard]] zx::result<AddPageTransaction> BeginAddPageWithSlotLocked(
      uint64_t offset, VmPageOrMarkerRef slot, CanOverwriteContent overwrite) TA_REQ(lock());

  // Completes an add page transaction that had been started by inserting the provided page |p| into
  // the slot looked up in |transaction|. Once complete the transaction must not be used in any
  // other complete or cancel calls.
  //
  // |p| must not be Empty()
  //
  // This operation unmaps the corresponding offset from any existing mappings, unless
  // |do_range_update| is false, in which case it will skip updating mappings.
  //
  // Any previous content in the slot is returned and must be dealt with by the caller.
  [[nodiscard]] VmPageOrMarker CompleteAddPageLocked(AddPageTransaction& transaction,
                                                     VmPageOrMarker&& p, bool do_range_update)
      TA_REQ(lock());

  // Similar to |CompleteAddPageLocked| except a |vm_page_t| is provided that is assumed to not yet
  // be in the |OBJECT| state, this page may also be optionally zeroed.
  [[nodiscard]] VmPageOrMarker CompleteAddNewPageLocked(AddPageTransaction& transaction,
                                                        vm_page_t* page, bool zero,
                                                        bool do_range_update) TA_REQ(lock());

  // Cancels an add page transaction and potentially frees the unused slot. It is required to call
  // this, instead of dropping an |AddPageTransaction| to ensure the page list does not gather empty
  // nodes. If the transaction was created with |BeginAddPageWithSlotLocked| then it is the
  // responsibility of the caller to know whether their slot might still be valid or not after
  // cancelling. Once cancelled the transaction must not have another further complete or cancel
  // calls against it.
  void CancelAddPageLocked(AddPageTransaction& transaction) TA_REQ(lock());

  // Helper for checking the |overwrite| conditions on a given slot.
  zx_status_t CheckOverwriteConditionsLocked(uint64_t offset, VmPageOrMarkerRef slot,
                                             CanOverwriteContent overwrite) TA_REQ(lock());

  // Add a page to the object at |offset|. This is just a wrapper around performing
  // |BeginAddPageLocked| and then |CompleteAddPageLocked|, with error handling to perform a free of
  // the page given in |p| should insertion fail. Otherwise see those methods for a description of
  // the parameters.
  zx::result<VmPageOrMarker> AddPageLocked(uint64_t offset, VmPageOrMarker&& p,
                                           CanOverwriteContent overwrite, bool do_range_update)
      TA_REQ(lock());

  // Unmaps and frees all the committed pages in the specified range.
  // Upon success the removed pages are freed and the number of pages freed is returned.
  //
  // Unlike DecommitRangeLocked(), this function only operates on |this| node, which must have no
  // parent.
  // |offset| must be page aligned. |len| must be less than or equal to |size_ - offset|. If |len|
  // is less than |size_ - offset| it must be page aligned.
  zx::result<uint64_t> UnmapAndFreePagesLocked(uint64_t offset, uint64_t len) TA_REQ(lock());

  // internal check if any pages in a range are pinned
  bool AnyPagesPinnedLocked(uint64_t offset, size_t len) TA_REQ(lock());

  // Helper function for ::GetAttributedMemoryInRangeLocked. Counts the number of bytes in
  // ancestor's vmos that should be attributed to this vmo for the specified range. It is an error
  // to pass in a range that does not need attributing (i.e. offset must be < parent_limit_),
  // although |len| is permitted to be sized such that the range exceeds parent_limit_. The return
  // value is the length of the processed region, which will be <= |size| and is guaranteed to be >
  // 0. The |count| is the number of bytes in this region that should be attributed to this vmo,
  // versus some other vmo.
  uint64_t CountAttributedAncestorBytesLocked(uint64_t offset, uint64_t size,
                                              AttributionCounts* count) const TA_REQ(lock());

  // Searches for the content for the page in |this| at |offset|. If the offset is already
  // populated in |this| then that page is returned with the |owner| set to |this|, otherwise the
  // parent hierarchy is searched for any content. The result could be used to initialize a commit,
  // or compare an existing commit with the original. The initial content is a VMPLCursor and may
  // be invalid if there was no explicit initial content. How to interpret an absence of content,
  // whether it is zero or otherwise, is left up to the caller.
  //
  // If an ancestor has a committed page which corresponds to |offset|, returns a cursor with
  // |current()| as that page as well as the VmCowPages and offset which own the page. If no
  // ancestor has a committed page for the offset, returns a cursor with a |current()| of nullptr as
  // well as the VmCowPages/offset which need to be queried to populate the page.
  //
  // The returned |visible_end| represents the size of the range in |owner| for which it can be
  // assumed that no child has content for, although for which the content might be in yet a higher
  // up parent. This will always be a subset of the provided |max_owner_length|, which serves as a
  // bound for the calculation and so passing in a smaller |max_owner_length| can sometimes be more
  // efficient.
  // It is an error for the |max_owner_length| to be < PAGE_SIZE.
  struct PageLookup {
    VMPLCursor cursor;
    VmCowPages* owner = nullptr;
    uint64_t owner_offset = 0;
    uint64_t visible_end = 0;
  };
  template <typename T>
  void FindPageContentLocked(uint64_t offset, uint64_t max_owner_length, T* out) TA_REQ(lock());

  // Searches for the initial content, i.e. the content that would be used to initially populate the
  // page, of |this| at |offset|. Whether there is presently any content populated in |this| is
  // ignored, and if there is content then this will still return what would be used to re-populate
  // that slot.
  void FindInitialPageContentLocked(uint64_t offset, PageLookup* out) TA_REQ(lock());

  // Helper function that 'forks' a page into |offset| of the current node, which must be a visible
  // node. If this function successfully inserts the page, it returns ZX_OK and populates
  // |out_page|. |page_request| must be provided and if ZX_ERR_SHOULD_WAIT is returned then this
  // indicates a transient allocation failure that should be resolved by waiting on the page_request
  // and retrying.
  //
  // The source page that is being forked has already been calculated - it is |page|, which
  // is currently in |page_owner| at offset |owner_offset|. |page_owner| must be a hidden node.
  //
  // This function is responsible for ensuring that COW clones never result in worse memory
  // consumption than simply creating a new VMO and memcpying the content. If |page| is not shared
  // at all, then this function assumes it is only accessible to this node. In that case |page| is
  // removed from |page_owner| and migrated into this node. Forking the page in that case would just
  // make |page| inaccessible, leaving |page| committed for no benefit.
  //
  // To handle memory allocation failure, this function allocates a slot in this node for the page
  // before modifying the source page or page list in |page_owner|. If that allocation fails, then
  // these are not altered.
  //
  // |page| must not be the zero-page, as there is no need to do the complex page fork logic to
  // reduce memory consumption in that case.
  zx_status_t CloneCowPageLocked(uint64_t offset, list_node_t* alloc_list, VmCowPages* page_owner,
                                 vm_page_t* page, uint64_t owner_offset,
                                 AnonymousPageRequest* page_request, vm_page_t** out_page)
      TA_REQ(lock());

  // Helper function that 'forks' the page into |offset| of the current node, which must be a
  // visible node. This function is similar to |CloneCowPageLocked|, but instead handles the case
  // where the forked page would be immediately overwritten with zeros after forking. It inserts a
  // marker into |offset| of the current node rather than actually forking or migrating the page. If
  // this function successfully inserts the marker, it returns ZX_OK.
  //
  // The source page that is being forked has already been calculated - it is |page|, which
  // is currently in |page_owner| at offset |owner_offset|. |page_owner| must be a hidden node.
  //
  // This function is responsible for ensuring that COW clones never result in worse memory
  // consumption than simply creating a new VMO and memcpying the content. If |page| is not shared
  // at all, then this function assumes it is only accessible to this node. In that case |page| is
  // removed from |page_owner| and placed in the |freed_list|. Forking the page in that case would
  // just make |page| inaccessible, leaving |page| committed for no benefit.
  //
  // To handle memory allocation failure, this function allocates a slot in this node for the marker
  // before modifying the source page or page list in |page_owner|. If that allocation fails, then
  // these are not altered.
  //
  // |page| must not be the zero-page, as there is no need to do the complex page fork logic to
  // reduce memory consumption in that case.
  zx_status_t CloneCowPageAsZeroLocked(uint64_t offset, list_node_t* freed_list,
                                       VmCowPages* page_owner, vm_page_t* page,
                                       uint64_t owner_offset, AnonymousPageRequest* page_request)
      TA_REQ(lock());

  // Helper struct which encapsulates a parent node along with a range and limit relative to it.
  struct ParentAndRange {
    VmCowPages* parent;
    uint64_t parent_offset;
    uint64_t parent_limit;
    uint64_t size;
  };

  // Helper function for |CloneUnidirectionalLocked| and |CloneBidirectionalLocked|.
  //
  // Walks the hierarchy from the `parent` node to the root and finds the most distant node which
  // could correctly be the parent for a new clone of this node.
  //
  // Computes a range and limit for the clone, relative to the final parent, such that the clone
  // can't see any more of that parent than it could if it was a direct clone of `this` node.
  //
  // For correctness the parent is either:
  //  * The last node encountered which satisfies `parent_must_be_hidden` if its true. The initial
  //    `parent` node satisfies `parent_must_be_hidden` even though its visible, because it may be
  //    converted to a hidden node by the caller.
  //  * The first node encountered that has pages in the clone's range for it to snapshot.
  //  * The root.
  //
  // The caller provides:
  //  * `parent`: Parent node to begin the search from. It must be a visible node.
  //             The new clone is logically a clone of it.
  //  * `offset`: Offset of the clone relative to the initial parent.
  //  * `size`: Size of the clone.
  //  * `parent_must_be_hidden`: true iff the final parent must satisfy this constraint.
  //
  // Returns the parent node along with the clones' range and limit relative to that parent.
  static ParentAndRange FindParentAndRangeForCloneLocked(VmCowPages* parent, uint64_t offset,
                                                         uint64_t size, bool parent_must_be_hidden);

  // Helper function for |CreateCloneLocked|.
  //
  // Performs a bidirectional clone operation, creating a new child node. The child behaves as if it
  // was an eager snapshot of the range [|offset|,|offset| + |size|) in this node.
  //
  // Writes to an uncommitted page in the child will cause it to snapshot that page from its
  // ancestors as the page existed at the time of this clone operation. The ancestor page is
  // guaranteed to never see the writes by the child.
  //
  // Unlike a unidirectional clone, the snapshot taken by the child is guaranteed to never contain
  // any writes or newly committed pages made by ancestor nodes after the clone operation is
  // performed.
  //
  // This node may not end up being the parent of the child, see |FindParentAndRangeForCloneLocked|.
  // However if this node does end up being the parent, then this method will create a second new
  // child node with the same properties as this node and this node will become the hidden parent of
  // both new children.
  zx_status_t CloneBidirectionalLocked(uint64_t offset, uint64_t size,
                                       fbl::RefPtr<VmCowPages>* cow_child) TA_REQ(lock());

  // Helper function for |CreateCloneLocked|.
  //
  // Performs a unidirectional clone operation, creating a new child node. The child behaves as if
  // it were a lazy snapshot of the range [|offset|,|offset| + |size|) in this node.
  //
  // Writes to an uncommitted page in the child will cause it to snapshot that page from its
  // ancestors *as the page exists at the time of the write*. The ancestor page is guaranteed to
  // never see the writes by the child.
  //
  // The snapshot taken by the child may or may not contain writes made by ancestor nodes after the
  // clone operation is performed, but this is not guaranteed. This includes pages which ancestors
  // have newly committed since this clone operation was performed - the clone may see these pages
  // to snapshot them but it is not guaranteed.
  //
  // This node may not end up being the parent of the child, see |FindParentAndRangeForCloneLocked|.
  zx_status_t CloneUnidirectionalLocked(uint64_t offset, uint64_t size,
                                        fbl::RefPtr<VmCowPages>* cow_child) TA_REQ(lock());

  // Release any pages this VMO can reference from the provided start offset till the end of the
  // VMO. This releases both directly owned pages, as well as pages in hidden parents that may be
  // considered owned by this VMO.
  // If applicable this method will update the parent_limit_ to reflect that it has removed any
  // reference to its parent range.
  void ReleaseOwnedPagesLocked(uint64_t start) TA_REQ(lock());

  // When cleaning up a hidden vmo, merges the hidden vmo's content (e.g. page list, view
  // of the parent) into the remaining child.
  void MergeContentWithChildLocked(VmCowPages* removed) TA_REQ(lock());

  // Moves an existing page to the wired queue as a consequence of the page being pinned.
  void MoveToPinnedLocked(vm_page_t* page, uint64_t offset) TA_REQ(lock());

  // Updates the page queue of an existing non-pinned page, moving it to whichever queue is
  // appropriate.
  void MoveToNotPinnedLocked(vm_page_t* page, uint64_t offset) TA_REQ(lock());

  // Places a newly added, not yet pinned, page into the appropriate page queue.
  void SetNotPinnedLocked(vm_page_t* page, uint64_t offset) TA_REQ(lock());

  // Updates the page's dirty state to the one specified, and also moves the page between page
  // queues if required by the dirty state. |dirty_state| should be a valid dirty tracking state,
  // i.e. one of Clean, AwaitingClean, or Dirty.
  //
  // |offset| is the page-aligned offset of the page in this object.
  //
  // |is_pending_add| indicates whether this page is yet to be added to this object's page list,
  // false by default. If the page is yet to be added, this function will skip updating the page
  // queue as an optimization, since the page queue will be updated later when the page gets added
  // to the page list. |is_pending_add| also helps determine certain validation checks that can be
  // performed on the page.
  void UpdateDirtyStateLocked(vm_page_t* page, uint64_t offset, DirtyState dirty_state,
                              bool is_pending_add = false) TA_REQ(lock());

  // Helper to invalidate any DIRTY requests in the specified range by spuriously resolving them.
  void InvalidateDirtyRequestsLocked(uint64_t offset, uint64_t len) TA_REQ(lock());

  // Helper to invalidate any READ requests in the specified range by spuriously resolving them.
  void InvalidateReadRequestsLocked(uint64_t offset, uint64_t len) TA_REQ(lock());

  // Initializes and adds as a child the given VmCowPages as a full clone of this one such that the
  // VmObjectPaged backlink can be moved from this to the child, keeping all page offsets, sizes and
  // other requirements (see VmObjectPaged::SetCowPagesReferenceLocked) are valid. This does also
  // move our paged_ref_ into child_ and update the VmObjectPaged backlinks.
  void CloneParentIntoChildLocked(fbl::RefPtr<VmCowPages> child) TA_REQ(lock());

  // Removes the specified child from this objects |children_list_| and performs any hierarchy
  // updates that need to happen as a result. This does not modify the |parent_| member of the
  // removed child and if this is not being called due to |removed| being destructed it is the
  // callers responsibility to correct parent_.
  void RemoveChildLocked(VmCowPages* removed) TA_REQ(lock());

  // Inserts a newly created VmCowPages into this hierarchy as a child of this VmCowPages.
  // Initializes child members based on the passed in values that only have meaning when an object
  // is a child. This updates the parent_ field in child to hold a refptr to |this|.
  void AddChildLocked(VmCowPages* child, uint64_t offset, uint64_t parent_limit) TA_REQ(lock());

  // Helpers to give convenience locked access to the parent_. Only valid to be called if there is a
  // parent.
  VmCowPages& parent_locked() TA_REQ(lock()) TA_ASSERT(parent_locked().lock()) {
    DEBUG_ASSERT(parent_);
    return *parent_;
  }
  const VmCowPages& parent_locked() const TA_REQ(lock()) TA_ASSERT(parent_locked().lock()) {
    DEBUG_ASSERT(parent_);
    return *parent_;
  }

  void ReplaceChildLocked(VmCowPages* old, VmCowPages* new_child) TA_REQ(lock());

  void DropChildLocked(VmCowPages* c) TA_REQ(lock());

  // Types for an additional linked list over the VmCowPages for use when doing a
  // RangeChangeUpdate.
  //
  // To avoid unbounded stack growth we need to reserve the memory to exist on a
  // RangeChange list in our object so that we can have a flat iteration over a
  // work list. RangeChangeLists should only be used by the RangeChangeUpdate
  // code.
  using RangeChangeNodeState = fbl::SinglyLinkedListNodeState<VmCowPages*>;
  struct RangeChangeTraits {
    static RangeChangeNodeState& node_state(VmCowPages& cow) { return cow.range_change_state_; }
  };
  using RangeChangeList =
      fbl::SinglyLinkedListCustomTraits<VmCowPages*, VmCowPages::RangeChangeTraits>;
  friend struct RangeChangeTraits;

  // Given an initial list of VmCowPages performs RangeChangeUpdate on it until the list is empty.
  static void RangeChangeUpdateListLocked(RangeChangeList* list, RangeChangeOp op);

  void RangeChangeUpdateFromParentLocked(uint64_t offset, uint64_t len, RangeChangeList* list)
      TA_REQ(lock());

  // Helper to check whether the requested range for LockRangeLocked() / TryLockRangeLocked() /
  // UnlockRangeLocked() is valid.
  bool IsLockRangeValidLocked(VmCowRange range) const TA_REQ(lock());

  // Returns the root parent's page source.
  fbl::RefPtr<PageSource> GetRootPageSourceLocked() const TA_REQ(lock());

  bool is_source_handling_free_locked() const TA_REQ(lock()) {
    return page_source_ && page_source_->properties().is_handling_free;
  }

  // Swap an old page for a new page in the page list. The old page must be at offset. The new page
  // must be in ALLOC state. On return, the old_page is owned by the caller. Typically the caller
  // will remove the old_page from pmm_page_queues() and free the old_page. The contents of new page
  // is not modified, and the caller is responsible for filling in the contents.
  void SwapPageInListLocked(uint64_t offset, vm_page_t* old_page, vm_page_t* new_page)
      TA_REQ(lock());

  // If page is still at offset, replace it with a different page.  If with_loaned is true, replace
  // with a loaned page.  If with_loaned is false, replace with a non-loaned page and a page_request
  // is required to be provided.
  zx_status_t ReplacePageLocked(vm_page_t* before_page, uint64_t offset, bool with_loaned,
                                vm_page_t** after_page, AnonymousPageRequest* page_request)
      TA_REQ(lock());

  // Copies the metadata information (dirty state, split bit information etc) from src_page to
  // dst_page in preparation for replacing src with dst. This copies the metadata information only
  // and not the contents, that must be done using the |CopyPageContentsForReplacementLocked|
  // method. This is split into two steps to allow the copying of the metadata and installation into
  // the page queues to be done under the pmm loaned pages lock, and then the copying of the page
  // contents to be done after with the loaned pages lock dropped.
  void CopyPageMetadataForReplacementLocked(vm_page_t* dst_page, vm_page_t* src_page)
      TA_REQ(lock());

  // Copies the page contents from src_page->dst_page to complete the replacement process. Typical
  // usage would have |CopyPageMetadatForReplacementLocked| already be performed, but this method
  // does not require it.
  void CopyPageContentsForReplacementLocked(vm_page_t* dst_page, vm_page_t* src_page)
      TA_REQ(lock());

  // Internal helper for performing reclamation via eviction on pager backed VMOs.
  // Assumes that the page is owned by this VMO at the specified offset.
  ReclaimCounts ReclaimPageForEvictionLocked(vm_page_t* page, uint64_t offset,
                                             EvictionHintAction hint_action) TA_REQ(lock());

  // Internal helper for performing reclamation via compression on an anonymous VMO. Assumes that
  // the page is owned by this VMO at the specified offset.
  // Assumes that the provided |compressor| is not-null.
  //
  // Borrows the guard for |lock_| and may drop the lock temporarily during execution.
  ReclaimCounts ReclaimPageForCompressionLocked(vm_page_t* page, uint64_t offset,
                                                VmCompressor* compressor,
                                                Guard<CriticalMutex>& guard) TA_REQ(lock());

  // Internal helper for performing reclamation against a discardable VMO. Assumes that the page is
  // owned by this VMO at the specified offset. If any discarding happens the number of pages is
  // returned. The passed in |page| must be the first page in the discardable VMO to trigger a
  // discard, otherwise it will fail.
  zx::result<uint64_t> ReclaimDiscardableLocked(vm_page_t* page, uint64_t offset) TA_REQ(lock());

  // Internal helper for discarding a VMO. Will discard if VMO is unlocked returning the count.
  zx::result<uint64_t> DiscardPagesLocked() TA_REQ(lock());

  // Internal helper for modifying just this value of high_priority_count_ without performing any
  // propagating.
  // Returns any delta that needs to be applied to the parent. If a zero value is returned then
  // propagation can be halted.
  int64_t ChangeSingleHighPriorityCountLocked(int64_t delta) TA_REQ(lock());

  // Specialized internal version of ZeroPagesLocked that only operates for a VMO where
  // |is_source_preserving_page_content| is true. |dirty_track| can be set to |true| if any zeroes
  // inserted are to be treated as Dirty, otherwise they are not dirty tracked.
  zx_status_t ZeroPagesPreservingContentLocked(uint64_t page_start_base, uint64_t page_end_base,
                                               bool dirty_track, list_node_t* freed_list,
                                               MultiPageRequest* page_request,
                                               uint64_t* processed_len_out) TA_REQ(lock());

  // magic value
  fbl::Canary<fbl::magic("VMCP")> canary_;

  const uint32_t pmm_alloc_flags_;

  VmCowPagesOptions options_ TA_GUARDED(lock());

  // length of children_list_
  uint32_t children_list_len_ TA_GUARDED(lock()) = 0;

  uint64_t size_ TA_GUARDED(lock());
  // Offset in the *parent* where this object starts.
  uint64_t parent_offset_ TA_GUARDED(lock()) = 0;
  // Offset in *this object* above which accesses will no longer access the parent.
  uint64_t parent_limit_ TA_GUARDED(lock()) = 0;
  // Offset in our root parent where this object would start if projected onto it. This value is
  // used as an efficient summation of accumulated offsets to ensure that an offset projected all
  // the way to the root would not overflow a 64-bit integer. Although actual page resolution
  // would never reach the root in such a case, a childs full range projected onto its parent is
  // used to simplify some operations and so this invariant of not overflowing accumulated offsets
  // needs to be maintained.
  uint64_t root_parent_offset_ TA_GUARDED(lock()) = 0;

  // parent pointer (may be null)
  fbl::RefPtr<VmCowPages> parent_ TA_GUARDED(lock());

  // list of every child
  fbl::TaggedDoublyLinkedList<VmCowPages*, internal::ChildListTag> children_list_
      TA_GUARDED(lock());

  // Counts the total number of pages pinned by ::CommitRange. If one page is pinned n times, it
  // contributes n to this count.
  uint64_t pinned_page_count_ TA_GUARDED(lock()) = 0;

  // The page source, if any.
  const fbl::RefPtr<PageSource> page_source_;

  // Count reclamation events so that we can report them to the user.
  uint64_t reclamation_event_count_ TA_GUARDED(lock()) = 0;

  // a tree of pages
  VmPageList page_list_ TA_GUARDED(lock());

  RangeChangeNodeState range_change_state_;
  VmCowRange range_change_range_ TA_GUARDED(lock());

  // Reference back to a VmObjectPaged, which should be valid at all times after creation until the
  // VmObjectPaged has been destroyed, unless this is a hidden node. We use this in places where we
  // have access to the VmCowPages and need to look up the "owning" VmObjectPaged for some
  // information, e.g. when deduping zero pages, for performing cache or mapping updates, for
  // inserting references to the reference list.
  //
  // This is a raw pointer to avoid circular references, the VmObjectPaged destructor needs to
  // update it.
  VmObjectPaged* paged_ref_ TA_GUARDED(lock()) = nullptr;

  // Non-null if this is a discardable VMO.
  const ktl::unique_ptr<DiscardableVmoTracker> discardable_tracker_;

  // Count of how many references to this VMO are requesting this be high priority, where references
  // include VmMappings and children. If this is >0 then it is considered high priority and any kind
  // of reclamation will be disabled. Further, if this is >0 and this has a parent, then this will
  // contribute a +1 count towards its parent.
  //
  // Due to the life cycle of a VmCowPages it is expected that at the point this is destroyed it has
  // a count of 0. This is because that to be destroyed we must have no mappings and no children,
  // i.e. no references, and so nothing can be contributing to a positive count.
  //
  // It is an error for this value to ever become negative.
  int64_t high_priority_count_ TA_GUARDED(lock()) = 0;

  // With this bool we achieve these things:
  //  * Avoid using loaned pages for a VMO that will just get pinned and replace the loaned pages
  //    with non-loaned pages again, possibly repeatedly.
  //  * Avoid increasing pin latency in the (more) common case of pinning a VMO the 2nd or
  //    subsequent times (vs the 1st time).
  //  * Once we have any form of active sweeping (of data from non-loaned to loaned physical pages)
  //    this bool is part of mitigating any potential DMA-while-not-pinned (which is not permitted
  //    but is also difficult to detect or prevent without an IOMMU).
  bool ever_pinned_ TA_GUARDED(lock()) = false;

  // Tracks whether this VMO was modified (written / resized) if backed by a pager. This gets reset
  // to false if QueryPagerVmoStatsLocked() is called with |reset| set to true.
  bool pager_stats_modified_ TA_GUARDED(lock()) = false;

  // Tracks the life cycle of the VmCowPages. The primary purpose of the life cycle is to create an
  // invariant that by the time a VmCowPages destructor runs it does not contain any pages. This is
  // achieved by requiring an explicit Dead transition that provides a point to perform cleanup.
  // An Init state is introduced to allow for multi step creation that may fail.
  enum class LifeCycle : uint8_t {
    Init,
    Alive,
    Dead,
  };
  LifeCycle life_cycle_ TA_GUARDED(lock()) = LifeCycle::Init;

  // PageCache instance for COW page allocations.
  inline static page_cache::PageCache page_cache_;
};

// Implements a cursor that allows for retrieving successive pages over a range in a VMO. The
// range that is iterated is determined at construction from GetLookupCursorLocked and cannot be
// modified, although it can be effectively shrunk by ceasing queries early.
//
// The cursor is designed under the assumption that the caller is tracking, implicitly or
// explicitly, how many queries have been done, and the methods do not return errors if more slots
// are queried than was originally requested in the range. They will, however, assert and panic.
//
// There are three controls provided by this object.
//
//   Zero forks: By default new zero pages will be considered zero forks and added to the zero page
//   scanner list, this can be disabled with |DisableZeroFork|.
//
//   Access time: By default pages that are returned will be considered accessed. This can be
//   changed with |DisableMarkAccessed|.
//
//   Allocation lists: By default pages will be acquired from the pmm as needed. An allocation list
//   can be given use |GiveAllocList|.
//
// The VMO lock *must* be held contiguously from the call to GetLookupCursorLocked over the entire
// usage of this object.
class VmCowPages::LookupCursor {
 public:
  ~LookupCursor() { DEBUG_ASSERT(!alloc_list_); }

  // Convenience struct holding the return result of the Require* methods.
  struct RequireResult {
    vm_page_t* page = nullptr;
    bool writable = false;
  };

  // The Require* methods will attempt to lookup the next offset in the VMO and return you a page
  // with the properties requested. If a page can be returned in the zx::ok result then the internal
  // cursor is incremented and future operations will act on the next offset. If an error occurs
  // then the internal cursor is not incremented.
  // These methods all take a PageRequest, which will be populated in the case of returning
  // ZX_ERR_SHOULD_WAIT. For optimal page request generation the |max_request_pages| controls how
  // many pages you are intending to lookup, and |max_request_pages| must not exceed the remaining
  // window of the cursor.
  // The returned page, unless it was just allocated, will have its access time updated based on
  // |EnableMarkAccessed|, with newly allocated pages always being default considered to have just
  // been accessed.

  // Returned page must be an allocated and owned page in this VMO. As such this will never return a
  // reference to the zero page. |will_write| indicates if this page needs to be writable or not,
  // which for an owned and allocated page just involves a potential dirty request / transition.
  zx::result<RequireResult> RequireOwnedPage(bool will_write, uint max_request_pages,
                                             MultiPageRequest* page_request) TA_REQ(lock());

  // Returned page will only be read from. This can return zero pages or pages from a parent VMO.
  zx::result<RequireResult> RequireReadPage(uint max_request_pages, MultiPageRequest* page_request)
      TA_REQ(lock());

  // Returned page will be readable or writable based on the |will_write| flag.
  zx::result<RequireResult> RequirePage(bool will_write, uint max_request_pages,
                                        MultiPageRequest* page_request) TA_REQ(lock()) {
    // Being writable implies owning the page, so forward to the correct operation.
    if (will_write) {
      return RequireOwnedPage(true, max_request_pages, page_request);
    }
    return RequireReadPage(max_request_pages, page_request);
  }

  // The IfExistPages methods is intended to be cheaper than the Require* methods and to allow for
  // performing actions if pages already exist, without performing allocations. As a result this
  // may fail to return pages in scenarios that Require* methods would, and in general are allowed
  // to always fail for any reason.
  // These methods cannot generate page requests and will not perform allocations or otherwise
  // mutate the VMO contents and will not update the access time of the pages.

  // Walks up to |max_pages| from the current offset, filling in |paddrs| as long as there are
  // actual pages and, if |will_write| is true, that they can be written to. The return value is
  // the number of contiguous pages found and filled into |paddrs|, and the cursor is incremented
  // by that many pages.
  uint IfExistPages(bool will_write, uint max_pages, paddr_t* paddrs) TA_REQ(lock());

  // Checks the current slot for a page and returns it. This does not return zero pages and, due to
  // the lack of taking a page request, will not perform copy-on-write allocations or dirty
  // transitions. In these cases it will return nullptr even though there is content.
  // The internal cursor is always incremented regardless of the return value.
  vm_page_t* MaybePage(bool will_write) TA_REQ(lock());

  // Has similar properties of |MaybePage|, except it returns how many times in a row |MaybePage|
  // would have returned a nullptr. Regardless of the return value of this method, it is not
  // guaranteed that the next call to |MaybePage| will not be a nullptr. The cursor is incremented
  // by the number of pages returned.
  uint64_t SkipMissingPages() TA_REQ(lock());

  // Provides a list of pages that can be used to service any allocations. This is useful if you
  // know you will be looking up multiple absent pages and want to avoid repeatedly hitting the pmm
  // for single pages.
  // If a list is provided then ClearAllocList must be called prior to the cursor being destroyed.
  void GiveAllocList(list_node_t* alloc_list) {
    DEBUG_ASSERT(alloc_list);
    alloc_list_ = alloc_list;
  }

  // Clears any remaining allocation list. This does not free any remaining pages, and it is the
  // callers responsibility to check the list and free any pages.
  void ClearAllocList() {
    DEBUG_ASSERT(alloc_list_);
    alloc_list_ = nullptr;
  }

  // Disables placing newly allocated zero pages in the zero fork list.
  void DisableZeroFork() { zero_fork_ = false; }

  // Indicates that any existing pages that are returned should not be considered accessed and have
  // their accessed times updated.
  void DisableMarkAccessed() { mark_accessed_ = false; }

  // Exposed for lock assertions.
  Lock<CriticalMutex>* lock() const TA_RET_CAP(target_->lock_ref()) { return target_->lock(); }
  Lock<CriticalMutex>& lock_ref() const TA_RET_CAP(target_->lock_ref()) {
    return target_->lock_ref();
  }

 private:
  LookupCursor(VmCowPages* target, VmCowRange range)
      : target_(target),
        offset_(range.offset),
        end_offset_(range.end()),
        target_preserving_page_content_(target->is_source_preserving_page_content()),
        zero_fork_(!target_preserving_page_content_ && target->can_decommit_zero_pages_locked()) {}

  // Note: Some of these methods are marked __ALWAYS_INLINE as doing so has a dramatic performance
  // improvement, and is worth the increase in code size. Due to gcc limitations to mark them
  // __ALWAYS_INLINE they need to be declared here in the header.

  // Increments the cursor to the next offset. Doing so may invalidate the cursor and requiring
  // recalculating.
  __ALWAYS_INLINE void IncrementCursor() TA_REQ(lock()) {
    offset_ += PAGE_SIZE;
    if (offset_ == owner_info_.visible_end_) {
      // Have reached either the end of the valid iteration range, or the end of the visible portion
      // of the owner. In the latter case we set owner_ to null as we need to walk up the hierarchy
      // again to find the next owner that applies to this slot.
      // In the case where we have reached the end of the range, i.e. offset_ is also equal to
      // end_offset_, there is nothing we need to do, but to ensure that an error is generated if
      // the user incorrectly attempts to get another page we also set the owner to the nullptr.
      owner_info_.owner_ = nullptr;
    } else {
      // Increment the owner offset and step the page list cursor to the next slot.
      owner_info_.owner_offset_ += PAGE_SIZE;
      owner_info_.owner_pl_cursor_.step();
      owner_cursor_ = owner_info_.owner_pl_cursor_.current();

      // When iterating, it's possible that we need to find a new owner even before we hit the
      // visible_end_. This happens since even if we have no content at our cursor, we might have a
      // parent with content, and the visible_end_ is tracking the range visible in us from the
      // target and does not imply we have all the content.
      // Consider a simple hierarchy where the root has a page in slot 1, [.P.], then its child has
      // a page in slot 0 [P...] and then its child, the target, has no pages [...] A cursor on this
      // range will initially find the owner as this middle object, and a visible length of 3 pages.
      // However, when we step the cursor we clearly need to then walk up to our parent to get the
      // page. In this case we would ideally walk up to the parent, if there is one, and check for
      // content, or if no parent keep returning empty slots. Unfortunately once the cursor returns
      // a nullptr we cannot know where the next content might be. To make things simpler we just
      // invalidate owner_ if we hit this case and re-walk from the bottom again.
      if (!owner_cursor_ || (owner_cursor_->IsEmpty() && owner()->parent_)) {
        owner_info_.owner_ = nullptr;
      }
    }
  }

  // Increments the current offset by the given delta, but invalidates the cursor itself requiring
  // it to be recalculated next time EstablishCursor is called.
  void IncrementOffsetAndInvalidateCursor(uint64_t delta);

  // Returns whether the cursor is currently valid or needs to be re-calculated.
  bool IsCursorValid() const {
    // The owner being set is used to indicate whether the cursor is valid or not. Any operations
    // that would invalidate the cursor will always clear owner_.
    return owner_info_.owner_;
  }

  // Calculates the current cursor, finding the correct owner, owner offset etc. There is always an
  // owner and this process can never fail.
  void EstablishCursor() TA_REQ(lock());

  // Helpers for querying the state of the cursor.
  bool CursorIsPage() const { return owner_cursor_ && owner_cursor_->IsPage(); }
  bool CursorIsMarker() const { return owner_cursor_ && owner_cursor_->IsMarker(); }
  bool CursorIsEmpty() const { return !owner_cursor_ || owner_cursor_->IsEmpty(); }
  bool CursorIsReference() const { return owner_cursor_ && owner_cursor_->IsReference(); }
  // Checks if the cursor is exactly at a sentinel, and not generally inside an interval.
  bool CursorIsIntervalZero() const { return owner_cursor_ && owner_cursor_->IsIntervalZero(); }

  // Checks if the cursor, as determined by the current offset and not the literal cursor_, is in a
  // zero interval.
  bool CursorIsInIntervalZero() const TA_REQ(lock()) {
    return CursorIsIntervalZero() ||
           owner()->page_list_.IsOffsetInZeroInterval(owner_info_.owner_offset_);
  }

  // The cursor can be considered to have content of zero if either it points at a zero marker, or
  // the cursor itself is empty and content is initially zero. Content is initially zero if either
  // there isn't a page source, or the offset is in a zero interval.
  // If a page source is not preserving content then we could consider it to be zero, except we
  // would not necessarily be able to fork that zero page to create an owned/writable page. In
  // practice this case only exists for contiguous VMOs, and the way they are used makes optimizing
  // to return the zero page in the case of reads not beneficial.
  bool CursorIsContentZero() const TA_REQ(lock());

  // A usable page is either just any page, if not writing, or if writing, a page that is owned by
  // the target and doesn't need any dirty transitions. i.e., a page that is ready to use right now.
  bool CursorIsUsablePage(bool writing) {
    return CursorIsPage() && (!writing || (owner_info_.owner_ == target_ && !TargetDirtyTracked()));
  }

  // Determines whether the zero content at the current cursor should be supplied as dirty or not.
  // This is only allowed to be called if CursorIsContentZero is true.
  bool TargetZeroContentSupplyDirty(bool writing) const TA_REQ(lock());

  // Returns whether the target is tracking the dirtying of content with dirty pages and dirty
  // transitions.
  bool TargetDirtyTracked() const {
    // Presently no distinction between preserving page content and being dirty tracked.
    return target_preserving_page_content_;
  }

  // Turns the supplied page into a result. Does not increment the cursor. |in_target| specifies
  // whether the page is known to be in target_ or in some parent object.
  RequireResult PageAsResultNoIncrement(vm_page_t* page, bool in_target);

  // Turns the current cursor, which must be a page, into a result and handles any access time
  // updating. Increments the cursor.
  __ALWAYS_INLINE RequireResult CursorAsResult() TA_REQ(lock()) {
    if (mark_accessed_) {
      pmm_page_queues()->MarkAccessed(owner_cursor_->Page());
    }
    // Inform PageAsResult whether the owner_ is the target_, but otherwise let it calculate the
    // actual writability of the page.
    RequireResult result =
        PageAsResultNoIncrement(owner_cursor_->Page(), owner_info_.owner_ == target_);
    IncrementCursor();
    return result;
  }

  // Allocates a new page for the target that is a copy of the provided |source| page. On success
  // page is inserted into target at the current offset_ and the cursor is incremented.
  zx::result<RequireResult> TargetAllocateCopyPageAsResult(vm_page_t* source,
                                                           DirtyState dirty_state,
                                                           AnonymousPageRequest* page_request)
      TA_REQ(lock());

  // Attempts to turn the current cursor, which must be a reference, into a page.
  zx_status_t CursorReferenceToPage(AnonymousPageRequest* page_request) TA_REQ(lock());

  // Helpers for generating read or dirty requests for the given maximal range.
  zx_status_t ReadRequest(uint max_request_pages, PageRequest* page_request) TA_REQ(lock());
  zx_status_t DirtyRequest(uint max_request_pages, LazyPageRequest* page_request) TA_REQ(lock());

  // If we held lock(), then since owner_ is from the same hierarchy as the target then we must also
  // hold its lock.
  VmCowPages* owner() const TA_REQ(lock()) TA_ASSERT(owner()->lock()) { return owner_info_.owner_; }

  // Target always exists. This is provided in the constructor and will always be non-null.
  VmCowPages* const target_;

  // The current offset_ in target_. This will always be <= end_offset_ and is only allowed to
  // increase. The validity of this range is checked prior to construction by GetLookupCursor
  uint64_t offset_ = 0;

  // The offset_ in target_ at which the cursor ceases being valid. The end_offset_ itself will
  // never be used as a valid offset_. VMOs are designed such that the end of a VMO+1 will not
  // overflow.
  const uint64_t end_offset_;

  // This struct exists to define a layout that is identical to |PageLookup| that is returned by
  // FindPageContentLocked. Using this custom struct instead of PageLookup here allows for more
  // meaningful names and comments.
  struct OwnerInfo {
    // Cursor in the page list of the current owner_ and is invalid if owner_ is nullptr. This is
    // used to efficiently pull contiguous pages in an owner_ and the current() value of it is
    // cached in cursor_.
    VMPLCursor owner_pl_cursor_;

    // owner_ represent the current owner of cursor_/pl_cursor_. owner_ can be non-null while
    // cursor_ is null to indicate a lack of content, although in this case the owner_ can also be
    // assumed to be the root. owner_ being null is used to indicate that the cursor is invalid and
    // the owner for any content in the current slot needs to be looked up.
    VmCowPages* owner_ = nullptr;

    // The offset_ normalized to the current owner_. This is equal to offset_ when owner_ ==
    // target_.
    uint64_t owner_offset_ = 0;

    // Tracks the offset in target_ at which the current pl_cursor_ becomes invalid. This range
    // essentially means that no VMO between target_ and owner_ had any content, and so the cursor
    // in owner is free to walk contiguous pages up to this point. This does not mean that there is
    // no content in the parent_ of owner_, and so even if visible_end_ is not reached, if an empty
    // slot is found the parent_ must then be checked. See IncrementCursor for more details.
    uint64_t visible_end_ = 0;
  } owner_info_;

  // This is a cache of owner_pl_cursor_.current()
  VmPageOrMarkerRef owner_cursor_;

  // Value of target_->is_source_preserving_page_content() cached on creation as there is spare
  // padding space to store it here, and needed to retrieve this value to initialize zero_fork_
  // anyway.
  const bool target_preserving_page_content_;

  // Tracks whether zero forks should be tracked and placed in the corresponding page queue. This is
  // initialized to true if it's legal to place pages in the zero fork queue, which requires that
  // target_ not be pager backed.
  bool zero_fork_ = false;

  // Whether existing pages should be have their access time updated when they are returned.
  bool mark_accessed_ = true;

  // Optional allocation list that will be used for any page allocations.
  list_node_t* alloc_list_ = nullptr;

  friend VmCowPages;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_VM_COW_PAGES_H_
