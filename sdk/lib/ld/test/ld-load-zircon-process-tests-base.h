// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_LOAD_ZIRCON_PROCESS_TESTS_BASE_H_
#define LIB_LD_TEST_LD_LOAD_ZIRCON_PROCESS_TESTS_BASE_H_

#include <lib/ld/testing/test-processargs.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>

#include <string_view>

#include "ld-load-zircon-ldsvc-tests-base.h"

namespace ld::testing {

// This is the common base class for test fixtures to launch a Zircon process.
class LdLoadZirconProcessTestsBase : public LdLoadZirconLdsvcTestsBase {
 public:
  // The Fuchsia test executables (via modules/zircon-test-start.cc) link
  // directly to the vDSO, so it appears before other modules.
  static constexpr std::string_view kTestExecutableNeedsVdso = "libzircon.so";

  static constexpr int64_t kRunFailureForTrap = ZX_TASK_RETCODE_EXCEPTION_KILL;
  static constexpr int64_t kRunFailureForBadPointer = ZX_TASK_RETCODE_EXCEPTION_KILL;

  ~LdLoadZirconProcessTestsBase();

  const char* process_name() const;

 protected:
  static zx::vmo GetExecutableVmo(std::string_view executable_name);

  const zx::process& process() const { return process_; }

  void set_process(zx::process process);

  int64_t Run(TestProcessArgs* bootstrap, std::optional<size_t> stack_size,
              const zx::thread& thread, uintptr_t entry, uintptr_t vdso_base,
              const zx::vmar& root_vmar);

  // Wait for the process to die and collect its exit code.
  int64_t Wait();

 private:
  zx::process process_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_LOAD_ZIRCON_PROCESS_TESTS_BASE_H_
