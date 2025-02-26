// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-load-zircon-process-tests-base.h"

#include <lib/elfldltl/machine.h>
#include <lib/ld/abi.h>
#include <zircon/processargs.h>

#include <gtest/gtest.h>

namespace ld::testing {

const char* LdLoadZirconProcessTestsBase::process_name() const {
  return ::testing::UnitTest::GetInstance()->current_test_info()->name();
}

void LdLoadZirconProcessTestsBase::set_process(zx::process process) {
  ASSERT_FALSE(process_);
  process_ = std::move(process);
}

int64_t LdLoadZirconProcessTestsBase::Wait() {
  int64_t result = -1;
  auto wait_for_termination = [process = std::exchange(process_, {}), &result]() {
    ASSERT_TRUE(process) << "Wait() called before Init()?";
    zx_signals_t signals;
    ASSERT_EQ(process.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), &signals), ZX_OK);
    ASSERT_TRUE(signals & ZX_PROCESS_TERMINATED);
    zx_info_process_t info;
    ASSERT_EQ(process.get_info(ZX_INFO_PROCESS, &info, sizeof(info), nullptr, nullptr), ZX_OK);
    ASSERT_TRUE(info.flags & ZX_INFO_PROCESS_FLAG_STARTED);
    ASSERT_TRUE(info.flags & ZX_INFO_PROCESS_FLAG_EXITED);
    result = info.return_code;
  };
  wait_for_termination();

  return result;
}

void LdLoadZirconProcessTestsBase::Start(TestProcessArgs* bootstrap, zx::channel bootstrap_receiver,
                                         std::optional<size_t> stack_size, const zx::thread& thread,
                                         uintptr_t entry, uintptr_t vdso_base,
                                         const zx::vmar& root_vmar) {
  // Allocate the stack.  This is delayed until here in case the test uses
  // bootstrap() methods after Init() that affect bootstrap().GetStackSize().
  zx::vmo stack_vmo;
  uintptr_t sp;
  std::optional<size_t> bootstrap_stack_size = stack_size;
  if (!bootstrap_stack_size) {
    // TODO(mcgrathr): stack use too big for procargs piddly default
    // bootstrap_stack_size = bootstrap.GetStackSize();
    bootstrap_stack_size = 64 << 10;
  }

  const size_t page_size = zx_system_get_page_size();
  const size_t stack_vmo_size = (*bootstrap_stack_size + page_size - 1) & -page_size;
  const size_t stack_vmar_size = stack_vmo_size + page_size;

  ASSERT_EQ(zx::vmo::create(stack_vmo_size, 0, &stack_vmo), ZX_OK);

  zx::vmar stack_vmar;
  uintptr_t stack_vmar_base;
  ASSERT_EQ(root_vmar.allocate(ZX_VM_CAN_MAP_SPECIFIC | ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE, 0,
                               stack_vmar_size, &stack_vmar, &stack_vmar_base),
            ZX_OK);

  zx_vaddr_t stack_base;
  ASSERT_EQ(stack_vmar.map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_SPECIFIC | ZX_VM_ALLOW_FAULTS,
                           page_size, stack_vmo, 0, stack_vmo_size, &stack_base),
            ZX_OK);

  if (bootstrap) {
    ASSERT_NO_FATAL_FAILURE(bootstrap->AddStackVmo(std::move(stack_vmo)));
  }

  sp = elfldltl::AbiTraits<>::InitialStackPointer(stack_base, stack_vmo_size);

  // Pack up the bootstrap message and start the process running.
  if (bootstrap_receiver) {
    ASSERT_FALSE(bootstrap);
  } else if (bootstrap) {
    bootstrap_receiver = bootstrap->PackBootstrap();
  }

  ASSERT_EQ(this->process().start(thread, entry, sp, std::move(bootstrap_receiver), vdso_base),
            ZX_OK);
}

int64_t LdLoadZirconProcessTestsBase::Run(  //
    TestProcessArgs* bootstrap, zx::channel bootstrap_receiver, std::optional<size_t> stack_size,
    const zx::thread& thread, uintptr_t entry, uintptr_t vdso_base, const zx::vmar& root_vmar) {
  Start(bootstrap, std::move(bootstrap_receiver), stack_size, thread, entry, vdso_base, root_vmar);
  return ::testing::Test::HasFatalFailure() ? -1 : Wait();
}

LdLoadZirconProcessTestsBase::~LdLoadZirconProcessTestsBase() {
  if (process_) {
    EXPECT_EQ(process_.kill(), ZX_OK);
  }
}

}  // namespace ld::testing
