// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/intrin.h>
#include <lib/counters.h>
#include <lib/fit/defer.h>
#include <lib/zircon-internal/macros.h>

#include <fbl/ref_counted_upgradeable.h>
#include <kernel/auto_preempt_disabler.h>
#include <object/thread_dispatcher.h>
#include <vm/compression.h>
#include <vm/page.h>
#include <vm/page_queues.h>
#include <vm/pmm.h>
#include <vm/scanner.h>
#include <vm/vm_cow_pages.h>

namespace {

KCOUNTER(pq_aging_reason_before_min_timeout, "pq.aging.reason_before_min_timeout")
KCOUNTER(pq_aging_spurious_wakeup, "pq.aging.spurious_wakeup")
KCOUNTER(pq_aging_reason_timeout, "pq.aging.reason.timeout")
KCOUNTER(pq_aging_reason_active_ratio, "pq.aging.reason.active_ratio")
KCOUNTER(pq_aging_reason_manual, "pq.aging.reason.manual")
KCOUNTER(pq_aging_blocked_on_lru, "pq.aging.blocked_on_lru")
KCOUNTER(pq_lru_spurious_wakeup, "pq.lru.spurious_wakeup")
KCOUNTER(pq_lru_pages_evicted, "pq.lru.pages_evicted")
KCOUNTER(pq_lru_pages_compressed, "pq.lru.pages_compressed")
KCOUNTER(pq_lru_pages_discarded, "pq.lru.pages_discarded")
KCOUNTER(pq_accessed_normal, "pq.accessed.normal")
KCOUNTER(pq_accessed_normal_same_queue, "pq.accessed.normal_same_queue")

// Helper class for building an isolate list for deferred processing when acting on the LRU queues.
// Pages are added while the page queues lock is held, and processed once the lock is dropped.
// Statically sized with the maximum number of items it might need to hold and it is an error to
// attempt to add more than this many items, as Flush() cannot automatically be called due to
// incompatible locking requirements between flushing and adding items.
template <size_t Items>
class LruIsolate {
 public:
  using LruAction = PageQueues::LruAction;
  LruIsolate() = default;
  ~LruIsolate() { Flush(); }
  // Sets the LRU action, this allows the object construction to happen without the page queues
  // lock, where as setting the LruAction can be done within it.
  void SetLruAction(LruAction lru_action) { lru_action_ = lru_action; }

  // Adds a page to be potentially replaced with a loaned page.
  // Requires PageQueues lock to be held
  void AddLoanReplacement(vm_page_t* page, PageQueues* pq) TA_REQ(pq->get_lock()) {
    DEBUG_ASSERT(page);
    DEBUG_ASSERT(!page->is_loaned());
    VmCowPages* cow = reinterpret_cast<VmCowPages*>(page->object.get_object());
    DEBUG_ASSERT(cow);
    fbl::RefPtr<VmCowPages> cow_ref = fbl::MakeRefPtrUpgradeFromRaw(cow, pq->get_lock());
    DEBUG_ASSERT(cow_ref);
    AddInternal(ktl::move(cow_ref), page, ListAction::ReplaceWithLoaned);
  }

  // Add a page to be reclaimed. Actual reclamation will only be done if the `SetLruAction` is
  // compatible with the page and its VMO owner.
  // Requires PageQueues lock to be held
  void AddReclaimable(vm_page_t* page, PageQueues* pq) TA_REQ(pq->get_lock()) {
    DEBUG_ASSERT(page);
    if (lru_action_ == LruAction::None) {
      return;
    }
    VmCowPages* cow = reinterpret_cast<VmCowPages*>(page->object.get_object());
    DEBUG_ASSERT(cow);
    // Need to get the cow refptr before we can check if our lru action is appropriate for this
    // page.
    fbl::RefPtr<VmCowPages> cow_ref = fbl::MakeRefPtrUpgradeFromRaw(cow, pq->get_lock());
    DEBUG_ASSERT(cow_ref);
    if (lru_action_ == LruAction::EvictAndCompress ||
        ((cow_ref->can_evict() || cow_ref->is_discardable()) ==
         (lru_action_ == LruAction::EvictOnly))) {
      AddInternal(ktl::move(cow_ref), page, ListAction::Reclaim);
    } else {
      // Must not let the cow refptr get dropped till after the lock, so even if not
      // reclaiming must keep this entry.
      AddInternal(ktl::move(cow_ref), page, ListAction::None);
    }
  }

  // Performs any pending operations on the stored pages.
  // Requires PageQueues lock NOT be held
  void Flush() {
    // Cannot check if the page queues lock specifically is held, but can validate that *no*
    // spinlocks at all are held, which also needs to be true for us to acquire VMO locks.
    DEBUG_ASSERT(arch_num_spinlocks_held() == 0);
    // Compression state will be lazily instantiate if needed, and then used for any remaining
    // pages in the list.
    VmCompression* compression = nullptr;
    ktl::optional<VmCompression::CompressorGuard> maybe_compressor;
    VmCompressor* compressor = nullptr;

    for (size_t i = 0; i < items_; ++i) {
      auto [backlink, action] = ktl::move(list_[i]);
      DEBUG_ASSERT(backlink.cow);
      if (action == ListAction::ReplaceWithLoaned) {
        // We ignore the return value because the page may have moved, become pinned, we may not
        // have any free loaned pages any more, or the VmCowPages may not be able to borrow.
        backlink.cow->ReplacePageWithLoaned(backlink.page, backlink.offset);
      } else if (action == ListAction::Reclaim) {
        // Attempt to acquire any compressor that might exist, unless only evicting. Note that if
        // LruAction::None we would not have enqueued any Reclaim pages, so we can just check for
        // EvictOnly.
        if (lru_action_ != LruAction::EvictOnly && !compression) {
          compression = Pmm::Node().GetPageCompression();
          if (compression) {
            maybe_compressor.emplace(compression->AcquireCompressor());
            compressor = &maybe_compressor->get();
          }
        }
        // If using a compressor, make sure it is Armed between reclamations.
        if (compressor) {
          zx_status_t status = compressor->Arm();
          if (status != ZX_OK) {
            // Continue processing as we might still be able to evict and we need to clear all the
            // refptrs as well.
            continue;
          }
        }
        if (VmCowPages::ReclaimCounts count = backlink.cow->ReclaimPage(
                backlink.page, backlink.offset, VmCowPages::EvictionAction::FollowHint, compressor);
            count.Total() > 0) {
          pq_lru_pages_evicted.Add(count.evicted_non_loaned + count.evicted_loaned);
          pq_lru_pages_discarded.Add(count.discarded);
          pq_lru_pages_compressed.Add(count.compressed);
        }
      }
    }
    items_ = 0;
  }

 private:
  // The None is needed since to know if a page can be reclaimed by the current LruAction a RefPtr
  // to the VMO must first be created. If the page shouldn't be reclaimed the RefPtr must not be
  // dropped till outside the lock, in case it's the last ref. The None action provides a way to
  // retain these RefPtrs and have them dropped outside the lock.
  enum class ListAction {
    None,
    ReplaceWithLoaned,
    Reclaim,
  };

  void AddInternal(fbl::RefPtr<VmCowPages>&& cow, vm_page_t* page, ListAction action) {
    DEBUG_ASSERT(cow);
    DEBUG_ASSERT(items_ < list_.size());
    if (cow) {
      list_[items_] = {PageQueues::VmoBacklink{cow, page, page->object.get_page_offset()}, action};
      items_++;
    }
  }

  // Cache of the PageQueues LruAction for checking what to do with different reclaimable pages.
  LruAction lru_action_ = LruAction::None;
  // List of pages and the actions to perform on them.
  ktl::array<ktl::pair<PageQueues::VmoBacklink, ListAction>, Items> list_;
  // Number of items in the list_.
  size_t items_ = 0;
};

}  // namespace

// static
uint64_t PageQueues::GetLruPagesCompressed() { return pq_lru_pages_compressed.SumAcrossAllCpus(); }

PageQueues::PageQueues()
    : min_mru_rotate_time_(kDefaultMinMruRotateTime),
      max_mru_rotate_time_(kDefaultMaxMruRotateTime),
      active_ratio_multiplier_(kDefaultActiveRatioMultiplier) {
  for (uint32_t i = 0; i < PageQueueNumQueues; i++) {
    list_initialize(&page_queues_[i]);
  }
  for (uint32_t i = 0; i < kNumIsolateQueues; i++) {
    list_initialize(&isolate_queues_[i]);
  }
}

