// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "vm/anonymous_page_request.h"

#include <kernel/lockdep.h>
#include <kernel/thread.h>
#include <vm/pmm.h>

zx_status_t AnonymousPageRequest::Wait() {
  DEBUG_ASSERT(active_);
  // Although the pmm_wait_till_free_pages call will unblock based on bounded kernel action, and not
  // some unbounded user request, the kernel might need to acquire arbitrary locks to achieve this.
  // Therefore blanket require no locks here to ensure no accidental lock dependencies. This can be
  // relaxed in the future if necessary.
  lockdep::AssertNoLocksHeld();

  // Time spent waiting is regarded as a memory stall.
  ScopedMemoryStall memory_stall;

  // This should only ever end up waiting momentarily until reclamation catches up. As such if we
  // end up waiting for a long time then this is probably a sign of a bug in reclamation somewhere,
  // so we want to make some noise here.
  constexpr zx_duration_mono_t kReportWaitTime = ZX_SEC(5);

  zx_status_t status = ZX_OK;
  uint32_t waited = 0;
  while ((status = pmm_wait_till_should_retry_single_alloc(
              Deadline::after_mono(kReportWaitTime))) == ZX_ERR_TIMED_OUT) {
    waited++;
    // If we've been waiting for more than 20 mins without being able to get more memory and without
    // declaring OOM, the memory watchdog is probably wedged and the system is in an unrecoverable
    // state. It would be nice to get some diagnostics here but we don't want to risk trying to
    // acquire any locks; just panic.
    if (waited >= ZX_MIN(20) / kReportWaitTime) {
      panic("AnonymousPageRequest waited for %" PRIi64 " seconds\n",
            (kReportWaitTime * waited) / ZX_SEC(1));
      return ZX_ERR_NO_MEMORY;
    }
    printf("WARNING: Waited %" PRIi64 " seconds to retry PMM allocations\n",
           (kReportWaitTime * waited) / ZX_SEC(1));
  }
  if (status == ZX_OK) {
    active_ = false;
  }
  return status;
}
