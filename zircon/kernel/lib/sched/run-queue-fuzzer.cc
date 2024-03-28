// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/sched/run-queue.h>
#include <lib/sched/thread-base.h>
#include <zircon/time.h>

#include <memory>
#include <vector>

#include <fuzzer/FuzzedDataProvider.h>

#include "test-thread.h"

//
// This file defines a simple fuzzer of sched::RunQueue, simulating the ongoing
// scheduling of threads.
//

namespace {

using Duration = sched::Duration;
using Time = sched::Time;

Time ConsumeTime(FuzzedDataProvider& provider, Time max = Time::Max()) {
  return Time{provider.ConsumeIntegralInRange<zx_time_t>(Time::Min().raw_value(), max.raw_value())};
}

Duration ConsumeDuration(FuzzedDataProvider& provider, Duration max = Duration::Max()) {
  return Duration{provider.ConsumeIntegralInRange<zx_duration_t>(1, max.raw_value())};
}

TestThread* AllocateNewThread(FuzzedDataProvider& provider,
                              std::vector<std::unique_ptr<TestThread>>& threads,
                              Time max_start = Time::Max()) {
  Time start = ConsumeTime(provider, max_start);
  Duration period = ConsumeDuration(provider, Time::Max() - start);
  Duration firm_capacity = ConsumeDuration(provider, period);
  threads.emplace_back(std::make_unique<TestThread>(
      sched::BandwidthParameters{
          .period = period,
          .firm_capacity = firm_capacity,
      },
      start));
  return threads.back().get();
}

}  // namespace

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  FuzzedDataProvider provider(data, size);

  Time now = ConsumeTime(provider);

  // Create the first thread and ensure it's active.
  std::vector<std::unique_ptr<TestThread>> threads;
  TestThread* current = AllocateNewThread(provider, threads, now);
  current->ReactivateIfExpired(now);

  sched::RunQueue<TestThread> queue;
  while (now < Time::Max() && provider.remaining_bytes() > 0) {
    if (bool new_thread = provider.ConsumeBool()) {
      queue.Queue(*AllocateNewThread(provider, threads), now);
    }

    ZX_ASSERT(current);
    ZX_ASSERT(!current->IsQueued());
    ZX_ASSERT(current->start() <= now);

    auto [next, preemption] = queue.EvaluateNextThread(*current, now);

    ZX_ASSERT(next);
    ZX_ASSERT(current == next || current->IsQueued());
    ZX_ASSERT(!next->IsQueued());
    ZX_ASSERT(next->start() <= now);
    ZX_ASSERT(now < preemption);
    ZX_ASSERT(preemption <= next->finish());

    // Advance time.
    next->Tick(preemption - now);
    now = preemption;
    current = next;
  }

  return 0;
}