PageQueues::~PageQueues() {
  StopThreads();
  for (uint32_t i = 0; i < PageQueueNumQueues; i++) {
    DEBUG_ASSERT(list_is_empty(&page_queues_[i]));
  }
  for (size_t i = 0; i < page_queue_counts_.size(); i++) {
    DEBUG_ASSERT_MSG(page_queue_counts_[i] == 0, "i=%zu count=%zu", i,
                     page_queue_counts_[i].load());
  }
}

void PageQueues::StartThreads(zx_duration_mono_t min_mru_rotate_time,
                              zx_duration_mono_t max_mru_rotate_time) {
  // Clamp the max rotate to the minimum.
  max_mru_rotate_time = ktl::max(min_mru_rotate_time, max_mru_rotate_time);
  // Prevent a rotation rate that is too small.
  max_mru_rotate_time = ktl::max(max_mru_rotate_time, ZX_SEC(1));

  min_mru_rotate_time_ = min_mru_rotate_time;
  max_mru_rotate_time_ = max_mru_rotate_time;

  // Cannot perform all of thread creation under the lock as thread creation requires
  // allocations so we create in temporaries first and then stash.
  Thread* mru_thread = Thread::Create(
      "page-queue-mru-thread",
      [](void* arg) -> int {
        static_cast<PageQueues*>(arg)->MruThread();
        return 0;
      },
      this, LOW_PRIORITY);
  DEBUG_ASSERT(mru_thread);

  mru_thread->Resume();

  Thread* lru_thread = Thread::Create(
      "page-queue-lru-thread",
      [](void* arg) -> int {
        static_cast<PageQueues*>(arg)->LruThread();
        return 0;
      },
      this, LOW_PRIORITY);
  DEBUG_ASSERT(lru_thread);
  lru_thread->Resume();

  {
    Guard<CriticalMutex> guard{&lock_};
    ASSERT(!mru_thread_);
    ASSERT(!lru_thread_);
    mru_thread_ = mru_thread;
    lru_thread_ = lru_thread;
  }
  // Kick start any LRU processing that might be pending to ensure it doesn't spuriously timeout.
  MaybeTriggerLruProcessing();
}

void PageQueues::StartDebugCompressor() {
  // The debug compressor should not be enabled without debug asserts as we guard all usages of the
  // debug compressor with compile time checks so that it cannot impact the performance of release
  // versions.
  ASSERT(DEBUG_ASSERT_IMPLEMENTED);
#if DEBUG_ASSERT_IMPLEMENTED
  fbl::AllocChecker ac;
  ktl::unique_ptr<VmDebugCompressor> dc(new (&ac) VmDebugCompressor);
  if (!ac.check()) {
    panic("Failed to allocate VmDebugCompressor");
  }
  zx_status_t status = dc->Init();
  ASSERT(status == ZX_OK);
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  debug_compressor_ = ktl::move(dc);
#endif
}

void PageQueues::StopThreads() {
  // Cannot wait for threads to complete with the lock held, so update state and then perform any
  // joins outside the lock.
  Thread* mru_thread = nullptr;
  Thread* lru_thread = nullptr;

  {
    bool signal_aging;
    {
      Guard<CriticalMutex> guard{&lock_};
      shutdown_threads_ = true;
      signal_aging = aging_disabled_.exchange(false);
      mru_thread = mru_thread_;
      lru_thread = lru_thread_;
    }
    if (signal_aging) {
      aging_token_.Signal();
    }
    aging_active_ratio_event_.Signal();
    lru_event_.Signal();
  }

  int retcode;
  if (mru_thread) {
    zx_status_t status = mru_thread->Join(&retcode, ZX_TIME_INFINITE);
    ASSERT(status == ZX_OK);
  }
  if (lru_thread) {
    zx_status_t status = lru_thread->Join(&retcode, ZX_TIME_INFINITE);
    ASSERT(status == ZX_OK);
  }
}

void PageQueues::SetLruAction(LruAction action) {
  Guard<CriticalMutex> guard{&lock_};
  lru_action_ = action;
}

void PageQueues::SetActiveRatioMultiplier(uint32_t multiplier) {
  Guard<CriticalMutex> guard{&lock_};
  active_ratio_multiplier_ = multiplier;
  // The change in multiplier might have caused us to need to age.
  CheckActiveRatioAgingLocked();
}

void PageQueues::CheckActiveRatioAgingLocked() {
  if (active_ratio_triggered_) {
    // Already triggered, nothing more to do.
    return;
  }
  if (IsActiveRatioTriggeringAging()) {
    active_ratio_triggered_ = true;
    aging_active_ratio_event_.Signal();
  }
}

bool PageQueues::IsActiveRatioTriggeringAging() {
  ActiveInactiveCounts counts = GetActiveInactiveCounts();
  return counts.active * active_ratio_multiplier_ > counts.inactive;
}

ktl::variant<PageQueues::AgeReason, zx_instant_mono_t> PageQueues::ConsumeAgeReason() {
  AutoPreemptDisabler apd;
  Guard<CriticalMutex> guard{&lock_};
  auto reason = GetAgeReasonLocked();
  // If the age reason is the active ratio, consume the trigger.
  if (const AgeReason* age_reason = ktl::get_if<AgeReason>(&reason)) {
    no_pending_aging_signal_.Unsignal();
    if (*age_reason == AgeReason::ActiveRatio) {
      active_ratio_triggered_ = false;
      aging_active_ratio_event_.Unsignal();
    }
  } else {
    no_pending_aging_signal_.Signal();
  }
  return reason;
}

void PageQueues::SynchronizeWithAging() {
  while (true) {
    // Wait for any in progress aging to complete. This is not an Autounsignal event and so waiting
    // on it without the lock is not manipulating its state.
    constexpr int kWarnTimeoutSeconds = 10;
    zx_status_t status =
        no_pending_aging_signal_.Wait(Deadline::after_mono(ZX_SEC(kWarnTimeoutSeconds)));
    if (status == ZX_ERR_TIMED_OUT) {
      printf("[pq]: WARNING Waited %d seconds so far for aging to complete\n", kWarnTimeoutSeconds);
      // Only warn once, wait now with an infinite timeout.
      no_pending_aging_signal_.Wait();
    }

    // The MruThread may not have woken up yet to clear the pending signal, so we must check
    // ourselves.
    Guard<CriticalMutex> guard{&lock_};
    if (!ktl::holds_alternative<AgeReason>(GetAgeReasonLocked())) {
      // There is no aging reason, so there is no race to worry about, and no aging can be in
      // progress.
      return;
    }
    // We may have raced with the MruThread. Either it has already seen that there is an AgeReason
    // and cleared the this signal, or it is still pending to be scheduled and clear it. If it
    // already cleared it, then us clearing it again is harmless, and if it is still waiting to run
    // by clearing it we can then Wait on the event, knowing once the MruThread finishes performing
    // aging it will do the signal.
    // Since we hold the lock, and know there is an age reason, we know that we are not racing with
    // the signal being set, and so cannot lose a signal here.
    no_pending_aging_signal_.Unsignal();
  }
}

ktl::variant<PageQueues::AgeReason, zx_instant_mono_t> PageQueues::GetAgeReasonLocked() const {
  const zx_instant_mono_t current = current_mono_time();
  // Check if there is an active ratio that wants us to age.
  if (active_ratio_triggered_) {
    // Need to have passed the min time though.
    const zx_instant_mono_t min_timeout =
        zx_time_add_duration(last_age_time_.load(ktl::memory_order_relaxed), min_mru_rotate_time_);
    if (current < min_timeout) {
      return min_timeout;
    }
    // At least min time has elapsed, can age via active ratio.
    return AgeReason::ActiveRatio;
  }

  // Exceeding the maximum time forces aging.
  const zx_instant_mono_t max_timeout =
      zx_time_add_duration(last_age_time_.load(ktl::memory_order_relaxed), max_mru_rotate_time_);
  if (max_timeout <= current) {
    return AgeReason::Timeout;
  }
  // With no other reason, we will age once we hit the maximum timeout.
  return max_timeout;
}

void PageQueues::MaybeTriggerLruProcessing() {
  bool needs_lru_processing;
  {
    Guard<CriticalMutex> guard{&lock_};
    needs_lru_processing = NeedsLruProcessingLocked();
  }
  if (needs_lru_processing) {
    lru_event_.Signal();
  }
}

bool PageQueues::NeedsLruProcessingLocked() const {
  // Currently only reason to trigger lru processing is if the MRU needs space. This requires the
  // lock since the typical use case wants an ordering of any changes to the generation counts with
  // respect to this query. This works since the generation counts are also modified with the lock
  // held.
  if (mru_gen_.load(ktl::memory_order_relaxed) - lru_gen_.load(ktl::memory_order_relaxed) ==
      kNumReclaim - 1) {
    return true;
  }
  return false;
}

