// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/fit/defer.h>
#include <lib/zircon-internal/macros.h>

#include <cassert>
#include <cstdint>

#include <kernel/lockdep.h>
#include <ktl/algorithm.h>
#include <vm/compression.h>
#include <vm/discardable_vmo_tracker.h>
#include <vm/evictor.h>
#include <vm/pmm.h>
#include <vm/pmm_node.h>
#include <vm/scanner.h>
#include <vm/stack_owned_loaned_pages_interval.h>
#include <vm/vm_cow_pages.h>

#include <ktl/enforce.h>

namespace {

KCOUNTER(pager_backed_pages_evicted, "vm.reclamation.pages_evicted_pager_backed.total")
KCOUNTER(pager_backed_pages_evicted_oom, "vm.reclamation.pages_evicted_pager_backed.oom")
KCOUNTER(compression_evicted, "vm.reclamation.pages_evicted_compressed.total")
KCOUNTER(compression_evicted_oom, "vm.reclamation.pages_evicted_compressed.oom")
KCOUNTER(discardable_pages_evicted, "vm.reclamation.pages_evicted_discardable.total")
KCOUNTER(discardable_pages_evicted_oom, "vm.reclamation.pages_evicted_discardable.oom")

inline void CheckedIncrement(uint64_t* a, uint64_t b) {
  uint64_t result;
  bool overflow = add_overflow(*a, b, &result);
  DEBUG_ASSERT(!overflow);
  *a = result;
}

ktl::optional<Evictor::EvictedPageCounts> ReclaimFromGlobalPageQueues(
    VmCompressor* compression_instance, Evictor::EvictionLevel eviction_level) {
  // Avoid evicting from the newest queue to prevent thrashing.
  const size_t lowest_evict_queue = eviction_level == Evictor::EvictionLevel::IncludeNewest
                                        ? PageQueues::kNumActiveQueues
                                        : PageQueues::kNumReclaim - PageQueues::kNumOldestQueues;
  // If we're going to include newest pages, ignore eviction hints as well, i.e. also consider
  // evicting pages with always_need set if we encounter them in LRU order.
  const VmCowPages::EvictionHintAction hint_action =
      eviction_level == Evictor::EvictionLevel::IncludeNewest
          ? VmCowPages::EvictionHintAction::Ignore
          : VmCowPages::EvictionHintAction::Follow;

  // TODO(rashaeqbal): The sequence of actions in PeekReclaim() and ReclaimPage() implicitly
  // guarantee forward progress if being repeatedly called, so that we're not stuck trying to evict
  // the same page (i.e. PeekReclaim keeps returning the same page). It would be nice to have
  // some explicit checks here (or in PageQueues) to guarantee forward progress. Or we might want
  // to use cursors to iterate the queues instead of peeking the tail each time.
  if (ktl::optional<PageQueues::VmoBacklink> backlink =
          pmm_page_queues()->PeekReclaim(lowest_evict_queue)) {
    // A valid backlink always has a valid cow
    DEBUG_ASSERT(backlink->cow);

    if (compression_instance) {
      zx_status_t status = compression_instance->Arm();
      if (status != ZX_OK) {
        return ktl::nullopt;
      }
    }
    VmCowPages::ReclaimCounts reclaimed = backlink->cow->ReclaimPage(
        backlink->page, backlink->offset, hint_action, compression_instance);

    return Evictor::EvictedPageCounts{
        .pager_backed = reclaimed.evicted_non_loaned,
        .pager_backed_loaned = reclaimed.evicted_loaned,
        .discardable = reclaimed.discarded,
        .compressed = reclaimed.compressed,
    };
  }
  return ktl::nullopt;
}

}  // namespace

