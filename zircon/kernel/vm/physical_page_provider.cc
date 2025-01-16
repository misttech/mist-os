// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "include/vm/physical_page_provider.h"

#include <lib/counters.h>
#include <lib/dump/depth_printer.h>
#include <lib/fit/result.h>
#include <trace.h>

#include <kernel/range_check.h>
#include <object/thread_dispatcher.h>

#define LOCAL_TRACE 0

KCOUNTER(physical_reclaim_total_requests, "physical.reclaim.total_requests")
KCOUNTER(physical_reclaim_succeeded_requests, "physical.reclaim.succeeded_requests")
KCOUNTER(physical_reclaim_failed_requests, "physical.reclaim.failed_requests")

namespace {

const PageSourceProperties kProperties{
    .is_user_pager = false,
    .is_preserving_page_content = false,
    .is_providing_specific_physical_pages = true,
    .is_handling_free = true,
};

}  // namespace

PhysicalPageProvider::PhysicalPageProvider(uint64_t size) : size_(size) { LTRACEF("\n"); }

PhysicalPageProvider::~PhysicalPageProvider() {
  LTRACEF("%p\n", this);
  // In error paths we can destruct without detached_ or closed_ becoming true.
}

const PageSourceProperties& PhysicalPageProvider::properties() const { return kProperties; }

void PhysicalPageProvider::Init(VmCowPages* cow_pages, PageSource* page_source, paddr_t phys_base) {
  DEBUG_ASSERT(cow_pages);
  DEBUG_ASSERT(!IS_PAGE_ALIGNED(kInvalidPhysBase));
  DEBUG_ASSERT(IS_PAGE_ALIGNED(phys_base));
  DEBUG_ASSERT(!cow_pages_);
  DEBUG_ASSERT(phys_base_ == kInvalidPhysBase);
  Guard<Mutex> guard{&mtx_};
  cow_pages_ = cow_pages;
  page_source_ = page_source;
  phys_base_ = phys_base;
}

// Called under lock of contiguous VMO that needs the pages.  The request is later processed at the
// start of WaitOnEvent.
void PhysicalPageProvider::SendAsyncRequest(PageRequest* request) {
  DEBUG_ASSERT(phys_base_ != kInvalidPhysBase);
  DEBUG_ASSERT(SupportsPageRequestType(GetRequestType(request)));
  Guard<Mutex> guard{&mtx_};
  ASSERT(!closed_);

  // PhysicalPageProvider always operates async (similar to PagerProxy), because we'd like to (in
  // typical non-overlapping commit/decommit usage) have one batch that covers the entire commit,
  // regardless of the fact that some of the pages may already be free and therefore could be
  // immediately obtained.  Quite often at least one page will be presently owned by a different
  // VMO, so we may as well always do one big async batch that deals with all the presently
  // non-FREE_LOANED pages.
  //
  // At this point the page may be FREE_LOANED, or in use by a different VMO.
  //
  // Allocation of a new page to a VMO has an interval during which the page is not free, but also
  // isn't state == OBJECT yet.  During processing we rely on that interval occurring only under the
  // other VMO's lock, but we can't acquire the other VMO's lock here since we're already currently
  // holding the underlying owning contiguous VMO's lock.
  QueueRequestLocked(request);
}

void PhysicalPageProvider::QueueRequestLocked(PageRequest* request) {
  DEBUG_ASSERT(phys_base_ != kInvalidPhysBase);
  DEBUG_ASSERT(SupportsPageRequestType(GetRequestType(request)));
  ASSERT(!closed_);
  pending_requests_.push_back(request);
}

void PhysicalPageProvider::ClearAsyncRequest(PageRequest* request) {
  DEBUG_ASSERT(phys_base_ != kInvalidPhysBase);
  DEBUG_ASSERT(SupportsPageRequestType(GetRequestType(request)));
  Guard<Mutex> guard{&mtx_};
  ASSERT(!closed_);

  if (fbl::InContainer<PageProviderTag>(*request)) {
    pending_requests_.erase(*request);
  }

  // No need to chase down any currently-processing request here, since before processing a request,
  // we stash the values of all fields we need from the PageRequest under the lock.  So any
  // currently-processing request is independent from the PageRequest that started it.
  //
  // TODO(https://fxbug.dev/42080926): There is an issue however with not tracking down
  // currently-processing requests. While the request is being processed, a new request for the same
  // range can be generated again (since we "canceled" the current one), which will break
  // assumptions around only ever processing unique ranges. More details in the linked bug.
}

