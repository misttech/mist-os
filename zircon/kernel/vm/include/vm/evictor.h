// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_EVICTOR_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_EVICTOR_H_

#include <lib/zircon-internal/thread_annotations.h>
#include <sys/types.h>
#include <zircon/time.h>

#include <kernel/event.h>
#include <kernel/spinlock.h>

class PmmNode;
class PageQueues;
class VmCompression;

namespace vm_unittest {
class TestPmmNode;
}

// Implements page evictor logic to free pages belonging to a PmmNode under memory pressure.
// Eviction in this context is both direct eviction of pager backed memory and discardable VMO
// memory, as well as by performing compression.
// This class is thread-safe.
class Evictor {
 public:
  enum class EvictionLevel : uint8_t {
    OnlyOldest = 0,
    IncludeNewest = 1,
  };

  enum class Output : bool {
    Print = true,
    NoPrint = false,
  };

  enum class TriggerReason : bool {
    OOM = true,
    Other = false,
  };

  // Eviction target state is grouped together behind a lock to allow different threads to safely
  // trigger and perform the eviction.
  struct EvictionTarget {
    bool pending = false;
    // The desired value to get |pmm_node_|'s free page count to
    uint64_t free_pages_target = 0;
    // A minimum amount of pages we want to evict, regardless of how much free memory is available.
    uint64_t min_pages_to_free = 0;
    EvictionLevel level = EvictionLevel::OnlyOldest;
    bool print_counts = false;
    bool oom_trigger = false;
  };

  // We count non-loaned and loaned evicted pages separately since the eviction goal is set in terms
  // of non-loaned pages (for now) so in order to verify expected behavior in tests we keep separate
  // counts.
  struct EvictedPageCounts {
    // The pager_backed and pager_backed_loaned counts are exclusive; any given evicted page is
    // counted as pager_backed or pager_backed_loaned depending on whether it's non-loaned or loaned
    // respectively.
    //
    // evicted from pager-backed VMO non-loaned page count
    uint64_t pager_backed = 0;
    // evicted from pager-backed VMO loaned page count
    uint64_t pager_backed_loaned = 0;
    // evicted from/via discardable VMO page count
    uint64_t discardable = 0;
    // evicted from an anonymous VMO via compression
    uint64_t compressed = 0;

    EvictedPageCounts &operator+=(const EvictedPageCounts &counts) {
      pager_backed += counts.pager_backed;
      pager_backed_loaned += counts.pager_backed_loaned;
      discardable += counts.discardable;
      compressed += counts.compressed;
      return *this;
    }
  };

  Evictor();
  ~Evictor();

  // Called from the scanner to enable eviction if required. Creates an eviction thread to process
  // asynchronous eviction requests.
  // By default this only enables user pager based eviction and |use_compression| can be used to
  // also perform compression.
  void EnableEviction(bool use_compression);
  // Called from the scanner to disable all eviction if needed, will shut down any in existing
  // eviction thread. It is a responsibility of the scanner to not have multiple concurrent calls
  // to this and EnableEviction.
  void DisableEviction();

  // Evict from a user specified external |target| which is only used for this eviction attempt and
  // does not interfere with the |eviction_target_|.
  EvictedPageCounts EvictFromExternalTarget(EvictionTarget target);

  // Performs a synchronous request to evict |min_mem_to_free| (in bytes). The return value is the
  // number of pages evicted. The |eviction_level| is a rough control that maps to how old a page
  // needs to be for being considered for eviction. This may acquire arbitrary vmo and aspace locks.
  uint64_t EvictSynchronous(uint64_t min_mem_to_free,
                            EvictionLevel eviction_level = EvictionLevel::OnlyOldest,
                            Output output = Output::NoPrint,
                            TriggerReason reason = TriggerReason::Other);

  // Reclaim memory until free memory equals the |free_mem_target| (in bytes) and at least
  // |min_mem_to_free| (in bytes) has been reclaimed. Reclamation will happen asynchronously on the
  // eviction thread and this function returns immediately. Once the target is reached, or there is
  // no more memory that can be reclaimed, this process will stop and the free memory target will be
  // cleared. The |eviction_level| is a rough control on how hard to try and evict. Multiple calls
  // to EvictAsynchronous will cause all the targets to get merged by adding together
  // |min_mem_to_free|, taking the max of |free_mem_target| and the highest or most aggressive of
  // any |eviction_level|.
  void EvictAsynchronous(uint64_t min_mem_to_free, uint64_t free_mem_target,
                         EvictionLevel eviction_level = EvictionLevel::OnlyOldest,
                         Output output = Output::NoPrint);

  // Whether any eviction can occur.
  bool IsEvictionEnabled() const;

