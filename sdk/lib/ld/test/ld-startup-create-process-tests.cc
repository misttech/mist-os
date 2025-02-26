// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-startup-create-process-tests.h"

#include <lib/zx/job.h>
#include <zircon/process.h>

#include <gtest/gtest.h>

namespace ld::testing {

void LdStartupCreateProcessTestsBase::Init(std::initializer_list<std::string_view> args,
                                           std::initializer_list<std::string_view> env) {
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
                              .SetArgs(args)
                              .SetEnv(env));
}

void LdStartupCreateProcessTestsBase::FinishLoad(zx::vmo executable_vmo) {
  // Send the executable VMO.
  ASSERT_NO_FATAL_FAILURE(bootstrap().AddExecutableVmo(std::move(executable_vmo)));

  // Prime the mock loader service from the Needed() calls.
  ASSERT_NO_FATAL_FAILURE(LdsvcExpectNeeded());

  // If a mock loader service has been set up by calls to Needed() et al,
  // send the client end over.
  if (zx::channel ldsvc = TakeLdsvc()) {
    ASSERT_NO_FATAL_FAILURE(bootstrap().AddLdsvc(std::move(ldsvc)));
  }
}

LdStartupCreateProcessTestsBase::~LdStartupCreateProcessTestsBase() = default;

int64_t LdStartupCreateProcessTestsBase::Run() {
  return LdLoadZirconProcessTestsBase::Run(&bootstrap(), {}, stack_size_, thread_, entry_,
                                           vdso_base_, root_vmar());
}

}  // namespace ld::testing
