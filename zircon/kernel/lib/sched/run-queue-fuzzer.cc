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

Time ConsumeTime(FuzzedDataProvider& provider) {
  return Time{provider.ConsumeIntegral<zx_time_t>()};
}

Duration ConsumeDuration(FuzzedDataProvider& provider, Duration max = Duration::Max()) {
  ZX_ASSERT(max > 0);
  return Duration{provider.ConsumeIntegralInRange<zx_duration_t>(1, max.raw_value())};
}

TestThread* AllocateNewThread(FuzzedDataProvider& provider,
                              std::vector<std::unique_ptr<TestThread>>& threads) {
  Time start = ConsumeTime(provider);
  Duration period = ConsumeDuration(provider, (Time::Max() - start) + Time{1});
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

  std::vector<std::unique_ptr<TestThread>> threads;
  Time now = ConsumeTime(provider);
  TestThread* current = nullptr;
  sched::RunQueue<TestThread> queue;
  while (now < Time::Max() && provider.remaining_bytes() > 0) {
    if (provider.ConsumeBool()) {
      queue.Queue(*AllocateNewThread(provider, threads), now);
    }

    ZX_ASSERT(current == queue.current_thread());
    if (current) {
      ZX_ASSERT(!current->IsQueued());
      ZX_ASSERT(current->start() <= now);
    }

    auto [next, preemption] = queue.SelectNextThread(now);

    ZX_ASSERT(current == next || !current || current->IsQueued());

    if (next) {
      ZX_ASSERT(!next->IsQueued());
      ZX_ASSERT(next->start() <= now);
      ZX_ASSERT(now < preemption);
      ZX_ASSERT(preemption <= next->finish());

      next->Tick(preemption - now);
    }

    now = preemption;
    current = next;
  }

  return 0;
}