  // Whether eviction should attempt to use compression.
  bool IsCompressionEnabled() const;

  struct EvictorStats {
    uint64_t pager_backed_oom = 0;
    uint64_t pager_backed_other = 0;
    uint64_t compression_oom = 0;
    uint64_t compression_other = 0;
    uint64_t discarded_oom = 0;
    uint64_t discarded_other = 0;
  };
  // Return global eviction stats from all instantiations of the Evictor.
  static EvictorStats GetGlobalStats();

 private:
  // Private constructor for test code to specify custom methods to fake the reclamation.
  using ReclaimFunction = fit::inline_function<ktl::optional<EvictedPageCounts>(
      VmCompression *compression, EvictionLevel eviction_level)>;
  using FreePagesFunction = fit::inline_function<uint64_t()>;

  Evictor(ReclaimFunction reclaim_function, FreePagesFunction free_pages_function);

  // Helpers for testing.
  EvictionTarget DebugGetEvictionTarget() const;

  friend class vm_unittest::TestPmmNode;

  // Combine the specified |target| with the pre-existing |eviction_target_|.
  void CombineEvictionTarget(EvictionTarget target);

  // Perform eviction based on the current values of |eviction_target_|. The expectation is that the
  // user will have set the target before calling this function with either SetEvictionTarget() or
  // CombineEvictionTarget(). This may acquire arbitrary vmo and aspace locks.
  EvictedPageCounts EvictFromPreloadedTarget();

  // Helper for EvictFromPreloadedTarget and EvictFromExternalTarget.
  EvictedPageCounts EvictFromTargetInternal(EvictionTarget target);

  // Evict until |min_pages_to_evict| have been evicted and there are at least |free_pages_target|
  // free pages on the system. Note that the eviction operation here is one-shot, i.e. as soon as
  // the targets are met, eviction will stop and the function will return. Returns the number of
  // discardable and pager-backed pages evicted and pages compressed. This may acquire arbitrary vmo
  // and aspace locks.
  EvictedPageCounts EvictUntilTargetsMet(uint64_t min_pages_to_evict, uint64_t free_pages_target,
                                         EvictionLevel level) TA_EXCL(lock_);

  // Evict the requested number of |target_pages| from vmos by querying the page queues. The
  // returned struct has the number of pages evicted. The |eviction_level|
  // is a rough control that maps to how old a page needs to be for being considered for eviction.
  // This may acquire arbitrary vmo and aspace locks.
  EvictedPageCounts EvictPageQueues(uint64_t target_pages, EvictionLevel eviction_level) const
      TA_EXCL(lock_);

  // The main loop for the eviction thread.
  int EvictionThreadLoop() TA_EXCL(lock_);

  // Returns the count of the free pages for use in performing target calculations. This could be
  // from the PMM or a test fake.
  uint64_t CountFreePages() const;

  // Helper method that performs a single 'step' of reclamation via the page queues. A return value
  // of some (even if the counts themselves are all zero) indicates that there is more to
  // reclamation that can be attempted, where as a nullopt indicates there is nothing left to try
  // and reclaim.
  // This method will either reclaim from the global page queues, or receive fake data from a test
  // instance.
  ktl::optional<EvictedPageCounts> EvictPageQueuesHelper(VmCompression *compression,
                                                         EvictionLevel eviction_level) const;

  // Target for eviction.
  EvictionTarget eviction_target_ TA_GUARDED(lock_) = {};

  // Event that enforces only one eviction attempt to be active at any time. This prevents us from
  // overshooting the free memory targets required by various simultaneous eviction requests.
  AutounsignalEvent no_ongoing_eviction_{true};

  // Use MonitoredSpinLock to provide lockup detector diagnostics for the critical sections
  // protected by this lock.
  mutable DECLARE_SPINLOCK_WITH_TYPE(Evictor, MonitoredSpinLock) lock_;

  // The eviction thread used to process asynchronous requests.
  // Created only if eviction is enabled i.e. |eviction_enabled_| is set to true.
  Thread *eviction_thread_ = nullptr;
  ktl::atomic<bool> eviction_thread_exiting_ = false;

  // Used by the eviction thread to wait for eviction requests.
  AutounsignalEvent eviction_signal_;

  // Optionally specified methods to allow for tests to fake interactions with the pmm. To avoid
  // virtual dispatch in non test scenarios these are empty/null methods when not set.
  const ReclaimFunction test_reclaim_function_;
  const FreePagesFunction test_free_pages_function_;

  // These parameters are initialized later from kernel cmdline options.
  // Whether eviction is enabled.
  bool eviction_enabled_ TA_GUARDED(lock_) = false;
  // Whether eviction should attempt compression.
  bool use_compression_ TA_GUARDED(lock_) = false;
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_EVICTOR_H_
