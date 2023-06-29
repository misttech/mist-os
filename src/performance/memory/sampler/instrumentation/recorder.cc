// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "recorder.h"

#include <elf-search.h>
#include <fidl/fuchsia.memory.sampler/cpp/fidl.h>
#include <fidl/fuchsia.memory.sampler/cpp/natural_types.h>
#include <fidl/fuchsia.memory.sampler/cpp/wire_types.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/zx/result.h>
#include <pthread.h>
#include <zircon/assert.h>
#include <zircon/sanitizer.h>
#include <zircon/syscalls/object.h>

#include <atomic>
#include <cstdint>
#include <vector>

#include <fbl/auto_lock.h>

#include "poisson_sampler.h"

namespace {
// Note: The destructor is never called. This is on purpose.
std::atomic<memory_sampler::Recorder*> singleton;

// Threshold for truncating overly long stack frames.
constexpr size_t kMaxStackFramesLength = 512;

// Note: this implementation currently relies on `thread_local` to
// track the allocated bytes and thresholds, which means that each
// thread implements a different Poisson process.
memory_sampler::PoissonSampler& GetNonDeterministicPoissonSampler() {
  thread_local memory_sampler::PoissonSampler sampler{
      memory_sampler::Recorder::kSamplingIntervalBytes};
  return sampler;
}
}  // namespace

namespace memory_sampler {

// Upon connecting, transmits the memory layout of the current process.
void Recorder::InitSingletonOnce() {
  alignas(Recorder) static std::byte storage[sizeof(Recorder)];
  auto result = component::Connect<fuchsia_memory_sampler::Sampler>();
  ZX_ASSERT(result.is_ok());
  auto* recorder = new (storage)
      Recorder(fidl::SyncClient{std::move(result.value())}, GetNonDeterministicPoissonSampler);

  recorder->SetModulesInfo();

  // Only set the singleton once all allocations are done, to avoid a deadlock.
  singleton.store(recorder);
}

Recorder* Recorder::Get() {
  static std::once_flag once_flag;
  std::call_once(once_flag, InitSingletonOnce);
  return singleton.load();
}

Recorder* Recorder::GetIfReady() { return singleton.load() == nullptr ? nullptr : Get(); }

// Profiling every single allocation has a large impact on the
// performance of the instrumented process. Sampling allocation
// profilers work around this issue by sampling a subset of the
// allocations, reducing the overhead while hopefully capturing enough
// relevant data to be useful.
void Recorder::MaybeRecordAllocation(void* address, size_t size) {
  if (!GetPoissonSampler().ShouldSampleAllocation(size))
    return;

  // Store the address of the allocation.
  {
    fbl::AutoLock lock(&lock_);
    recorded_allocations_.emplace(address);
  }

  RecordAllocation(address, size);
}

void Recorder::RecordAllocation(void* address, size_t size) {
  // Collect a stack trace.
  uint64_t pc_buffer[kMaxStackFramesLength]{0};
  const size_t count = __sanitizer_fast_backtrace(pc_buffer, kMaxStackFramesLength);
  {
    fbl::AutoLock lock(&lock_);
    auto result = client_->RecordAllocation(
        {{.address = reinterpret_cast<uint64_t>(address),
          .stack_trace = {{.stack_frames = std::vector<uint64_t>(pc_buffer, pc_buffer + count)}},
          .size = size}});

    ZX_ASSERT(result.is_ok());
  }
}

void Recorder::MaybeForgetAllocation(void* address) {
  {
    fbl::AutoLock lock(&lock_);
    auto allocation = recorded_allocations_.find(address);
    if (allocation == recorded_allocations_.end()) {
      return;
    }
    recorded_allocations_.erase(allocation);
  }

  ForgetAllocation(address);
}

void Recorder::ForgetAllocation(void* address) {
  // Collect a stack trace.
  uint64_t pc_buffer[kMaxStackFramesLength]{0};
  const size_t count = __sanitizer_fast_backtrace(pc_buffer, kMaxStackFramesLength);

  {
    fbl::AutoLock lock(&lock_);

    auto result = client_->RecordDeallocation(
        {{.address = reinterpret_cast<uint64_t>(address),
          .stack_trace = {{.stack_frames = std::vector<uint64_t>(pc_buffer, pc_buffer + count)}}}});
    ZX_ASSERT(result.is_ok());
  }
}

void Recorder::SetModulesInfo() {
  // Collect the layout of the modules loaded in memory.
  const zx_handle_t process = zx_process_self();
  std::vector<fuchsia_memory_sampler::ModuleMap> modules;

  // Iterate through modules to map code memory ranges to the
  // corresponding build id.
  elf_search::ForEachModule(
      *zx::unowned_process{process}, [&modules](const elf_search::ModuleInfo& info) mutable {
        const size_t kPageSize = zx_system_get_page_size();
        std::vector<fuchsia_memory_sampler::ExecutableSegment> segments;

        // Iterate through program segments.
        for (const auto& phdr : info.phdrs) {
          // Skip non-loadable sections.
          if (phdr.p_type != PT_LOAD) {
            continue;
          }
          // Skip non-executable sections.
          bool executable = !!(phdr.p_flags & PF_X);
          if (!executable) {
            continue;
          }

          const uintptr_t start = phdr.p_vaddr & -kPageSize;
          const uintptr_t end = (phdr.p_vaddr + phdr.p_memsz + kPageSize - 1) & -kPageSize;
          auto& segment = segments.emplace_back();
          segment.start_address(info.vaddr + start);
          segment.size(end - start);
          segment.relative_address(start);
        }

        auto& module_map = modules.emplace_back();
        module_map.build_id({{info.build_id.begin(), info.build_id.end()}});
        module_map.executable_segments(segments);
      });

  // Retrieve the name of the current process.
  char name[ZX_MAX_NAME_LEN];
  zx_object_get_property(process, ZX_PROP_NAME, name, ZX_MAX_NAME_LEN);

  // Perform the FIDL call.
  {
    fbl::AutoLock lock(&lock_);
    auto result = client_->SetProcessInfo({{.process_name = name, .module_map = modules}});
    ZX_ASSERT(result.is_ok());
  }
}

Recorder::Recorder(fidl::SyncClient<fuchsia_memory_sampler::Sampler> client,
                   std::function<PoissonSampler&()> get_poisson_sampler)
    : client_(std::move(client)), GetPoissonSampler(std::move(get_poisson_sampler)) {}

Recorder Recorder::CreateRecorderForTesting(
    fidl::SyncClient<fuchsia_memory_sampler::Sampler> client,
    std::function<PoissonSampler&()> get_poisson_sampler) {
  return Recorder{std::move(client), std::move(get_poisson_sampler)};
}
}  // namespace memory_sampler