void PhysicalPageProvider::SwapAsyncRequest(PageRequest* old, PageRequest* new_req) {
  DEBUG_ASSERT(phys_base_ != kInvalidPhysBase);
  DEBUG_ASSERT(SupportsPageRequestType(GetRequestType(old)));
  DEBUG_ASSERT(SupportsPageRequestType(GetRequestType(new_req)));
  Guard<Mutex> guard{&mtx_};
  ASSERT(!closed_);

  if (fbl::InContainer<PageProviderTag>(*old)) {
    pending_requests_.insert(*old, new_req);
    pending_requests_.erase(*old);
  }
}

void PhysicalPageProvider::FreePages(list_node* pages) {
  // This marks the pages loaned, and makes them FREE_LOANED for potential use by other clients that
  // are ok with getting loaned pages when allocating.
  pmm_begin_loan(pages);
}

bool PhysicalPageProvider::DebugIsPageOk(vm_page_t* page, uint64_t offset) {
  Guard<Mutex> guard{&mtx_};
  DEBUG_ASSERT((cow_pages_ != nullptr) == (phys_base_ != kInvalidPhysBase));
  // Assume pages added before we know the cow_pages_ or phys_base_ are ok.
  if (!cow_pages_) {
    return true;
  }
  return (page->paddr() - phys_base_) == offset;
}

void PhysicalPageProvider::OnDetach() {
  Guard<Mutex> guard{&mtx_};
  ASSERT(!closed_);
  detached_ = true;
}

void PhysicalPageProvider::OnClose() {
  Guard<Mutex> guard{&mtx_};
  ASSERT(!closed_);
  closed_ = true;
  // By the time OnClose() is called, VmCowPages::DeadTransition() has already loaned all the pages,
  // so we can do pmm_delete_lender() on the whole range here.
  if (phys_base_ != kInvalidPhysBase) {
    pmm_delete_lender(phys_base_, size_ / PAGE_SIZE);
  }
}

bool PhysicalPageProvider::DequeueRequest(uint64_t* request_offset, uint64_t* request_length) {
  Guard<Mutex> guard{&mtx_};
  // closed_ can be true here, but if closed_ is true, then pending_requests_ is also empty, so
  // we won't process any more requests once closed_ is true.
  DEBUG_ASSERT(!closed_ || pending_requests_.is_empty());
  if (pending_requests_.is_empty()) {
    // Done with all requests (or remaining requests cancelled).
    return false;
  }
  PageRequest* request = pending_requests_.pop_front();
  DEBUG_ASSERT(request);
  DEBUG_ASSERT(SupportsPageRequestType(GetRequestType(request)));
  *request_offset = GetRequestOffset(request);
  *request_length = GetRequestLen(request);
  DEBUG_ASSERT(InRange(*request_offset, *request_length, size_));
  return true;
}