void PageQueues::DisableAging() {
  // Validate a double DisableAging is not happening.
  if (aging_disabled_.exchange(true)) {
    panic("Mismatched disable/enable pair");
  }

  // Take the aging token. This will both wait for the aging thread to complete any in progress
  // aging, and prevent it from aging until we return it.
  aging_token_.Wait();
#if DEBUG_ASSERT_IMPLEMENTED
  // Pause might drop the last reference to a VMO and trigger VMO destruction, which would then call
  // back into the page queues, so we must not hold the lock_ over the operation. We can utilize the
  // fact that once the debug_compressor_ is set it is never destroyed, so can take a raw pointer to
  // it.
  VmDebugCompressor* dc = nullptr;
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    if (debug_compressor_) {
      dc = &*debug_compressor_;
    }
  }
  if (dc) {
    dc->Pause();
  }
#endif
}

void PageQueues::EnableAging() {
  // Validate a double EnableAging is not happening.
  if (!aging_disabled_.exchange(false)) {
    panic("Mismatched disable/enable pair");
  }

  // Return the aging token, allowing the aging thread to proceed if it was waiting.
  aging_token_.Signal();
#if DEBUG_ASSERT_IMPLEMENTED
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  if (debug_compressor_) {
    debug_compressor_->Resume();
  }
#endif
}

const char* PageQueues::string_from_age_reason(PageQueues::AgeReason reason) {
  switch (reason) {
    case AgeReason::ActiveRatio:
      return "Active ratio";
    case AgeReason::Timeout:
      return "Timeout";
    case AgeReason::Manual:
      return "Manual";
    default:
      panic("Unreachable");
  }
}

void PageQueues::Dump() {
  // Need to grab a copy of all the counts and generations. As the lock is needed to acquire the
  // active/inactive counts, also hold the lock over the copying of the counts to avoid needless
  // races.
  uint64_t mru_gen;
  uint64_t lru_gen;
  size_t counts[kNumReclaim] = {};
  size_t inactive_count;
  size_t failed_reclaim;
  size_t dirty;
  zx_instant_mono_t last_age_time;
  AgeReason last_age_reason;
  ActiveInactiveCounts activeinactive;
  {
    Guard<CriticalMutex> guard{&lock_};
    mru_gen = mru_gen_.load(ktl::memory_order_relaxed);
    lru_gen = lru_gen_.load(ktl::memory_order_relaxed);
    failed_reclaim = page_queue_counts_[PageQueueFailedReclaim].load(ktl::memory_order_relaxed);
    inactive_count = page_queue_counts_[PageQueueReclaimIsolate].load(ktl::memory_order_relaxed);
    dirty = page_queue_counts_[PageQueuePagerBackedDirty].load(ktl::memory_order_relaxed);
    for (uint32_t i = 0; i < kNumReclaim; i++) {
      counts[i] = page_queue_counts_[PageQueueReclaimBase + i].load(ktl::memory_order_relaxed);
    }
    activeinactive = GetActiveInactiveCounts();
    last_age_time = last_age_time_.load(ktl::memory_order_relaxed);
    last_age_reason = last_age_reason_;
  }
  // Small arbitrary number that should be more than large enough to hold the constructed string
  // without causing stack allocation pressure.
  constexpr size_t kBufSize = 50;
  // Start with the buffer null terminated. snprintf will always keep it null terminated.
  char buf[kBufSize] __UNINITIALIZED = "\0";
  size_t buf_len = 0;
  // This formats the counts of all buckets, not just those within the mru->lru range, even though
  // any buckets not in that range should always have a count of zero. The format this generates is
  // [active],[active],inactive,inactive,{last inactive},should-be-zero,should-be-zero
  // Although the inactive and should-be-zero use the same formatting, they are broken up by the
  // {last inactive}.
  for (uint64_t i = 0; i < kNumReclaim; i++) {
    PageQueue queue = gen_to_queue(mru_gen - i);
    ASSERT(buf_len < kBufSize);
    const size_t remain = kBufSize - buf_len;
    int write_len;
    if (i < kNumActiveQueues) {
      write_len = snprintf(buf + buf_len, remain, "[%zu],", counts[queue - PageQueueReclaimBase]);
    } else if (i == mru_gen - lru_gen) {
      write_len = snprintf(buf + buf_len, remain, "{%zu},", counts[queue - PageQueueReclaimBase]);
    } else {
      write_len = snprintf(buf + buf_len, remain, "%zu,", counts[queue - PageQueueReclaimBase]);
    }
    // Negative values are returned on encoding errors, which we never expect to get.
    ASSERT(write_len >= 0);
    if (static_cast<uint>(write_len) >= remain) {
      // Buffer too small, just use whatever we have constructed so far.
      break;
    }
    buf_len += write_len;
  }
  zx_instant_mono_t current = current_mono_time();
  timespec age_time = zx_timespec_from_duration(zx_time_sub_time(current, last_age_time));
  printf("pq: MRU generation is %" PRIu64
         " set %ld.%lds ago due to \"%s\", LRU generation is %" PRIu64 "\n",
         mru_gen, age_time.tv_sec, age_time.tv_nsec, string_from_age_reason(last_age_reason),
         lru_gen);
  printf("pq: Pager buckets %s evict first: %zu\n", buf, inactive_count);
  printf("pq: active/inactive totals: %zu/%zu dirty: %zu failed reclaim: %zu\n",
         activeinactive.active, activeinactive.inactive, dirty, failed_reclaim);
}

// This runs the aging thread. Aging, unlike lru processing, scanning or eviction, requires very
// little work and is more about coordination. As such this thread is heavy on checks and signalling
// but generally only needs to hold any locks for the briefest of times.
// There is, currently, one exception to that, which is the calls to scanner_wait_for_accessed_scan.
// The scanner will, eventually, be a separate thread that is synchronized with, but presently
// a full scan may happen inline in that method call, and get attributed directly to this thread.
void PageQueues::MruThread() {
  // Pretend that aging happens during startup to simplify the rest of the loop logic.
  last_age_time_ = current_mono_time();
  unsigned int iterations_since_last_age = 0;
  while (!shutdown_threads_.load(ktl::memory_order_relaxed)) {
    // Normally we should retry the loop at most once (i.e. pass this line of code twice) if an
    // active ratio was triggered (kicking us out of the event), but we still needed to wait for the
    // min timeout. In this case in the first pass we do not get an age reason, wake up on the event
    // then perform the Sleep, come back around the loop and can now get an age reason.
    //
    // Unfortunately due to the way DeferredPendingSignals works, there is race where a thread can
    // set `active_ratio_triggered_`, but fail to actually signal the event before being preempted.
    // It is possible for us to then call ConsumeAgeReason and perform the aging without waiting on
    // the event. At some later point that first thread could finally deliver the signal, spuriously
    // waking us up. In an extremely unlikely event there could be multiple threads queued up in
    // this state to deliver an unbounded number of late signals. This is extremely unlikely though
    // and would require some precise scheduling behavior. Nevertheless it is technically possible
    // and so we just print a warning that it has happened and do not generate any errors.
    if (iterations_since_last_age == 10) {
      printf("%s iterated %u times, possible bug or overloaded system", __FUNCTION__,
             iterations_since_last_age);
    }
    // Check if there is an age reason waiting for us, consuming if there is, or if we need to wait.
    auto reason_or_timeout = ConsumeAgeReason();
    if (const zx_instant_mono_t* age_deadline =
            ktl::get_if<zx_instant_mono_t>(&reason_or_timeout)) {
      // Wait for this time, ensuring we wake up if the active ratio should change.
      zx_status_t result = aging_active_ratio_event_.WaitDeadline(*age_deadline, Interruptible::No);
      // Check if shutdown has been requested, we need this extra check even though it is part of
      // the main loop check to ensure that we do not perform the minimal rotate time sleep with a
      // shutdown pending.
      if (shutdown_threads_.load(ktl::memory_order_relaxed)) {
        break;
      }
      if (result != ZX_ERR_TIMED_OUT) {
        // Might have woken up too early, ensure we have passed the minimal timeout. If the timeout
        // was already passed and we legitimately woke up due to an active ratio event, then this
        // sleep will short-circuit internally and immediately return.
        Thread::Current::Sleep(zx_time_add_duration(last_age_time_.load(ktl::memory_order_relaxed),
                                                    min_mru_rotate_time_));
      }
      // Due to races, there may or may not be an age reason at this point, so go back around the
      // loop and find out, counting how many times we go around.
      iterations_since_last_age++;
      continue;
    }
    AgeReason age_reason = ktl::get<AgeReason>(reason_or_timeout);

    if (iterations_since_last_age == 0) {
      // If we did zero iterations then this means there was an age_reason waiting for us, meaning
      // the min rotation time had already elapsed. This is not an error, but implies that aging
      // thread is running behind.
      pq_aging_reason_before_min_timeout.Add(1);
    } else if (iterations_since_last_age > 1) {
      // Typically a single iteration is expected as we might fail ConsumeAgeReason once due to
      // needing to wait for a timeout. However, due to DeferredPendingSignals, there could be
      // additional spurious wakeups (see comment at the top of the loop). This does not necessarily
      // mean there is an error, but implies that other threads are running badly behind.
      pq_aging_spurious_wakeup.Add(iterations_since_last_age - 1);
    }
    iterations_since_last_age = 0;

    // Taken the aging token, potentially blocking if aging is disabled, make sure to return it when
    // we are done.
    aging_token_.Wait();

    // Make sure the accessed information has been harvested since the last time we aged, otherwise
    // we are deliberately making the age information coarser, by effectively not using one of the
    // queues.
    scanner_wait_for_accessed_scan(last_age_time_);

    RotateReclaimQueues(age_reason);

    // Changing mru_gen_ could have impacted the eviction logic.
    MaybeTriggerLruProcessing();
    aging_token_.Signal();
  }
}