// static
Evictor::EvictorStats Evictor::GetGlobalStats() {
  EvictorStats stats;
  stats.pager_backed_oom = pager_backed_pages_evicted_oom.SumAcrossAllCpus();
  stats.pager_backed_other = pager_backed_pages_evicted.SumAcrossAllCpus() - stats.pager_backed_oom;
  stats.compression_oom = compression_evicted_oom.SumAcrossAllCpus();
  stats.compression_other = compression_evicted.SumAcrossAllCpus() - stats.compression_oom;
  stats.discarded_oom = discardable_pages_evicted_oom.SumAcrossAllCpus();
  stats.discarded_other = discardable_pages_evicted.SumAcrossAllCpus() - stats.discarded_oom;
  return stats;
}

Evictor::Evictor() : Evictor(nullptr, nullptr) {}

Evictor::Evictor(ReclaimFunction reclaim_function, FreePagesFunction free_pages_function)
    : test_reclaim_function_(ktl::move(reclaim_function)),
      test_free_pages_function_(ktl::move(free_pages_function)) {}

Evictor::~Evictor() { DisableEviction(); }

bool Evictor::IsEvictionEnabled() const {
  Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
  return eviction_enabled_;
}

bool Evictor::IsCompressionEnabled() const {
  Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
  return use_compression_;
}

void Evictor::EnableEviction(bool use_compression) {
  {
    Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
    // It's an error to call this whilst the eviction thread is still exiting.
    ASSERT(!eviction_thread_exiting_);
    eviction_enabled_ = true;
    use_compression_ = use_compression;

    if (eviction_thread_) {
      return;
    }
  }

  // Set up the eviction thread to process asynchronous eviction requests.
  auto eviction_thread = [](void* arg) -> int {
    Evictor* evictor = reinterpret_cast<Evictor*>(arg);
    return evictor->EvictionThreadLoop();
  };
  eviction_thread_ = Thread::Create("eviction-thread", eviction_thread, this, LOW_PRIORITY);
  DEBUG_ASSERT(eviction_thread_);
  eviction_thread_->Resume();
}

void Evictor::DisableEviction() {
  Thread* eviction_thread = nullptr;
  {
    // Grab the lock and update any state. We cannot actually wait for the eviction thread to
    // complete whilst the lock is held, however.
    Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
    if (!eviction_thread_) {
      return;
    }
    // It's an error to call this in parallel with another DisableEviction call.
    ASSERT(!eviction_thread_exiting_);
    eviction_thread = eviction_thread_;
    eviction_thread_exiting_ = true;
    eviction_signal_.Signal();
  }
  // Now with the lock dropped wait for the thread to complete. Use a locally cached copy of the
  // pointer so that even if the scanner performs a concurrent EnableEviction call we should not
  // crash or have races, although the eviction thread may fail to join.
  int res = 0;
  eviction_thread->Join(&res, ZX_TIME_INFINITE);
  DEBUG_ASSERT(res == 0);
  {
    Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
    // Now update the state to indicate that eviction is disabled.
    eviction_thread_ = nullptr;
    eviction_enabled_ = false;
    eviction_thread_exiting_ = false;
  }
}

Evictor::EvictionTarget Evictor::DebugGetEvictionTarget() const {
  Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
  return eviction_target_;
}

void Evictor::CombineEvictionTarget(EvictionTarget target) {
  Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
  eviction_target_.pending = eviction_target_.pending || target.pending;
  eviction_target_.level = ktl::max(eviction_target_.level, target.level);
  CheckedIncrement(&eviction_target_.min_pages_to_free, target.min_pages_to_free);
  eviction_target_.free_pages_target =
      ktl::max(eviction_target_.free_pages_target, target.free_pages_target);
  eviction_target_.print_counts = eviction_target_.print_counts || target.print_counts;
}

Evictor::EvictedPageCounts Evictor::EvictFromExternalTarget(Evictor::EvictionTarget target) {
  return EvictFromTargetInternal(target);
}

