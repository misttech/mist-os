// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-startup-create-process-tests.h"

#include <lib/elfldltl/machine.h>
#include <lib/zx/job.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include <gtest/gtest.h>

namespace ld::testing {

zx::unowned_vmo LdStartupCreateProcessTestsBase::GetVdsoVmo() {
  static const zx::vmo vdso{zx_take_startup_handle(PA_HND(PA_VMO_VDSO, 0))};
  return vdso.borrow();
}

void LdStartupCreateProcessTestsBase::Init(std::initializer_list<std::string_view> args) {
  std::string_view name = process_name();
  zx::process process;
  ASSERT_EQ(zx::process::create(*zx::job::default_job(), name.data(),
                                static_cast<uint32_t>(name.size()), 0, &process, &root_vmar_),
            ZX_OK);
  set_process(std::move(process));

  ASSERT_EQ(zx::thread::create(this->process(), name.data(), static_cast<uint32_t>(name.size()), 0,
                               &thread_),
            ZX_OK);

  fbl::unique_fd log_fd;
  ASSERT_NO_FATAL_FAILURE(InitLog(log_fd));
  ASSERT_NO_FATAL_FAILURE(bootstrap()
                              .AddProcess(this->process().borrow())
                              .AddThread(thread_.borrow())
                              .AddAllocationVmar(root_vmar_.borrow())
                              .AddFd(STDERR_FILENO, std::move(log_fd))
                              .SetArgs(args));
}

LdStartupCreateProcessTestsBase::~LdStartupCreateProcessTestsBase() = default;

int64_t LdStartupCreateProcessTestsBase::Run() {
  // Allocate the stack.  This is delayed until here in case the test uses
  // bootstrap() methods after Init() that affect bootstrap().GetStackSize().
  zx::vmo stack_vmo;
  uintptr_t sp;
  auto allocate_stack = [this, &stack_vmo, &sp]() {
    std::optional<size_t> stack_size = bootstrap().GetStackSize();
    if (!stack_size) {
      ASSERT_TRUE(stack_size_);
      stack_size = stack_size_;
    } else {
      // TODO(mcgrathr): stack use too big for procargs piddly default
      stack_size = 64 << 10;
    }

    const size_t page_size = zx_system_get_page_size();
    const size_t stack_vmo_size = (*stack_size + page_size - 1) & -page_size;
    const size_t stack_vmar_size = stack_vmo_size + page_size;

    ASSERT_EQ(zx::vmo::create(stack_vmo_size, 0, &stack_vmo), ZX_OK);

    zx::vmar stack_vmar;
    uintptr_t stack_vmar_base;
    ASSERT_EQ(
        root_vmar().allocate(ZX_VM_CAN_MAP_SPECIFIC | ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE, 0,
                             stack_vmar_size, &stack_vmar, &stack_vmar_base),
        ZX_OK);

    zx_vaddr_t stack_base;
    ASSERT_EQ(
        stack_vmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_SPECIFIC | ZX_VM_ALLOW_FAULTS,
                       page_size, stack_vmo, 0, stack_vmo_size, &stack_base),
        ZX_OK);

    ASSERT_NO_FATAL_FAILURE(bootstrap().AddStackVmo(std::move(stack_vmo)));

    sp = elfldltl::AbiTraits<>::InitialStackPointer(stack_base, stack_vmo_size);
  };

  allocate_stack();
  if (::testing::Test::HasFailure()) {
    return -1;
  }

  // Pack up the bootstrap message and start the process running.
  auto start_process = [this, sp]() {
    zx::channel bootstrap_receiver = procargs_.PackBootstrap();

    ASSERT_EQ(this->process().start(thread_, entry_, sp, std::move(bootstrap_receiver), vdso_base_),
              ZX_OK);
  };

  start_process();
  if (::testing::Test::HasFailure()) {
    return -1;
  }

  return Wait();
}

}  // namespace ld::testing