// This thread should, at some point, have some of its logic and signaling merged with the Evictor.
// Currently it might process the lru queue whilst the evictor is already trying to evict, which is
// not harmful but it's a bit wasteful as it doubles the work that happens.
// LRU processing, via ProcessIsolateAndLruQueues, is expensive and happens under the lock_. It is
// expected that ProcessIsolateAndLruQueues perform small units of work to avoid this thread
// causing excessive lock contention.
void PageQueues::LruThread() {
  constexpr uint kLruNeedsProcessingPollSeconds = 90;
  uint64_t pending_target_gen = UINT64_MAX;
  while (!shutdown_threads_.load(ktl::memory_order_relaxed)) {
    zx_status_t wait_status =
        lru_event_.Wait(Deadline::after_mono(ZX_SEC(kLruNeedsProcessingPollSeconds)));

    uint64_t target_gen;
    bool needs_processing = false;
    // Take the lock so we can calculate (race free) a target mru-gen.
    {
      Guard<CriticalMutex> guard{&lock_};
      needs_processing = NeedsLruProcessingLocked();
      // If needs processing is false this will calculate an incorrect target_gen, but that's fine
      // as we'll just discard it and it's simpler to just do it here unconditionally while the lock
      // is already held.
      target_gen = lru_gen_.load(ktl::memory_order_relaxed) + 1;
    }
    if (!needs_processing) {
      pq_lru_spurious_wakeup.Add(1);
      continue;
    }
    if (wait_status == ZX_ERR_TIMED_OUT) {
      // The queue needs processing, but we woke up due to a timeout on the event and not a signal.
      // This could happen due to a race where we woke up before the MruThread could actually set
      // the signal, in which case we want to record the target_gen that we saw and then go back and
      // wait for the signal.
      // In the case where we have timed out *and* the target_gen we want is the same as the last
      // target_gen we were looking for then this means that we have gone a full poll interval with:
      //  * Processing needing to happen.
      //  * No event being signaled.
      //  * No other thread processing the queue for us.
      if (pending_target_gen != target_gen) {
        pending_target_gen = target_gen;
        continue;
      }
      printf("ERROR LruThread signal was not seen after %u seconds and queue needs processing\n",
             kLruNeedsProcessingPollSeconds);
      Dump();
    }

    // Keep processing until we have caught up to what is required. This ensures we are
    // re-synchronized with the mru-thread and will not miss any signals on the lru_event_.
    while (needs_processing) {
      // With the lock dropped process the target. This is not racy as generations are monotonic, so
      // worst case someone else already processed this generation and this call will be a no-op.
      ProcessLruQueue(target_gen, false);

      // Take the lock so we can calculate (race free) a target mru-gen.
      Guard<CriticalMutex> guard{&lock_};
      needs_processing = NeedsLruProcessingLocked();
      // If needs processing is false this will calculate an incorrect target_gen, but that's fine
      // as we'll just discard it and it's simpler to just do it here unconditionally while the lock
      // is already held.
      target_gen = lru_gen_.load(ktl::memory_order_relaxed) + 1;
    }
  }
}

void PageQueues::RotateReclaimQueues(AgeReason reason) {
  VM_KTRACE_DURATION(2, "RotatePagerBackedQueues");
  // We expect LRU processing to have already happened, so first poll the mru semaphore.
  if (mru_semaphore_.Wait(Deadline::infinite_past()) == ZX_ERR_TIMED_OUT) {
    // We should not have needed to wait for lru processing here, as it should have already been
    // made available due to earlier triggers. Although this could reasonably happen due to races or
    // delays in scheduling we record in a counter as happening regularly could indicate a bug.
    pq_aging_blocked_on_lru.Add(1);

    MaybeTriggerLruProcessing();

    // The LRU thread could take an arbitrary amount of time to get scheduled and run, so we cannot
    // enforce a deadline. However, we can assume there might be a bug and start making noise to
    // inform the user if we have waited multiples of the expected maximum aging interval, since
    // that implies we are starting to lose the requested fidelity of age information.
    int64_t timeouts = 0;
    while (mru_semaphore_.Wait(Deadline::after_mono(max_mru_rotate_time_, TimerSlack::none())) ==
           ZX_ERR_TIMED_OUT) {
      timeouts++;
      printf("[pq] WARNING: Waited %" PRIi64 " seconds for LRU thread, MRU semaphore %" PRIi64
             ", aging is presently stalled\n",
             (max_mru_rotate_time_ * timeouts) / ZX_SEC(1), mru_semaphore_.count());
      Dump();
    }
  }

  ASSERT(mru_gen_.load(ktl::memory_order_relaxed) - lru_gen_.load(ktl::memory_order_relaxed) <
         kNumReclaim - 1);

  {
    // Acquire the lock to increment the mru_gen_. This allows other queue logic to not worry about
    // mru_gen_ changing whilst they hold the lock.
    Guard<CriticalMutex> guard{&lock_};
    mru_gen_.fetch_add(1, ktl::memory_order_relaxed);
    last_age_time_ = current_mono_time();
    last_age_reason_ = reason;
    CheckActiveRatioAgingLocked();

    if (aging_event_) {
      aging_event_->Signal();
    }
  }

  // Keep a count of the different reasons we have rotated.
  switch (reason) {
    case AgeReason::Timeout:
      pq_aging_reason_timeout.Add(1);
      break;
    case AgeReason::ActiveRatio:
      pq_aging_reason_active_ratio.Add(1);
      break;
    case AgeReason::Manual:
      pq_aging_reason_manual.Add(1);
      break;
    default:
      panic("Unknown age reason");
  }
}