Evictor::EvictedPageCounts Evictor::EvictFromPreloadedTarget() {
  // Create a local copy of the eviction target to operate against.
  EvictionTarget target;
  {
    Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
    target = eviction_target_;
  }
  EvictedPageCounts counts = EvictFromTargetInternal(target);
  {
    Guard<MonitoredSpinLock, IrqSave> guard{&lock_, SOURCE_TAG};
    uint64_t total = counts.compressed + counts.discardable + counts.pager_backed;
    // Clear the eviction target but retain any min pages that we might still need to free in a
    // subsequent eviction attempt.
    eviction_target_ = {};
    eviction_target_.min_pages_to_free =
        (total < target.min_pages_to_free) ? target.min_pages_to_free - total : 0;
  }
  return counts;
}

Evictor::EvictedPageCounts Evictor::EvictFromTargetInternal(Evictor::EvictionTarget target) {
  EvictedPageCounts total_evicted_counts = {};

  if (!target.pending) {
    return total_evicted_counts;
  }

  uint64_t free_pages_before = CountFreePages();

  total_evicted_counts =
      EvictUntilTargetsMet(target.min_pages_to_free, target.free_pages_target, target.level);

  if (target.print_counts) {
    printf("[EVICT]: Free memory before eviction was %zuMB and after eviction is %zuMB\n",
           free_pages_before * PAGE_SIZE / MB, CountFreePages() * PAGE_SIZE / MB);
    if (total_evicted_counts.pager_backed > 0) {
      printf("[EVICT]: Evicted %lu user pager backed pages\n", total_evicted_counts.pager_backed);
    }
    if (total_evicted_counts.discardable > 0) {
      printf("[EVICT]: Evicted %lu pages from discardable vmos\n",
             total_evicted_counts.discardable);
    }
    if (total_evicted_counts.compressed > 0) {
      printf("[EVICT]: Evicted %lu pages by compression\n", total_evicted_counts.compressed);
    }
  }

  if (target.oom_trigger) {
    pager_backed_pages_evicted_oom.Add(static_cast<int64_t>(total_evicted_counts.pager_backed));
    compression_evicted_oom.Add(static_cast<int64_t>(total_evicted_counts.compressed));
    discardable_pages_evicted_oom.Add(static_cast<int64_t>(total_evicted_counts.discardable));
  }

  return total_evicted_counts;
}

uint64_t Evictor::EvictSynchronous(uint64_t min_mem_to_free, EvictionLevel eviction_level,
                                   Output output, TriggerReason reason) {
  if (!IsEvictionEnabled()) {
    return 0;
  }
  EvictionTarget target = {
      .pending = true,
      // No target free pages to get to. Evict based only on the min pages requested to evict.
      .free_pages_target = 0,
      // For synchronous eviction, set the eviction level and min target as requested.
      .min_pages_to_free = min_mem_to_free / PAGE_SIZE,
      .level = eviction_level,
      .print_counts = (output == Output::Print),
      .oom_trigger = (reason == TriggerReason::OOM),
  };

  auto evicted_counts = EvictFromExternalTarget(target);
  return evicted_counts.pager_backed + evicted_counts.discardable + evicted_counts.compressed;
}

void Evictor::EvictAsynchronous(uint64_t min_mem_to_free, uint64_t free_mem_target,
                                Evictor::EvictionLevel eviction_level, Evictor::Output output) {
  if (!IsEvictionEnabled()) {
    return;
  }
  CombineEvictionTarget(Evictor::EvictionTarget{
      .pending = true,
      .free_pages_target = free_mem_target / PAGE_SIZE,
      .min_pages_to_free = min_mem_to_free / PAGE_SIZE,
      .level = eviction_level,
      .print_counts = (output == Output::Print),
  });
  // Unblock the eviction thread.
  eviction_signal_.Signal();
}

