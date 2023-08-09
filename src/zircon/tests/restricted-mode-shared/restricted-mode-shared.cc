// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/job.h>
#include <lib/zx/resource.h>
#include <lib/zx/result.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>

#include <thread>

#include <zxtest/zxtest.h>

#include "write_to_stack_blob.h"

// Because zx_restricted_enter doesn't return like normal syscalls,
// we use a wrapper function to make it easier to use.
extern "C" zx_status_t restricted_enter_wrapper(uint32_t options,
                                                zx_restricted_reason_t* reason_code);

namespace {

zx::result<zx::resource> GetVmexResource() {
  zx::result client = component::Connect<fuchsia_kernel::VmexResource>();
  if (client.is_error()) {
    return client.take_error();
  }
  fidl::WireResult result = fidl::WireCall(client.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

void RunRestrictedMode(zx_handle_t restricted_vmar_handle, zx_status_t* result) {
  zx::vmar rvmar(restricted_vmar_handle);

  // Set up the restricted mode stack.
  zx::vmo stack;
  zx_vaddr_t stack_addr;
  auto page_size = zx_system_get_page_size();
  *result = zx::vmo::create(page_size, 0, &stack);
  if (*result != ZX_OK) {
    return;
  }
  *result = rvmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, stack, 0, page_size, &stack_addr);
  if (*result != ZX_OK) {
    return;
  }

  // Set up the restricted mode code segment.
  zx::vmo cs;
  zx::vmo cs_exe;
  zx_vaddr_t cs_addr;
  *result = zx::vmo::create(page_size, 0, &cs);
  if (*result != ZX_OK) {
    return;
  }
  cpp20::span<const std::byte> code_blob = write_to_stack_blob();
  *result = cs.write(code_blob.data(), 0, code_blob.size_bytes());
  if (*result != ZX_OK) {
    return;
  }
  auto vmex = GetVmexResource();
  *result = vmex.status_value();
  if (*result != ZX_OK) {
    return;
  }
  *result = cs.replace_as_executable(*vmex, &cs_exe);
  if (*result != ZX_OK) {
    return;
  }
  *result = rvmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_EXECUTE, 0, cs_exe, 0, page_size, &cs_addr);
  if (*result != ZX_OK) {
    return;
  }

  // Set up the restricted state.
  zx::vmo restricted;
  *result = zx_restricted_bind_state(0, restricted.reset_and_get_address());
  if (*result != ZX_OK) {
    return;
  }
  zx_restricted_state_t state{};
#if defined(__x86_64__)
  state.rsp = reinterpret_cast<uintptr_t>(stack_addr + page_size);
  state.ip = reinterpret_cast<uintptr_t>(cs_addr);
#elif defined(__aarch64__) || defined(__riscv)  // defined(__x86_64__)
  state.sp = reinterpret_cast<uintptr_t>(stack_addr + page_size);
  state.pc = reinterpret_cast<uintptr_t>(cs_addr);
#endif                                          // defined(__arch64__) || defined(__riscv)
  *result = restricted.write(&state, 0, sizeof(state));
  if (*result != ZX_OK) {
    return;
  }

  // Enter restricted mode.
  zx_restricted_reason_t exit_reason = 99;
  *result = restricted_enter_wrapper(0, &exit_reason);
  if (*result != ZX_OK) {
    return;
  }
  if (exit_reason != ZX_RESTRICTED_REASON_SYSCALL) {
    *result = ZX_ERR_INTERNAL;
    return;
  }

  // Validate that the restricted mode routine wrote the right value to the stack.
  uint8_t buffer[8];
  *result = stack.read(buffer, page_size - 8, 8);
  if (*result != ZX_OK) {
    return;
  }
  uint8_t expected[8] = {0xdd, 0xdd, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa};
  for (size_t i = 0; i < sizeof(buffer); i++) {
    if (expected[i] != buffer[i]) {
      *result = ZX_ERR_INTERNAL;
      return;
    }
  }
  *result = ZX_OK;
}

}  // namespace

TEST(RestrictedModeUnified, Basic) {
  // Create the shared process that will run restricted mode.
  static constexpr char kProcName[] = "restricted-shared-test";
  zx::process restricted_proc;
  zx::vmar restricted_vmar;
  ASSERT_OK(zx_process_create_shared(zx_process_self(), 0, kProcName, sizeof(kProcName),
                                     restricted_proc.reset_and_get_address(),
                                     restricted_vmar.reset_and_get_address()));

  // Spawn a thread inside the shared process that executes `RunRestrictedMode`. We want this
  // thread to have all of the functionality provided by the C runtime (safe stack, libraries,
  // etc.) but we don't want to have to do this ourselves. We accomplish this by setting the
  // current thread's process handle to the shared process' handle, and then spawning a thread
  // using C++ primitives.
  auto prev_proc = thrd_set_zx_process(restricted_proc.get());
  zx_status_t result = ZX_ERR_INTERNAL;
  std::thread restricted_thread(RunRestrictedMode, restricted_vmar.get(), &result);
  thrd_set_zx_process(prev_proc);

  // Wait for the restricted thread to complete.
  restricted_thread.join();
  ASSERT_OK(result);
}
