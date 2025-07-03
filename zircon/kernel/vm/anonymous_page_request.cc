// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "vm/anonymous_page_request.h"

#include <lib/fit/defer.h>

#include <kernel/lockdep.h>
#include <kernel/thread.h>
#include <vm/pmm.h>
#include <vm/scanner.h>

namespace {
// Atomic token used to ensure that only one thread performs the informational dump so that in the
// scenario of many threads all reaching the timeout at the same time there is not a rush of spam.
ktl::atomic<bool> dump_info_before_panic_token = true;
}  // namespace

zx::result<> AnonymousPageRequest::Allocate() {
  DEBUG_ASSERT(active_);
  DEBUG_ASSERT(!has_page());
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
  constexpr unsigned int kMaxWaits = ZX_SEC(30) / kReportWaitTime;
  static_assert(kMaxWaits >= 1);
  // Trigger an informational dump 1 wait prior to triggering a panic. In the case of many threads
  // waiting this gives a chance for the informational dump to happen before a different thread
  // triggers the panic.
  constexpr unsigned int kDumpInfoWaits = kMaxWaits - 1;
  uint32_t waited = 0;

  // Once we return, clear the `active_` flag.
  auto defer = fit::defer([this]() { active_ = false; });
  while (true) {
    zx_status_t wait_result =
        Pmm::Node().WaitTillShouldRetrySingleAlloc(Deadline::after_mono(kReportWaitTime));
    if (wait_result == ZX_ERR_TIMED_OUT) {
      waited++;
      dprintf(INFO, "WARNING: Waited %" PRIi64 " seconds to retry PMM allocations\n",
              (kReportWaitTime * waited) / ZX_SEC(1));

      // If we've reached the threshold to dump information *and* we're the first thread to do so,
      // i.e. first to get the token, then ask the scanner to do a pre panic dump. The assumption
      // here is that if we've been blocked this long then we are most likely going to hit kMaxWaits
      // and so it's worth performing a slightly unsafe informational dump for the purposes of
      // debugging.
      if (waited == kDumpInfoWaits && dump_info_before_panic_token.exchange(false)) {
        scanner_debug_dump_state_before_panic();
      }

      // If we've been waiting for a while without being able to get more memory and
      // without declaring OOM, the memory watchdog is probably wedged and the system is in an
      // unrecoverable state. It would be nice to get some diagnostics here but we don't want to
      // risk trying to acquire any locks; just panic.
      ZX_ASSERT_MSG(waited < kMaxWaits, "Allocation stalled for %" PRIi64 " seconds",
                    (kReportWaitTime * waited) / ZX_SEC(1));
      continue;
    }

    // Shouldn't be possible, since we only signal `ZX_OK`.
    if (wait_result != ZX_OK) {
      return zx::error(wait_result);
    }

    // Try to allocate the page now, it may fail sporadically, since there is no guarantee that
    // by the time we attempt to allocate the pages are still available.
    zx::result<vm_page_t*> res = Pmm::Node().AllocPage(PMM_ALLOC_FLAG_CAN_WAIT);
    if (res.status_value() == ZX_ERR_SHOULD_WAIT) {
      continue;
    }

    // Let the caller decide if we should retry or not.
    if (res.is_error()) {
      return res.take_error();
    }

    page_ = res.value();
    return zx::ok();
  }

  __UNREACHABLE;
}

void AnonymousPageRequest::FreePage() { Pmm::Node().FreePage(take_page()); }