zx_status_t PhysicalPageProvider::WaitOnEvent(Event* event) {
  // When WaitOnEvent is called, we know that the event being waited on is associated with a request
  // that's already been queued, so we can use this thread to process _all_ the queued requests
  // first, and then wait on the event which then won't have any reason to block this thread, since
  // every page of every request that existed on entry to this method has been succeeded or failed
  // by the time we wait on the passed-in event.
  uint64_t request_offset;
  uint64_t request_length;
  while (DequeueRequest(&request_offset, &request_length)) {
    DEBUG_ASSERT(request_offset + request_length > request_offset);

    pmm_cancel_loan(phys_base_ + request_offset, request_length / PAGE_SIZE);

    // Evict needed physical pages from other VMOs, so that needed physical pages become free.  This
    // is iterating over the destination offset in cow_pages_.  The needed pages can be scattered
    // around in various VMOs and offsets of those VMOs, and can be free (but loan_cancelled so they
    // won't be picked up for a new use), and may be becoming free as we're running this loop.
    uint64_t request_end = request_offset + request_length;
    for (uint64_t offset = request_offset; offset < request_end; offset += PAGE_SIZE) {
      vm_page_t* page = paddr_to_vm_page(phys_base_ + offset);
      DEBUG_ASSERT(page);
      // GetCowForLoanedPage() only finds pages that are definitely in the page queues. Pages might
      // be stack owned if they are moving between the page queues and the pmm. In this case we must
      // wait on the stack ownership event until the page 'settles' into place. We might need to
      // both wait for the alloc path, but then it could be getting free'd before we manage to
      // lookup the page queues, and so we must wait again.
      // Due to the loaned cancelled flag (that pmm_end_loan sets) the number of waits should be
      // bounded to a max of two, since a page can at most be going from ALLOC->OBJECT and then
      // ALLOC->FREE_LOANED, after which it is trapped by loan cancelled. For safety we allow the
      // loop to run additional times, but warn on an excessive number of iterations.
      // cannot keep we run in a loop for safety and warn if we see an excessive number
      // of iterations.
      uint32_t iterations = 0;
      while (!page->is_free_loaned()) {
        // Page should never have entered the regular FREE state without us finding it and
        // explicitly ending the loan.
        DEBUG_ASSERT(!page->is_free());
        if (++iterations % 2 == 0) {
          dprintf(INFO, "PhysicalPageProvider::WaitOnEvent() looping more than expected\n");
        }
        auto maybe_vmo_backlink = pmm_page_queues()->GetCowForLoanedPage(page);
        if (!maybe_vmo_backlink) {
          // If the page is not in the page queues then it is in one of the following states.
          //  * Is in the PMM.
          //  * Just got allocated and is on the way to the page queues.
          //  * Just got removed from the page queues and is on the way to the PMM.
          // For the first case it will kept in free loaned by the pmm_cancel_loan call above, and
          // our work is done. For the latter two cases we can wait till the page is not stack
          // owned, solidifying it into one of the two locations.
          // All of these are handled by just going around the loop.
        } else {
          auto& vmo_backlink = maybe_vmo_backlink.value();
          DEBUG_ASSERT(vmo_backlink.cow);
          auto& cow_container = vmo_backlink.cow;

          DEBUG_ASSERT(!page->object.always_need);
          bool needs_evict = true;

          // Check if we should attempt to replace the page to avoid eviction.
          if (pmm_physical_page_borrowing_config()->is_replace_on_unloan_enabled()) {
            __UNINITIALIZED AnonymousPageRequest page_request;
            zx_status_t replace_result = cow_container->ReplacePage(page, vmo_backlink.offset,
                                                                    false, nullptr, &page_request);
            // If replacement failed for any reason, fall back to eviction.
            needs_evict = replace_result != ZX_OK;
            if (replace_result == ZX_ERR_SHOULD_WAIT) {
              page_request.Cancel();
            }
          }

          if (needs_evict) {
            cow_container->ReclaimPageForEviction(page, vmo_backlink.offset);
            // Either eviction succeeded, and the loaned page is in the PMM, or it failed and we are
            // racing with another thread doing the free. For both cases we can just wait till its
            // not stack owned and then when we go around the loop we should find it in the
            // FREE_LOANED state.
          }
        }
        // Ensure the page is not currently stack owned before trying the loop again. This method
        // has a very quick early abort if not stack owned, so calling redundantly is not
        // inefficient, so we do it for simplicity of code layout.
        StackOwnedLoanedPagesInterval::WaitUntilContiguousPageNotStackOwned(page);
      }
    }  // for pages of request

    // Finish processing request.

    // These are ordered by cow_pages_ offsets (destination offsets).
    list_node pages_in_transit;
    list_initialize(&pages_in_transit);
    // Now get the FREE_LOANED pages from PMM.  Thanks to PageSource only allowing up to 1 request
    // for a given page at a time, we know all these pages are still loaned, and currently
    // FREE_LOANED, so we'll get all these pages.
    pmm_end_loan(phys_base_ + request_offset, request_length / PAGE_SIZE, &pages_in_transit);
    // An interfering decommit can occur after we've moved these pages into VmCowPages, but not yet
    // moved the entire commit request into VmCowPages.  If not all pages end up present in
    // cow_pages_ on return to the user from the present commit, due to concurrent decommit, that's
    // just normal commit semantics.
    //
    // Supply the pages we got to cow_pages_.  Also tell it what range to claim is supplied now for
    // convenience.
    //
    // If there's an interfering decommit, then that decommit can only interfere after we've added
    // the pages to VmCowPages, so isn't an immediate concern here.
    //
    // We want to use VmCowPages::SupplyPages() to avoid a proliferation of VmCowPages code that
    // calls OnPagesSupplied() / OnPagesFailed(), so to call SupplyPages() we need a
    // VmPageSpliceList.  We put all the pages in the "head" portion of the VmPageSpliceList since
    // there are no VmPageListNode(s) involved in this path.  We also zero the pages here, since
    // SupplyPages() doesn't do that.
    //
    // We can zero the pages before we supply them, which avoids holding the VmCowPages::lock_ while
    // zeroing, and also allows us to flush the zeroes to RAM here just in case any client is
    // (incorrectly) assuming that non-pinned pages necessarily remain cache clean once they are
    // cache clean.
    vm_page_t* page;
    list_for_every_entry (&pages_in_transit, page, vm_page, queue_node) {
      void* ptr = paddr_to_physmap(page->paddr());
      DEBUG_ASSERT(ptr);
      arch_zero_page(ptr);
      arch_clean_invalidate_cache_range(reinterpret_cast<vaddr_t>(ptr), PAGE_SIZE);
    }
    auto splice_list =
        VmPageSpliceList::CreateFromPageList(request_offset, request_length, &pages_in_transit);
    // The pages have now been moved to splice_list and pages_in_transit should be empty.
    DEBUG_ASSERT(list_is_empty(&pages_in_transit));
    uint64_t supplied_len = 0;
    // The splice_list being inserted has only true vm_page_t in it, and so SupplyPages will never
    // need to allocate or otherwise perform a partial success that would generate a page request.
    zx_status_t supply_result =
        cow_pages_->SupplyPages(VmCowRange(request_offset, request_length), &splice_list,
                                SupplyOptions::PhysicalPageProvider, &supplied_len, nullptr);
    ASSERT(supplied_len == request_length || supply_result != ZX_OK);
    if (supply_result != ZX_OK) {
      DEBUG_ASSERT(supply_result == ZX_ERR_NO_MEMORY);
      DEBUG_ASSERT(PageSource::IsValidInternalFailureCode(supply_result));
      // Since supplying pages didn't work, give up on this whole request and fail the whole range.
      // This also fails any current requests that overlap any part of this range.  Any page that
      // wasn't consumed by SupplyPages() can be re-loaned to keep the invariant that absent pages
      // in cow_pages_ are loaned.
      while (!splice_list.IsProcessed()) {
        VmPageOrMarker page_or_marker = splice_list.Pop();
        DEBUG_ASSERT(page_or_marker.IsPage());
        vm_page_t* p = page_or_marker.ReleasePage();
        DEBUG_ASSERT(!list_in_list(&p->queue_node));
        // Add the page back to pages_in_transit, which was empty before we entered this loop.
        list_add_tail(&pages_in_transit, &p->queue_node);
      }
      pmm_begin_loan(&pages_in_transit);
      page_source_->OnPagesFailed(request_offset, request_length, supply_result);
      // next request
    }
  }  // while have requests to process

  kcounter_add(physical_reclaim_total_requests, 1);
  // Will immediately return, because we've already processed all the requests that were pending
  // above (with success or failure).
  zx_status_t wait_result = event->Wait(Deadline::infinite());
  if (wait_result == ZX_OK) {
    kcounter_add(physical_reclaim_succeeded_requests, 1);
  } else {
    kcounter_add(physical_reclaim_failed_requests, 1);
  }
  if (wait_result != ZX_OK) {
    return wait_result;
  }
  return ZX_OK;
}

void PhysicalPageProvider::Dump(uint depth, uint32_t max_items) {
  Guard<Mutex> guard{&mtx_};
  dump::DepthPrinter printer(depth);
  printer.Emit("physical_page_provider %p cow_pages_ %p phys_base_ 0x%" PRIx64 " closed %d", this,
               cow_pages_, phys_base_, closed_);
  printer.BeginList(max_items);
  for (auto& req : pending_requests_) {
    DEBUG_ASSERT(SupportsPageRequestType(GetRequestType(&req)));
    printer.Emit("  pending req [0x%lx, 0x%lx)", GetRequestOffset(&req), GetRequestLen(&req));
  }
  printer.EndList();
}

bool PhysicalPageProvider::SupportsPageRequestType(page_request_type type) const {
  return type == page_request_type::READ;
}