ktl::optional<PageQueues::VmoBacklink> PageQueues::ProcessIsolateList(ktl::optional<size_t> peek) {
  // Need to move every page out of the list and either put it back in the regular Isolate list,
  // or in its correct queue. If we hit active pages we may need to replace them with loaned.

  // Processing the Isolate queue requires holding the page_queues_ lock_. The only other actions
  // that require this lock are inserting or removing pages from the page queues. To ensure these
  // actions can complete in a small bounded time kMaxDeferredWork is chosen to be very small so
  // that the lock will be regularly dropped. As processing the Isolate queue is not time critical
  // and can be somewhat inefficient in its operation we err on the side of doing less work per lock
  // acquisition.
  constexpr uint64_t kMaxDeferredWork = 16;
  // Pages in this list might be replaced with a loaned page, this must be done outside the lock_,
  // so we accumulate pages and then act after lock_ is released.
  LruIsolate<kMaxDeferredWork> deferred_list;
  // Only accumulate pages to try to replace with loaned pages if loaned pages are available and
  // we're allowed to borrow at this code location.
  const bool do_sweeping = (pmm_count_loaned_free_pages() != 0) &&
                           pmm_physical_page_borrowing_config()->is_borrowing_on_mru_enabled();

  // Calculate a worst case iterations for processing any given isolate list.
  ActiveInactiveCounts active_inactive = GetActiveInactiveCounts();
  const uint64_t max_isolate_iterations =
      active_inactive.active + active_inactive.inactive + kNumReclaim;

  // In order to safely resume iteration where we left off between lock drops we need to make use of
  // the isolate_cursor_, which requires holding the isolate_cursor_lock_.
  Guard<Mutex> isolate_cursor_guard{&isolate_cursor_lock_};

  const size_t max_queue = ktl::min(peek.value_or(kNumIsolateQueues - 1), kNumIsolateQueues - 1);

  for (size_t i = 0; i <= max_queue; i++) {
    deferred_list.Flush();
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    list_node_t* list = &isolate_queues_[i];
    vm_page_t* current = list_peek_head_type(list, vm_page_t, queue_node);
    // Count work done separately to all iterations so we can periodically drop the lock and process
    // the deferred_list.
    uint64_t work_done = 0;
    // Separately count iterations for debug purposes.
    uint64_t loop_iterations = 0;
    while (current) {
      if (loop_iterations++ == max_isolate_iterations) {
        KERNEL_OOPS("[pq]: WARNING: %s exceeded expected max isolate loop iterations %" PRIu64 "",
                    __FUNCTION__, max_isolate_iterations);
      }

      vm_page_t* page = current;
      current = list_next_type(list, &current->queue_node, vm_page_t, queue_node);
      PageQueue page_queue =
          static_cast<PageQueue>(page->object.get_page_queue_ref().load(ktl::memory_order_relaxed));
      // Place in the correct list, preserving age
      if (page_queue == PageQueueReclaimIsolate) {
        if (peek) {
          VmCowPages* cow = reinterpret_cast<VmCowPages*>(page->object.get_object());
          DEBUG_ASSERT(cow);
          // Upgrading to a refptr can never fail as all pages are removed from a VmCowPages, and
          // hence from the page queues here, prior to the last reference to a cow pages being
          // dropped.
          fbl::RefPtr<VmCowPages> cow_pages = fbl::MakeRefPtrUpgradeFromRaw(cow, list_lock_);
          DEBUG_ASSERT(cow_pages);
          return VmoBacklink{
              .cow = ktl::move(cow_pages), .page = page, .offset = page->object.get_page_offset()};
        }
      } else {
        list_delete(&page->queue_node);

        // Only reason for a page to be in the DontNeed list and have the wrong queue is if it was
        // recently accessed. That means it's active and we can attempt to loan to it. As the entire
        // DontNeed queue is processed each time we change the LRU, we know this is a valid page
        // queue that has not yet aged out. We have no way to know the relative age of this page
        // with respect to its target queue, so the head is as good a place as any to put it.
        list_add_head(&page_queues_[page_queue], &page->queue_node);
        if (do_sweeping && !page->is_loaned()) {
          deferred_list.AddLoanReplacement(page, this);
        }
      }
      work_done++;
      if (work_done >= kMaxDeferredWork) {
        // Drop the lock and flush the deferred_list. Saving and restoring current to the
        // isolate_cursor_.
        isolate_cursor_ = {.page = current, .list = list};
        guard.CallUnlocked([&deferred_list]() { deferred_list.Flush(); });
        current = isolate_cursor_.page;
        isolate_cursor_ = {.page = nullptr, .list = nullptr};
        work_done = 0;
      }
    }
  }
  return ktl::nullopt;
}

void PageQueues::ProcessLruQueue(uint64_t target_gen, bool isolate) {
  // This assertion is <=, and not strictly <, since to evict a some queue X, the target must be
  // X+1. Hence to preserve kNumActiveQueues, we can allow target_gen to become equal to the first
  // active queue, as this will process all the non-active queues. Although we might refresh our
  // value for the mru_queue, since the mru_gen_ is monotonic increasing, if this assert passes once
  // it should continue to be true.
  ASSERT(target_gen <= mru_gen_.load(ktl::memory_order_relaxed) - (kNumActiveQueues - 1));

  {
    VM_KTRACE_DURATION(2, "ProcessIsolateList");
    // Process through the Isolate list and move out anything that has been updated.
    ProcessIsolateList(ktl::nullopt);
  }

  // Processing the LRU queue requires holding the page_queues_ lock_. The only other
  // actions that require this lock are inserting or removing pages from the page queues. To ensure
  // these actions can complete in a small bounded time kMaxQueueWork is chosen to be very small so
  // that the lock will be regularly dropped. As processing the Isolate/LRU queue is not time
  // critical and can be somewhat inefficient in its operation we err on the side of doing less work
  // per lock acquisition.
  //
  // Also, we need to limit the number to avoid sweep_to_loaned taking up excessive stack space.
  static constexpr uint32_t kMaxQueueWork = 16;

  // Calculate a truly worst case loop iteration count based on every page being in the LRU
  // queue and needing to iterate the LRU multiple steps to the target_gen. Instead of reading the
  // LRU and comparing the target_gen, just add a buffer of the maximum number of page queues.
  ActiveInactiveCounts active_inactive = GetActiveInactiveCounts();
  const uint64_t max_lru_iterations =
      active_inactive.active + active_inactive.inactive + kNumReclaim;
  // Loop iteration counting is just for diagnostic purposes.
  uint64_t loop_iterations = 0;

  // Pages in this list might be reclaimed or replaced with a loaned page, depending on the action
  // specified in deferred_action. Each of these actions must be done outside the lock_, so we
  // accumulate pages and then act after lock_ is released.
  // The deferred_list is declared here as it is expensive to construct/destruct and we would like
  // to reuse it between iterations.
  LruIsolate<kMaxQueueWork> deferred_list;

  // Only accumulate pages to try to replace with loaned pages if loaned pages are available and
  // we're allowed to borrow at this code location.
  const bool do_sweeping = (pmm_count_loaned_free_pages() != 0) &&
                           pmm_physical_page_borrowing_config()->is_borrowing_on_mru_enabled();

  VM_KTRACE_DURATION(2, "ProcessLruQueue");
  while (true) {
    if (loop_iterations++ == max_lru_iterations) {
      KERNEL_OOPS("[pq]: WARNING: %s exceeded expected max LRU loop iterations %" PRIu64 "",
                  __FUNCTION__, max_lru_iterations);
    }

    deferred_list.Flush();
    bool post = false;
    {
      // Need to hold the general lock, in addition to the list lock, so that the lru_gen_ does not
      // change while we are working.
      Guard<CriticalMutex> guard{&lock_};
      // Fill in the lru action now that the lock is held.
      deferred_list.SetLruAction(lru_action_);
      Guard<SpinLock, IrqSave> list_guard{&list_lock_};
      const uint64_t lru = lru_gen_.load(ktl::memory_order_relaxed);
      if (lru >= target_gen) {
        break;
      }
      const PageQueue mru_queue = mru_gen_to_queue();
      const PageQueue lru_queue = gen_to_queue(lru);
      list_node_t* list = &page_queues_[lru_queue];

      for (uint iterations = 0; !list_is_empty(list) && iterations < kMaxQueueWork; iterations++) {
        vm_page_t* page = list_remove_head_type(list, vm_page_t, queue_node);
        // As we are processing the LRU queues we should never see the page of the isolate cursor,
        // as that is from a different list, and hence we do not need to call AdvanceIsolateCursorIf
        DEBUG_ASSERT(page != isolate_cursor_.page);
        PageQueue page_queue = static_cast<PageQueue>(
            page->object.get_page_queue_ref().load(ktl::memory_order_relaxed));
        DEBUG_ASSERT(page_queue >= PageQueueReclaimBase);

        // If the queue stored in the page does not match then we want to move it to its correct
        // queue with the caveat that its queue could be invalid. The queue would be invalid if
        // MarkAccessed had raced. Should this happen we know that the page is actually *very* old,
        // and so we will fall back to the case of forcibly changing its age to the new lru gen.
        if (page_queue != lru_queue && queue_is_valid(page_queue, lru_queue, mru_queue)) {
          list_add_head(&page_queues_[page_queue], &page->queue_node);

          if (do_sweeping && !page->is_loaned() && queue_is_active(page_queue, mru_queue)) {
            deferred_list.AddLoanReplacement(page, this);
          }
        } else {
          // Force it into either our target queue or the isolate list, don't care about races. If
          // we happened to access it at the same time then too bad.
          PageQueue new_queue = isolate ? PageQueueReclaimIsolate : gen_to_queue(target_gen);
          list_node_t* target_queue = isolate ? &isolate_queues_[0] : &page_queues_[new_queue];
          PageQueue old_queue =
              static_cast<PageQueue>(page->object.get_page_queue_ref().exchange(new_queue));
          DEBUG_ASSERT(old_queue >= PageQueueReclaimBase);

          page_queue_counts_[old_queue].fetch_sub(1, ktl::memory_order_relaxed);
          page_queue_counts_[new_queue].fetch_add(1, ktl::memory_order_relaxed);
          list_add_tail(target_queue, &page->queue_node);
          // We should only have performed this step to move from one inactive bucket to the next,
          // so there should be no active/inactive count changes needed.
          DEBUG_ASSERT(!queue_is_active(new_queue, mru_queue));
          deferred_list.AddReclaimable(page, this);
        }
      }
      if (list_is_empty(list)) {
        // Note that we held the lock the entire time, and lru_gen_ is always modified with the lock
        // held, so this should always precisely set lru_gen_ to lru + 1.
        [[maybe_unused]] uint64_t prev = lru_gen_.fetch_add(1, ktl::memory_order_relaxed);
        DEBUG_ASSERT(prev == lru);
        post = true;
      }
    }
    if (post) {
      mru_semaphore_.Post();
    }
  }
}

