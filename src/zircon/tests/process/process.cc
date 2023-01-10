// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/test/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/defer.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <zircon/errors.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>

#include <mini-process/mini-process.h>

#include "src/lib/test-suite/test_suite.h"

#define ASSERT_OK(e)                      \
  if (e != ZX_OK) {                       \
    return fuchsia::test::Status::FAILED; \
  }

#define ASSERT_EQ(e1, e2)                 \
  if (e1 != e2) {                         \
    return fuchsia::test::Status::FAILED; \
  }

#define ASSERT_NE(e1, e2)                 \
  if (e1 == e2) {                         \
    return fuchsia::test::Status::FAILED; \
  }

fuchsia::test::Status shared_map_in_prototype() {
  zx::process prototype_process;
  zx::vmar shared_vmar;
  constexpr const char kPrototypeName[] = "prototype_process";
  ASSERT_OK(zx::process::create(*zx::job::default_job(), kPrototypeName, sizeof(kPrototypeName),
                                ZX_PROCESS_SHARED, &prototype_process, &shared_vmar));

  zx::process process;
  zx::vmar restricted_vmar;
  constexpr const char kProcessName[] = "process";
  if (zx_process_create_shared(prototype_process.get(), 0, kProcessName, sizeof(kProcessName),
                               process.reset_and_get_address(),
                               restricted_vmar.reset_and_get_address()) != ZX_OK) {
    return fuchsia::test::Status::FAILED;
  }

  // Map a vmo into the shared vmar using the prototype handle.
  zx_handle_t vmo;
  ASSERT_OK(zx_vmo_create(zx_system_get_page_size(), 0, &vmo));
  uintptr_t addr;
  ASSERT_OK(zx_vmar_map(shared_vmar.get(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                        zx_system_get_page_size(), &addr));

  // Write some data to the vmo.
  std::vector<char> shared_data = {'a', 'b', 'c'};
  ASSERT_OK(zx_vmo_write(vmo, shared_data.data(), 0, shared_data.size()));

  // Read process memory from both processes to make sure they have the data.
  std::vector<char> read_data = {'d', 'e', 'f'};
  size_t actual = 0;
  ASSERT_EQ(prototype_process.read_memory(addr, read_data.data(), read_data.size(), &actual),
            ZX_OK);
  ASSERT_EQ(read_data, shared_data);

  // Now read the same address from the second process.
  read_data = {'d', 'e', 'f'};
  ASSERT_EQ(process.read_memory(addr, read_data.data(), read_data.size(), &actual), ZX_OK);
  ASSERT_EQ(read_data, shared_data);

  return fuchsia::test::Status::PASSED;
}

fuchsia::test::Status restricted_vmar_not_shared() {
  zx::process prototype_process;
  zx::vmar shared_vmar;
  constexpr const char kPrototypeName[] = "prototype_process";
  ASSERT_OK(zx::process::create(*zx::job::default_job(), kPrototypeName, sizeof(kPrototypeName),
                                ZX_PROCESS_SHARED, &prototype_process, &shared_vmar));

  zx::process process;
  zx::vmar restricted_vmar;
  constexpr const char kProcessName[] = "process";
  ASSERT_OK(zx_process_create_shared(prototype_process.get(), 0, kProcessName, sizeof(kProcessName),
                                     process.reset_and_get_address(),
                                     restricted_vmar.reset_and_get_address()));

  // Map a vmo into the restricted vmar of the second process.
  zx_handle_t vmo;
  ASSERT_OK(zx_vmo_create(zx_system_get_page_size(), 0, &vmo));
  uintptr_t addr;
  ASSERT_OK(zx_vmar_map(restricted_vmar.get(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                        zx_system_get_page_size(), &addr));

  // Write some data to the vmo.
  std::vector<char> restricted_data = {'a', 'b', 'c'};
  ASSERT_OK(zx_vmo_write(vmo, restricted_data.data(), 0, restricted_data.size()));

  std::vector<char> read_data = {'d', 'e', 'f'};
  size_t actual = 0;
  // Now try to read the same address from the first process, which should fail.
  if (prototype_process.read_memory(addr, read_data.data(), read_data.size(), &actual) == ZX_OK) {
    // If `addr` happened to be valid, make sure that the read data was not the restricted data from
    // the prototype.
    ASSERT_NE(read_data, restricted_data);
  }

  return fuchsia::test::Status::PASSED;
}

fuchsia::test::Status shared_process_invalid_prototype() {
  zx::process prototype_process;
  zx::vmar prototype_vmar;
  constexpr const char kPrototypeName[] = "prototype_process";
  ASSERT_OK(zx::process::create(*zx::job::default_job(), kPrototypeName, sizeof(kPrototypeName), 0,
                                &prototype_process, &prototype_vmar));

  zx::process process;
  zx::vmar restricted_vmar;
  constexpr const char kProcessName[] = "process";
  ASSERT_EQ(zx_process_create_shared(prototype_process.get(), 0, kProcessName, sizeof(kProcessName),
                                     process.reset_and_get_address(),
                                     restricted_vmar.reset_and_get_address()),
            ZX_ERR_INVALID_ARGS);

  return fuchsia::test::Status::PASSED;
}

fuchsia::test::Status info_process_vmos() {
  // Build a shareable process.
  static constexpr char kSharedProcessName[] = "object-info-shar-proc";
  zx::process shared_process;
  zx::vmar shared_vmar;
  ASSERT_OK(zx::process::create(*zx::job::default_job(), kSharedProcessName,
                                sizeof(kSharedProcessName), ZX_PROCESS_SHARED, &shared_process,
                                &shared_vmar));

  // Build another process that shares its address space.
  static constexpr char kPrivateProcessName[] = "object-info-priv-proc";
  zx::process private_process;
  zx::vmar private_vmar;
  ASSERT_OK(zx_process_create_shared(
      shared_process.get(), 0, kPrivateProcessName, sizeof(kPrivateProcessName),
      private_process.reset_and_get_address(), private_vmar.reset_and_get_address()));

  // Map a single VMO that contains some code (a busy loop) and a stack into the shared address
  // space.
  zx_vaddr_t stack_base, sp;
  ASSERT_OK(mini_process_load_stack(shared_vmar.get(), true, &stack_base, &sp));

  // Allocate two extra VMOs.
  const size_t kVmoSize = zx_system_get_page_size();
  zx::vmo vmo1, vmo2;
  ASSERT_OK(zx::vmo::create(kVmoSize, 0u, &vmo1));
  ASSERT_OK(zx::vmo::create(kVmoSize, 0u, &vmo2));

  // Start the shared process and pass a handle to vmo1 to it.
  zx::thread shared_thread;
  static constexpr char kSharedThreadName[] = "object-info-shar-thrd";
  ASSERT_OK(zx::thread::create(shared_process, kSharedThreadName, sizeof(kSharedThreadName), 0u,
                               &shared_thread));
  ASSERT_OK(shared_process.start(shared_thread, stack_base, sp, std::move(vmo1), 0));
  auto shared_cleanup = fit::defer([&] { shared_process.kill(); });

  // Start the private process.
  zx::thread private_thread;
  static constexpr char kPrivateThreadName[] = "object-info-priv-thrd";
  ASSERT_OK(zx::thread::create(private_process, kPrivateThreadName, sizeof(kPrivateThreadName), 0u,
                               &private_thread));
  ASSERT_OK(private_process.start(private_thread, stack_base, sp, zx::handle(), 0));
  auto private_cleanup = fit::defer([&] { private_process.kill(); });

  // Map vmo2 into the private address space only.
  zx_vaddr_t vaddr;
  ASSERT_OK(private_vmar.map(ZX_VM_PERM_READ, 0u, vmo2, 0, kVmoSize, &vaddr));

  // Buffer big enough to read all of the test processes' VMO entries: the mini-process VMO, vmo1
  // and vmo2.
  const size_t buf_size = 3;
  std::unique_ptr<zx_info_vmo_t[]> buf(new zx_info_vmo_t[buf_size]);

  // Read the VMO entries of the shared process.
  size_t actual, available_shared;
  ASSERT_OK(shared_process.get_info(ZX_INFO_PROCESS_VMOS, buf.get(),
                                    buf_size * sizeof(zx_info_vmo_t), &actual, &available_shared));
  ASSERT_EQ(actual, available_shared);
  ASSERT_EQ(2, available_shared);

  // Read the VMO entries of the private process.
  size_t available_private;
  ASSERT_OK(private_process.get_info(ZX_INFO_PROCESS_VMOS, buf.get(),
                                     buf_size * sizeof(zx_info_vmo_t), &actual,
                                     &available_private));
  ASSERT_EQ(actual, available_private);
  ASSERT_EQ(available_shared + 1, available_private);

  // Verify that the VMO entries from the private address space are accounted correctly if they
  // don't fit in the buffer.
  const size_t smallbuf_size = available_shared;
  std::unique_ptr<zx_info_vmo_t[]> smallbuf(new zx_info_vmo_t[smallbuf_size]);
  size_t actual_smallbuf, available_smallbuf;
  ASSERT_OK(private_process.get_info(ZX_INFO_PROCESS_VMOS, smallbuf.get(),
                                     smallbuf_size * sizeof(zx_info_vmo_t), &actual_smallbuf,
                                     &available_smallbuf));
  ASSERT_EQ(smallbuf_size, actual_smallbuf);
  ASSERT_EQ(available_private, available_smallbuf);

  return fuchsia::test::Status::PASSED;
}

int main(int argc, const char** argv) {
  printf("test");
  std::vector<example::TestInput> inputs = {
      {.name = "ShareableProcess.GetInfo", .status = info_process_vmos()},
      {.name = "ProcessTest.SharedMapInPrototype", .status = shared_map_in_prototype()},
      {.name = "ProcessTest.RestrictedVmarNotShared", .status = restricted_vmar_not_shared()},
      {.name = "ProcessTest.SharedProcessInvalidPrototype",
       .status = shared_process_invalid_prototype()},
  };

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto context = sys::ComponentContext::CreateAndServeOutgoingDirectory();

  example::TestSuite suite(&loop, std::move(inputs));
  context->outgoing()->AddPublicService(suite.GetHandler());

  loop.Run();
  return 0;
}
