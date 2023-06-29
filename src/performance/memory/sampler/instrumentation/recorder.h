// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_MEMORY_SAMPLER_INSTRUMENTATION_RECORDER_H_
#define SRC_PERFORMANCE_MEMORY_SAMPLER_INSTRUMENTATION_RECORDER_H_

#include <fidl/fuchsia.memory.sampler/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <unordered_set>

#include <fbl/macros.h>
#include <fbl/mutex.h>

#include "poisson_sampler.h"

namespace memory_sampler {

// Allocation recorder. This class is designed to be initialized in
// static storage, to be thread safe, and to be suitable for use
// during an allocation or a deallocation (by being safe to call
// from within the scudo allocation hooks).
//
// It establishes a FIDL connection to a sampling profiler on
// startup, reports allocations and deallocations, and relevant
// information to support symbolization of the collected profiles.
class Recorder {
 public:
  DISALLOW_COPY_ASSIGN_AND_MOVE(Recorder);
  // Returns a reference to the singleton object, initializing (and
  // blocking) it if necessary.
  static Recorder *Get();
  // Returns a pointer to the singleton object if it is initialized;
  // returns nullptr otherwise.
  static Recorder *GetIfReady();
  // Decides whether to discard or sample this allocation, and acts
  // appropriately.
  void MaybeRecordAllocation(void *address, size_t size) __TA_EXCLUDES(&lock_);
  // Decides whether to discard or sample this deallocation, and acts
  // appropriately.
  void MaybeForgetAllocation(void *address) __TA_EXCLUDES(&lock_);
  // Collects the module layout of the current process and
  // communicates it to the profiler.
  void SetModulesInfo();

  // Convenience factory method for use in tests. This eschews the
  // singleton interface, and supports providing a custom FIDL
  // client. It does not perform any of the initialisations handled by
  // the singleton interface. This should not be used outside of tests.
  static Recorder CreateRecorderForTesting(fidl::SyncClient<fuchsia_memory_sampler::Sampler> client,
                                           std::function<PoissonSampler &()> get_poisson_sampler);

  // The average count of bytes allocated between two samples.
  static constexpr size_t kSamplingIntervalBytes = static_cast<size_t>(128 * 1024);

 private:
  Recorder(fidl::SyncClient<fuchsia_memory_sampler::Sampler> client,
           std::function<PoissonSampler &()> get_poisson_sampler);
  // Initializes the singleton into statically-allocated storage.
  static void InitSingletonOnce();
  fbl::Mutex lock_;
  fidl::SyncClient<fuchsia_memory_sampler::Sampler> client_ __TA_GUARDED(&lock_);
  std::unordered_set<void *> recorded_allocations_ __TA_GUARDED(&lock_);
  std::function<PoissonSampler &()> GetPoissonSampler;

  // Records an allocation's address and size and communicates it to
  // the profiler.
  void RecordAllocation(void *address, size_t size) __TA_EXCLUDES(&lock_);
  // Records a deallocation's address and communicates it to the
  // profiler.
  void ForgetAllocation(void *address) __TA_EXCLUDES(&lock_);
};
}  // namespace memory_sampler

#endif  // SRC_PERFORMANCE_MEMORY_SAMPLER_INSTRUMENTATION_RECORDER_H_