void PageQueues::MarkAccessed(vm_page_t* page) {
  pq_accessed_normal.Add(1);
  auto queue_ref = page->object.get_page_queue_ref();
  uint8_t old_gen = queue_ref.load(ktl::memory_order_relaxed);
  if (!queue_is_reclaim(static_cast<PageQueue>(old_gen))) {
    return;
  }
  const uint32_t target_queue = mru_gen_to_queue();
  if (old_gen == target_queue) {
    pq_accessed_normal_same_queue.Add(1);
    return;
  }
  // Between loading the mru_gen and finally storing it in the queue_ref it's possible for our
  // calculated target_queue to become invalid. This is extremely unlikely as it would require
  // us to stall for long enough for the lru_gen to pass this point, but if it does happen then
  // ProcessLruQueues will notice our queue is invalid and correct our age to be that of lru_gen.
  while (!queue_ref.compare_exchange_weak(old_gen, static_cast<uint8_t>(target_queue),
                                          ktl::memory_order_relaxed)) {
    // If we ever find old_gen to not be in the active/inactive range then this means the page has
    // either been racily removed from, or was never in, the reclaim queue. In which case we
    // can return as there's nothing to be marked accessed.
    if (!queue_is_reclaim(static_cast<PageQueue>(old_gen))) {
      return;
    }
  }
  page_queue_counts_[old_gen].fetch_sub(1, ktl::memory_order_relaxed);
  page_queue_counts_[target_queue].fetch_add(1, ktl::memory_order_relaxed);

  MaybeCheckActiveRatioAging(1);
}

void PageQueues::MaybeCheckActiveRatioAging(size_t pages) {
  if (unlikely(!RecordActiveRatioSkips(pages))) {
    Guard<CriticalMutex> guard{&lock_};
    CheckActiveRatioAgingLocked();
  }
}

void PageQueues::MaybeCheckActiveRatioAgingLocked(size_t pages) {
  if (unlikely(!RecordActiveRatioSkips(pages))) {
    CheckActiveRatioAgingLocked();
  }
}

void PageQueues::SetQueueBacklinkLockedList(vm_page_t* page, void* object, uintptr_t page_offset,
                                            PageQueue queue) {
  DEBUG_ASSERT(page->state() == vm_page_state::OBJECT);
  DEBUG_ASSERT(!page->is_free());
  DEBUG_ASSERT(!list_in_list(&page->queue_node));
  DEBUG_ASSERT(object);
  DEBUG_ASSERT(!page->object.get_object());
  DEBUG_ASSERT(page->object.get_page_offset() == 0);

  page->object.set_object(object);
  page->object.set_page_offset(page_offset);

  DEBUG_ASSERT(page->object.get_page_queue_ref().load(ktl::memory_order_relaxed) == PageQueueNone);
  page->object.get_page_queue_ref().store(queue, ktl::memory_order_relaxed);
  list_add_head(&page_queues_[queue], &page->queue_node);
  page_queue_counts_[queue].fetch_add(1, ktl::memory_order_relaxed);
}

void PageQueues::MoveToQueueLockedList(vm_page_t* page, PageQueue queue) {
  DEBUG_ASSERT(page->state() == vm_page_state::OBJECT);
  DEBUG_ASSERT(!page->is_free());
  DEBUG_ASSERT(list_in_list(&page->queue_node));
  DEBUG_ASSERT(page->object.get_object());
  uint32_t old_queue = page->object.get_page_queue_ref().exchange(queue, ktl::memory_order_relaxed);
  DEBUG_ASSERT(old_queue != PageQueueNone);

  AdvanceIsolateCursorIf(page);
  list_delete(&page->queue_node);
  if (queue == PageQueueReclaimIsolate) {
    list_add_tail(&isolate_queues_[0], &page->queue_node);
  } else {
    list_add_head(&page_queues_[queue], &page->queue_node);
  }
  page_queue_counts_[old_queue].fetch_sub(1, ktl::memory_order_relaxed);
  page_queue_counts_[queue].fetch_add(1, ktl::memory_order_relaxed);
}

void PageQueues::SetWired(vm_page_t* page, VmCowPages* object, uint64_t page_offset) {
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  SetQueueBacklinkLockedList(page, object, page_offset, PageQueueWired);
}

void PageQueues::MoveToWired(vm_page_t* page) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    MoveToQueueLockedList(page, PageQueueWired);
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::SetAnonymous(vm_page_t* page, VmCowPages* object, uint64_t page_offset,
                              bool skip_reclaim) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    SetQueueBacklinkLockedList(
        page, object, page_offset,
        anonymous_is_reclaimable_ && !skip_reclaim ? mru_gen_to_queue() : PageQueueAnonymous);
#if DEBUG_ASSERT_IMPLEMENTED
    if (debug_compressor_) {
      debug_compressor_->Add(page, object, page_offset);
    }
#endif
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::SetHighPriority(vm_page_t* page, VmCowPages* object, uint64_t page_offset) {
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  SetQueueBacklinkLockedList(page, object, page_offset, PageQueueHighPriority);
}

void PageQueues::MoveToHighPriority(vm_page_t* page) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    MoveToQueueLockedList(page, PageQueueHighPriority);
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::MoveToAnonymous(vm_page_t* page, bool skip_reclaim) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    MoveToQueueLockedList(
        page, anonymous_is_reclaimable_ && !skip_reclaim ? mru_gen_to_queue() : PageQueueAnonymous);
#if DEBUG_ASSERT_IMPLEMENTED
    if (debug_compressor_) {
      debug_compressor_->Add(page, reinterpret_cast<VmCowPages*>(page->object.get_object()),
                             page->object.get_page_offset());
    }
#endif
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::SetReclaim(vm_page_t* page, VmCowPages* object, uint64_t page_offset) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    SetQueueBacklinkLockedList(page, object, page_offset, mru_gen_to_queue());
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::MoveToReclaim(vm_page_t* page) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    MoveToQueueLockedList(page, mru_gen_to_queue());
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::MoveToReclaimDontNeed(vm_page_t* page) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    MoveToQueueLockedList(page, PageQueueReclaimIsolate);
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::SetPagerBackedDirty(vm_page_t* page, VmCowPages* object, uint64_t page_offset) {
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  SetQueueBacklinkLockedList(page, object, page_offset, PageQueuePagerBackedDirty);
}

void PageQueues::MoveToPagerBackedDirty(vm_page_t* page) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    MoveToQueueLockedList(page, PageQueuePagerBackedDirty);
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::SetAnonymousZeroFork(vm_page_t* page, VmCowPages* object, uint64_t page_offset) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    SetQueueBacklinkLockedList(
        page, object, page_offset,
        zero_fork_is_reclaimable_ ? mru_gen_to_queue() : PageQueueAnonymousZeroFork);
#if DEBUG_ASSERT_IMPLEMENTED
    if (debug_compressor_) {
      debug_compressor_->Add(page, object, page_offset);
    }