Evictor::EvictedPageCounts Evictor::EvictUntilTargetsMet(uint64_t min_pages_to_evict,
                                                         uint64_t free_pages_target,
                                                         EvictionLevel level) {
  EvictedPageCounts total_evicted_counts = {};
  if (!IsEvictionEnabled()) {
    return total_evicted_counts;
  }

  // Wait until no eviction attempts are ongoing, so that we don't overshoot the free pages target.
  no_ongoing_eviction_.Wait(Deadline::infinite());
  auto signal_cleanup = fit::defer([&]() {
    // Unblock any waiting eviction requests.
    no_ongoing_eviction_.Signal();
  });

  uint64_t total_non_loaned_pages_freed = 0;

  while (true) {
    const uint64_t free_pages = CountFreePages();
    uint64_t pages_to_free = 0;
    if (total_non_loaned_pages_freed < min_pages_to_evict) {
      pages_to_free = min_pages_to_evict - total_non_loaned_pages_freed;
    } else if (free_pages < free_pages_target) {
      pages_to_free = free_pages_target - free_pages;
    } else {
      // The targets have been met. No more eviction is required right now.
      break;
    }

    EvictedPageCounts pages_freed = EvictPageQueues(pages_to_free, level);
    const uint64_t non_loaned_evicted =
        pages_freed.pager_backed + pages_freed.compressed + pages_freed.discardable;
    total_evicted_counts += pages_freed;
    total_non_loaned_pages_freed += non_loaned_evicted;

    // Should we fail to free any pages then we give up and consider the eviction request complete.
    if (non_loaned_evicted == 0) {
      break;
    }
  }

  return total_evicted_counts;
}

Evictor::EvictedPageCounts Evictor::EvictPageQueues(uint64_t target_pages,
                                                    EvictionLevel eviction_level) const {
  EvictedPageCounts counts = {};

  if (!IsEvictionEnabled()) {
    return counts;
  }

  ktl::optional<VmCompression::CompressorGuard> maybe_instance;
  VmCompressor* compression_instance = nullptr;
  if (IsCompressionEnabled()) {
    VmCompression* compression = pmm_page_compression();
    if (compression) {
      maybe_instance.emplace(compression->AcquireCompressor());
      compression_instance = &maybe_instance->get();
    }
  }

  // Evict until we've counted enough pages to hit the target_pages. Explicitly do not consider
  // pager_backed_loaned towards our total, as loaned pages do not go to the free memory pool.
  while (counts.pager_backed + counts.compressed + counts.discardable < target_pages) {
    // Use the helper to perform a single 'step' of eviction.
    ktl::optional<EvictedPageCounts> reclaimed =
        EvictPageQueuesHelper(compression_instance, eviction_level);
    // An empty return from the helper indicates that there are no more eviction candidates, so
    // regardless of our desired target we must give up.
    if (!reclaimed.has_value()) {
      break;
    }
    counts += *reclaimed;
  }

  pager_backed_pages_evicted.Add(counts.pager_backed + counts.pager_backed_loaned);
  compression_evicted.Add(counts.compressed);
  return counts;
}

int Evictor::EvictionThreadLoop() {
  while (!eviction_thread_exiting_) {
    eviction_signal_.Wait();

    if (eviction_thread_exiting_) {
      break;
    }

    // Process an eviction target if there is one. This is a no-op and no pages are evicted if no
    // target is pending.
    EvictFromPreloadedTarget();
  }
  return 0;
}

uint64_t Evictor::CountFreePages() const {
  if (unlikely(test_free_pages_function_)) {
    return test_free_pages_function_();
  }
  return pmm_count_free_pages();
}

ktl::optional<Evictor::EvictedPageCounts> Evictor::EvictPageQueuesHelper(
    VmCompressor* compression_instance, EvictionLevel eviction_level) const {
  if (unlikely(test_reclaim_function_)) {
    return test_reclaim_function_(compression_instance, eviction_level);
  }
  return ReclaimFromGlobalPageQueues(compression_instance, eviction_level);
}
