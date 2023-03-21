// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "recorder.h"

namespace {
// Static variable only used to initialize the `Recorder` at startup
// time as a global constructor. This variable should not be read.
struct RecorderInitializer {
  RecorderInitializer() { memory_sampler::Recorder::Get(); }
} recorder_initializer;
}  // namespace

extern "C" {
// Registers a callback to be called by the Scudo allocator whenever
// it serves an allocation. Due to reentrancy issues, this function is
// forbidden to allocate.
__attribute__((visibility("default"))) void __scudo_allocate_hook(void* ptr, unsigned int size) {
  auto* recorder = memory_sampler::Recorder::GetIfReady();
  if (recorder == nullptr)
    return;
  recorder->RecordAllocation(reinterpret_cast<uint64_t>(ptr), size);
}
// Registers a callback to be called by the Scudo allocator whenever
// it serves a deallocation. Due to reentrancy issues, this function
// is forbidden to allocate.
__attribute__((visibility("default"))) void __scudo_deallocate_hook(void* ptr) {
  auto* recorder = memory_sampler::Recorder::GetIfReady();
  if (recorder == nullptr)
    return;
  recorder->ForgetAllocation(reinterpret_cast<uint64_t>(ptr));
}
}