#endif
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::MoveToAnonymousZeroFork(vm_page_t* page) {
  // The common case is that the |page| being moved was previously placed into the anonymous queue.
  // If the zero fork queue is reclaimable, then most likely so is the anonymous queue, and so this
  // move would be a no-op. As this case is common it is worth doing this quick check to
  // short-circuit.
  if (zero_fork_is_reclaimable_ &&
      queue_is_reclaim(static_cast<PageQueue>(
          page->object.get_page_queue_ref().load(ktl::memory_order_relaxed)))) {
    return;
  }
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    MoveToQueueLockedList(
        page, zero_fork_is_reclaimable_ ? mru_gen_to_queue() : PageQueueAnonymousZeroFork);
#if DEBUG_ASSERT_IMPLEMENTED
    if (debug_compressor_) {
      debug_compressor_->Add(page, reinterpret_cast<VmCowPages*>(page->object.get_object()),
                             page->object.get_page_offset());
    }
#endif
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::CompressFailed(vm_page_t* page) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    // Move the page if its currently in some kind of reclaimable queue.
    if (queue_is_reclaim(static_cast<PageQueue>(
            page->object.get_page_queue_ref().load(ktl::memory_order_relaxed)))) {
      MoveToQueueLockedList(page, PageQueueFailedReclaim);
    }
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::ChangeObjectOffset(vm_page_t* page, VmCowPages* object, uint64_t page_offset) {
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  ChangeObjectOffsetLockedList(page, object, page_offset);
}

void PageQueues::ChangeObjectOffsetArray(vm_page_t** pages, VmCowPages* object, uint64_t* offsets,
                                         size_t count) {
  DEBUG_ASSERT(pages);
  DEBUG_ASSERT(offsets);
  DEBUG_ASSERT(object);

  for (size_t i = 0; i < count;) {
    // Don't process more than kMaxBatchSize pages while holding the lock.
    // Instead, drop out of the lock and let other operations proceed before
    // picking the lock up again and resuming.
    size_t end = i + ktl::min(count - i, kMaxBatchSize);
    {
      Guard<SpinLock, IrqSave> guard{&list_lock_};
      for (; i < end; i++) {
        DEBUG_ASSERT(pages[i]);
        ChangeObjectOffsetLockedList(pages[i], object, offsets[i]);
      }
    }

    // If we are not done yet, relax the CPU a bit just to let someone else have
    // a chance at grabbing the spinlock.
    //
    // TODO(johngro): Once our spinlocks have been updated to be more fair
    // (ticket locks, MCS locks, whatever), come back here and remove this
    // pessimistic cpu relax.
    if (i < count) {
      arch::Yield();
    }
  }
}

void PageQueues::ChangeObjectOffsetLockedList(vm_page_t* page, VmCowPages* object,
                                              uint64_t page_offset) {
  DEBUG_ASSERT(page->state() == vm_page_state::OBJECT);
  DEBUG_ASSERT(!page->is_free());
  DEBUG_ASSERT(list_in_list(&page->queue_node));
  DEBUG_ASSERT(object);
  DEBUG_ASSERT(page->object.get_object());
  page->object.set_object(object);
  page->object.set_page_offset(page_offset);
}

void PageQueues::RemoveLockedList(vm_page_t* page) {
  // Directly exchange the old gen.
  uint32_t old_queue =
      page->object.get_page_queue_ref().exchange(PageQueueNone, ktl::memory_order_relaxed);
  DEBUG_ASSERT(old_queue != PageQueueNone);
  page_queue_counts_[old_queue].fetch_sub(1, ktl::memory_order_relaxed);
  page->object.set_object(nullptr);
  page->object.set_page_offset(0);
  AdvanceIsolateCursorIf(page);
  list_delete(&page->queue_node);
}

void PageQueues::Remove(vm_page_t* page) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    RemoveLockedList(page);
  }
  MaybeCheckActiveRatioAging(1);
}

void PageQueues::RemoveArrayIntoList(vm_page_t** pages, size_t count, list_node_t* out_list) {
  DEBUG_ASSERT(pages);

  for (size_t i = 0; i < count;) {
    // Don't process more than kMaxBatchSize pages while holding the lock.
    // Instead, drop out of the lock and let other operations proceed before
    // picking the lock up again and resuming.
    size_t end = i + ktl::min(count - i, kMaxBatchSize);
    {
      Guard<SpinLock, IrqSave> guard{&list_lock_};
      for (; i < end; i++) {
        DEBUG_ASSERT(pages[i]);
        RemoveLockedList(pages[i]);
        list_add_tail(out_list, &pages[i]->queue_node);
      }
    }

    // If we are not done yet, relax the CPU a bit just to let someone else have
    // a chance at grabbing the spinlock.
    //
    // TODO(johngro): Once our spinlocks have been updated to be more fair
    // (ticket locks, MCS locks, whatever), come back here and remove this
    // pessimistic cpu relax.
    if (i < count) {
      arch::Yield();
    }
  }
  MaybeCheckActiveRatioAging(count);
}

PageQueues::ReclaimCounts PageQueues::GetReclaimQueueCounts() const {
  ReclaimCounts counts;

  // Grab the lock to prevent LRU processing, this lets us get a slightly less racy snapshot of
  // the queue counts, although we may still double count pages that move after we count them.
  // Specifically any parallel callers of MarkAccessed could move a page and change the counts,
  // causing us to either double count or miss count that page. As these counts are not load
  // bearing we accept the very small chance of potentially being off a few pages.
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  uint64_t lru = lru_gen_.load(ktl::memory_order_relaxed);
  uint64_t mru = mru_gen_.load(ktl::memory_order_relaxed);

  counts.total = 0;
  for (uint64_t index = lru; index <= mru; index++) {
    uint64_t count = page_queue_counts_[gen_to_queue(index)].load(ktl::memory_order_relaxed);
    // Distance to the MRU, and not the LRU, determines the bucket the count goes into. This is to
    // match the logic in PeekPagerBacked, which is also based on distance to MRU.
    if (index > mru - kNumActiveQueues) {
      counts.newest += count;
    } else if (index <= mru - (kNumReclaim - kNumOldestQueues)) {
      counts.oldest += count;
    }
    counts.total += count;
  }
  // Account the Isolate queue length under |oldest|, since (Isolate + oldest LRU) pages are
  // eligible for reclamation first. |oldest| is meant to track pages eligible for eviction first.
  uint64_t inactive_count =
      page_queue_counts_[PageQueueReclaimIsolate].load(ktl::memory_order_relaxed);
  counts.oldest += inactive_count;
  counts.total += inactive_count;
  return counts;
}

PageQueues::Counts PageQueues::QueueCounts() const {
  Counts counts = {};

  // Grab the lock to prevent LRU processing, this lets us get a slightly less racy snapshot of
  // the queue counts. We may still double count pages that move after we count them.
  Guard<SpinLock, IrqSave> guard{&list_lock_};
  uint64_t lru = lru_gen_.load(ktl::memory_order_relaxed);
  uint64_t mru = mru_gen_.load(ktl::memory_order_relaxed);

  for (uint64_t index = lru; index <= mru; index++) {
    counts.reclaim[mru - index] =
        page_queue_counts_[gen_to_queue(index)].load(ktl::memory_order_relaxed);
  }
  counts.reclaim_isolate =
      page_queue_counts_[PageQueueReclaimIsolate].load(ktl::memory_order_relaxed);
  counts.pager_backed_dirty =
      page_queue_counts_[PageQueuePagerBackedDirty].load(ktl::memory_order_relaxed);
  counts.anonymous = page_queue_counts_[PageQueueAnonymous].load(ktl::memory_order_relaxed);
  counts.wired = page_queue_counts_[PageQueueWired].load(ktl::memory_order_relaxed);
  counts.anonymous_zero_fork =
      page_queue_counts_[PageQueueAnonymousZeroFork].load(ktl::memory_order_relaxed);
  counts.failed_reclaim =
      page_queue_counts_[PageQueueFailedReclaim].load(ktl::memory_order_relaxed);
  counts.high_priority = page_queue_counts_[PageQueueHighPriority].load(ktl::memory_order_relaxed);
  return counts;
}

template <typename F>
bool PageQueues::DebugPageIsSpecificReclaim(const vm_page_t* page, F validator,
                                            size_t* queue) const {
  fbl::RefPtr<VmCowPages> cow_pages;
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    PageQueue q = (PageQueue)page->object.get_page_queue_ref().load(ktl::memory_order_relaxed);
    if (q < PageQueueReclaimBase || q > PageQueueReclaimLast) {
      return false;
    }
    if (queue) {
      *queue = queue_age(q, mru_gen_to_queue());
    }
    VmCowPages* cow = reinterpret_cast<VmCowPages*>(page->object.get_object());
    DEBUG_ASSERT(cow);
    cow_pages = fbl::MakeRefPtrUpgradeFromRaw(cow, guard);
    DEBUG_ASSERT(cow_pages);
  }
  return validator(cow_pages);
}

