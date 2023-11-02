// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib.h"

#include <lib/fit/defer.h>
#include <lib/zx/clock.h>
#include <lib/zx/event.h>
#include <lib/zx/exception.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>

#include <thread>

#include <zxtest/zxtest.h>

namespace {

enum class TestState : uint8_t {
  Mapped,
  Unmapped,
};

void Reader(std::atomic<uint32_t>& num_readers_ready, std::atomic<TestState>& mapped,
            uint64_t* shared_value, uint64_t* read_value) {
  // Attempt to read from the shared value. This should succeed because the value has been mapped
  // at this point.
  *read_value = *shared_value;

  // Signal to the orchestrator that we've completed our read and that it should unmap the
  // shared_value.
  num_readers_ready.fetch_add(1);

  // Wait for the orchestrator to unmap.
  while (mapped.load() == TestState::Mapped) {
  }

  // Now, this read should fault. The orchestrator should ensure that it does so.
  *read_value = *shared_value;
}

void RunTestIteration() {
  const uint32_t kNumReaders = 16;
  zx::process procs[kNumReaders];
  zx::vmar vmars[kNumReaders];
  zx::channel exception_channels[kNumReaders];
  uint64_t read_values[kNumReaders];

  std::vector<std::thread> threads;
  threads.reserve(kNumReaders);

  // Create a sub-vmar that will contain the address we are mapping. We do this to make sure no
  // other code can "steal" this address out from under us.
  auto page_size = zx_system_get_page_size();
  zx::vmar sub_vmar;
  uintptr_t vmar_addr;
  ASSERT_OK(zx::vmar::root_self()->allocate(ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE, 0, page_size,
                                            &sub_vmar, &vmar_addr));
  auto cleanup = fit::defer([&sub_vmar]() { ASSERT_OK(sub_vmar.destroy()); });

  // Map a shared address.
  zx::vmo vmo;
  zx_vaddr_t shared_addr;
  ASSERT_OK(zx::vmo::create(page_size, 0, &vmo));
  ASSERT_OK(sub_vmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, page_size, &shared_addr));
  ASSERT_EQ(shared_addr, (zx_vaddr_t)vmar_addr);

  // Write a random value (in this case the current ticks) to addr.
  zx_ticks_t expected_value = zx::ticks::now().get();
  *reinterpret_cast<volatile uint64_t*>(shared_addr) = expected_value;

  std::atomic<TestState> mapped = TestState::Mapped;
  std::atomic<uint32_t> num_readers_ready = 0;
  for (uint32_t i = 0; i < kNumReaders; i++) {
    // Create a shared process to house the reader thread.
    auto proc_name = fbl::StringPrintf("%u-unified-shared-stress-reader", i);
    ASSERT_OK(zx_process_create_shared(zx_process_self(), 0, proc_name.data(), proc_name.size(),
                                       procs[i].reset_and_get_address(),
                                       vmars[i].reset_and_get_address()));

    // Create an exception channel attached to the shared process.
    ASSERT_OK(procs[i].create_exception_channel(0, &exception_channels[i]));

    // Create the reader thread.
    zx_handle_t orig_proc = thrd_set_zx_process(procs[i].get());
    threads.emplace_back(Reader, std::ref(num_readers_ready), std::ref(mapped),
                         (uint64_t*)(shared_addr), &read_values[i]);
    thrd_set_zx_process(orig_proc);
  }

  // Wait for all of the readers to signal to us that they have read the value and are ready.
  while (num_readers_ready.load() != kNumReaders) {
  }

  // Verify that all of the readers read the expected value out of the given address.
  for (uint32_t i = 0; i < kNumReaders; i++) {
    EXPECT_EQ(read_values[i], expected_value);
  }

  // Unmap the shared address and signal to the readers that we have done so.
  ASSERT_OK(sub_vmar.unmap(shared_addr, page_size));
  mapped.store(TestState::Unmapped);

  // Wait for all the readers to fault.
  zx::exception exceptions[kNumReaders];
  for (uint32_t i = 0; i < kNumReaders; i++) {
    zx_exception_info_t info;
    ASSERT_OK(exception_channels[i].wait_one(ZX_CHANNEL_READABLE, zx::time::infinite(), nullptr));
    ASSERT_OK(exception_channels[i].read(0, &info, exceptions[i].reset_and_get_address(),
                                         sizeof(info), 1, nullptr, nullptr));
    EXPECT_EQ(info.type, ZX_EXCP_FATAL_PAGE_FAULT);
  }

  // Remap the VMO.
  ASSERT_OK(sub_vmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, page_size, &shared_addr));
  ASSERT_EQ(shared_addr, (zx_vaddr_t)vmar_addr);
  expected_value = zx::ticks::now().get();
  *reinterpret_cast<volatile uint64_t*>(shared_addr) = expected_value;

  // Resume all of the readers.
  for (uint32_t i = 0; i < kNumReaders; i++) {
    uint32_t handled = ZX_EXCEPTION_STATE_HANDLED;
    ASSERT_OK(exceptions[i].set_property(ZX_PROP_EXCEPTION_STATE, &handled, sizeof(handled)));
    exceptions[i].reset();
  }

  // Wait for all of the readers to complete successfully.
  for (size_t i = 0; i < threads.size(); i++) {
    threads[i].join();
  }

  // Once again, verify that all of the readers read the expected value out of the given address.
  for (uint32_t i = 0; i < kNumReaders; i++) {
    EXPECT_EQ(read_values[i], expected_value);
  }
}

}  // namespace

void Orchestrator(zx::duration runtime, zx::duration poll_interval) {
  const zx::time deadline = zx::deadline_after(runtime);
  zx::time poll_deadline = zx::deadline_after(poll_interval);
  uint64_t num_iterations = 0;
  printf("running stress test for %ld seconds\n", runtime.to_secs());
  while (zx::clock::get_monotonic() < deadline) {
    if (zx::clock::get_monotonic() > poll_deadline) {
      printf("test still running, %lu iterations completed\n", num_iterations);
      poll_deadline = zx::deadline_after(poll_interval);
    }
    RunTestIteration();
    num_iterations++;
  }
}
