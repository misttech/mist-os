// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "vm/anonymous_page_requester.h"

#include <lib/lazy_init/lazy_init.h>

#include <vm/pmm.h>

namespace {
lazy_init::LazyInit<AnonymousPageRequester> anonymous_page_requester;
// Need to hold one refcount permanently so that other RefPtrs to the requester can be created and
// deleted.
lazy_init::LazyInit<fbl::RefPtr<AnonymousPageRequester>> anonymous_page_requester_ref;

}  // namespace

zx_status_t AnonymousPageRequester::FillRequest(PageRequest* request) {
  DEBUG_ASSERT(!request->IsInitialized());
  // Pretend a read request at offset 0. The only actor that should ever inspect these values is
  // us, and we don't, so they can be anything.
  request->Init(fbl::RefPtr<PageRequestInterface>(this), 0, page_request_type::READ,
                VmoDebugInfo{0, 0});
  return ZX_ERR_SHOULD_WAIT;
}

zx_status_t AnonymousPageRequester::WaitOnRequest(PageRequest* request) {
  // Although the pmm_wait_till_free_pages call will unblock based on bounded kernel action, and not
  // some unbounded user request, the kernel might need to acquire arbitrary locks to achieve this.
  // Therefore blanket require no locks here to ensure no accidental lock dependencies. This can be
  // relaxed in the future if necessary.
  lockdep::AssertNoLocksHeld();

  // This should only ever end up waiting momentarily until reclamation catches up. As such if we
  // end up waiting for a long time then this is probably a sign of a bug in reclamation somewhere,
  // so we want to make some noise here.
  constexpr zx_duration_t kReportWaitTime = ZX_SEC(5);

  zx_status_t status = ZX_OK;
  uint32_t waited = 0;
  while ((status = pmm_wait_till_should_retry_single_alloc(Deadline::after(kReportWaitTime))) ==
         ZX_ERR_SHOULD_WAIT) {
    waited++;
    printf("WARNING: Waited %" PRIi64 " seconds to retry PMM allocations\n",
           (kReportWaitTime * waited) / ZX_SEC(1));
  }
  // Whether we succeeded or failed, this request is finished so clear out offset_.
  request->offset_ = UINT64_MAX;
  return status;
}

// static
AnonymousPageRequester& AnonymousPageRequester::Get() { return anonymous_page_requester.Get(); }

// static
void AnonymousPageRequester::Init() {
  anonymous_page_requester.Initialize();
  anonymous_page_requester_ref.Initialize(fbl::AdoptRef(&anonymous_page_requester.Get()));
}