template <typename F>
bool PageQueues::DebugPageIsSpecificQueue(const vm_page_t* page, PageQueue queue,
                                          F validator) const {
  fbl::RefPtr<VmCowPages> cow_pages;
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    PageQueue q = (PageQueue)page->object.get_page_queue_ref().load(ktl::memory_order_relaxed);
    if (q != queue) {
      return false;
    }
    VmCowPages* cow = reinterpret_cast<VmCowPages*>(page->object.get_object());
    DEBUG_ASSERT(cow);
    cow_pages = fbl::MakeRefPtrUpgradeFromRaw(cow, guard);
    DEBUG_ASSERT(cow_pages);
  }
  return validator(cow_pages);
}

bool PageQueues::DebugPageIsReclaim(const vm_page_t* page, size_t* queue) const {
  return DebugPageIsSpecificReclaim(page, [](auto cow) { return true; }, queue);
}

bool PageQueues::DebugPageIsReclaimIsolate(const vm_page_t* page) const {
  return DebugPageIsSpecificQueue(page, PageQueueReclaimIsolate,
                                  [](auto cow) { return cow->can_evict(); });
}

bool PageQueues::DebugPageIsPagerBackedDirty(const vm_page_t* page) const {
  return page->object.get_page_queue_ref().load(ktl::memory_order_relaxed) ==
         PageQueuePagerBackedDirty;
}

bool PageQueues::DebugPageIsAnonymous(const vm_page_t* page) const {
  if (ReclaimIsOnlyPagerBacked()) {
    return page->object.get_page_queue_ref().load(ktl::memory_order_relaxed) == PageQueueAnonymous;
  }
  return DebugPageIsSpecificReclaim(page, [](auto cow) { return !cow->can_evict(); }, nullptr);
}

bool PageQueues::DebugPageIsWired(const vm_page_t* page) const {
  return page->object.get_page_queue_ref().load(ktl::memory_order_relaxed) == PageQueueWired;
}

bool PageQueues::DebugPageIsHighPriority(const vm_page_t* page) const {
  return page->object.get_page_queue_ref().load(ktl::memory_order_relaxed) == PageQueueHighPriority;
}

bool PageQueues::DebugPageIsAnonymousZeroFork(const vm_page_t* page) const {
  if (ReclaimIsOnlyPagerBacked()) {
    return page->object.get_page_queue_ref().load(ktl::memory_order_relaxed) ==
           PageQueueAnonymousZeroFork;
  }
  return DebugPageIsSpecificReclaim(page, [](auto cow) { return !cow->can_evict(); }, nullptr);
}

bool PageQueues::DebugPageIsAnyAnonymous(const vm_page_t* page) const {
  return DebugPageIsAnonymous(page) || DebugPageIsAnonymousZeroFork(page);
}

ktl::optional<PageQueues::VmoBacklink> PageQueues::PopAnonymousZeroFork() {
  ktl::optional<PageQueues::VmoBacklink> ret;
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};

    vm_page_t* page =
        list_peek_tail_type(&page_queues_[PageQueueAnonymousZeroFork], vm_page_t, queue_node);
    if (!page) {
      return ktl::nullopt;
    }

    VmCowPages* cow = reinterpret_cast<VmCowPages*>(page->object.get_object());
    uint64_t page_offset = page->object.get_page_offset();
    DEBUG_ASSERT(cow);
    MoveToQueueLockedList(page, PageQueueAnonymous);
    ret = VmoBacklink{fbl::MakeRefPtrUpgradeFromRaw(cow, guard), page, page_offset};
  }
  MaybeCheckActiveRatioAging(1);
  return ret;
}

ktl::optional<PageQueues::VmoBacklink> PageQueues::PeekIsolate(size_t lowest_queue) {
  // Ignore any requests to evict from the active queues as this is never allowed.
  lowest_queue = ktl::max(lowest_queue, kNumActiveQueues);

  // TODO(adanis): Restructure this loop such that there is no question about its termination, but
  // for now be paranoid.
  constexpr uint kMaxIterations = kNumReclaim * 2;
  uint loop_iterations = 0;

  while (true) {
    // Peek the Isolate queue in case anything is ready for us.
    ktl::optional<VmoBacklink> result = ProcessIsolateList(0);
    if (result) {
      return result;
    }
    if (loop_iterations++ > kMaxIterations) {
      KERNEL_OOPS("[pq]: %s iterated more than %u times", __FUNCTION__, kMaxIterations);
    }

    SynchronizeWithAging();
    // The limit gen is 1 larger than the lowest queue because evicting from queue X is done by
    // attempting to make the lru queue be X+1.
    const uint64_t lru_limit = mru_gen_.load(ktl::memory_order_relaxed) - (lowest_queue - 1);
    // Attempt to process one generation at a time to limit the work done before we find a
    // reclaimable page.
    const uint64_t lru_target = lru_gen_.load(ktl::memory_order_relaxed) + 1;
    if (lru_target > lru_limit) {
      return ProcessIsolateList(kNumIsolateQueues - 1);
    }
    ProcessLruQueue(lru_target, true);
  }
}

PageQueues::ActiveInactiveCounts PageQueues::GetActiveInactiveCounts() const {
  uint64_t active_count = 0;
  uint64_t inactive_count = 0;
  PageQueue mru = mru_gen_to_queue();
  for (uint8_t queue = 0; queue < PageQueueNumQueues; queue++) {
    uint64_t count = page_queue_counts_[queue].load(ktl::memory_order_relaxed);
    if (queue_is_active(static_cast<PageQueue>(queue), mru)) {
      active_count += count;
    }
    if (queue_is_inactive(static_cast<PageQueue>(queue), mru)) {
      inactive_count += count;
    }
  }
  return ActiveInactiveCounts{.active = active_count, .inactive = inactive_count};
}

void PageQueues::SetAgingEvent(Event* event) {
  Guard<CriticalMutex> guard{&lock_};
  ASSERT(!event || !aging_event_);
  aging_event_ = event;
}

void PageQueues::EnableAnonymousReclaim(bool zero_forks) {
  {
    Guard<SpinLock, IrqSave> guard{&list_lock_};
    anonymous_is_reclaimable_ = true;
    zero_fork_is_reclaimable_ = zero_forks;

    const PageQueue mru_queue = mru_gen_to_queue();

    // Migrate any existing pages into the reclaimable queues.

    while (!list_is_empty(&page_queues_[PageQueueAnonymous])) {
      vm_page_t* page =
          list_peek_head_type(&page_queues_[PageQueueAnonymous], vm_page_t, queue_node);
      MoveToQueueLockedList(page, mru_queue);
    }
    while (zero_forks && !list_is_empty(&page_queues_[PageQueueAnonymousZeroFork])) {
      vm_page_t* page =
          list_peek_head_type(&page_queues_[PageQueueAnonymousZeroFork], vm_page_t, queue_node);
      MoveToQueueLockedList(page, mru_queue);
    }
  }
  Guard<CriticalMutex> guard{&lock_};
  CheckActiveRatioAgingLocked();
}

ktl::optional<PageQueues::VmoBacklink> PageQueues::GetCowForLoanedPage(vm_page_t* page) {
  DEBUG_ASSERT(page->is_loaned());
  vm_page_state state = page->state();
  switch (state) {
    case vm_page_state::FREE_LOANED:
      // Page is not owned by the page queues, so no cow pages to lookup.
      return ktl::nullopt;
    case vm_page_state::OBJECT: {
      // Delaying the lock acquisition and then reading the object field here is safe since the
      // caller has guaranteed that the page state is not changing, and then the  object field is
      // only modified under lock_, which we will be holding.
      Guard<SpinLock, IrqSave> guard{&list_lock_};
      VmCowPages* cow = reinterpret_cast<VmCowPages*>(page->object.get_object());
      if (!cow) {
        // Our examination of the state was racy and this page may or may not be owned by a VMO, but
        // it's not in the page queues. It is the responsibility of the caller to deal with scenario
        // and the fact that once we drop our lock the page could get inserted into the page queues.
        return ktl::nullopt;
      }
      // There is a using/borrowing cow and we know it is still alive as we hold the
      // PageQueues lock, and the cow may not destruct while it still has pages.
      uint64_t page_offset = page->object.get_page_offset();
      VmoBacklink backlink{fbl::MakeRefPtrUpgradeFromRaw(cow, guard), page, page_offset};
      DEBUG_ASSERT(backlink.cow);
      return backlink;
    }
    case vm_page_state::ALLOC:
      // Page is moving between the PMM and a VMO in some direction, but is not in the page
      // queues.
      return ktl::nullopt;
    default:
      // A loaned page in any other state is invalid and represents a programming error or bug.
      panic("Unexpected page state %s for loaned page", page_state_to_string(state));
      return ktl::nullopt;
  }
}
