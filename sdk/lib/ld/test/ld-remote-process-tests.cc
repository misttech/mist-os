// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-remote-process-tests.h"

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/abi.h>
#include <lib/zx/job.h>
#include <zircon/process.h>

#include <string_view>

namespace ld::testing {

LdRemoteProcessTests::LdRemoteProcessTests() = default;

LdRemoteProcessTests::~LdRemoteProcessTests() = default;

void LdRemoteProcessTests::Init(std::initializer_list<std::string_view> args,
                                std::initializer_list<std::string_view> env) {
  std::string_view name = process_name();
  zx::process process;
  ASSERT_EQ(zx::process::create(*zx::job::default_job(), name.data(),
                                static_cast<uint32_t>(name.size()), 0, &process, &root_vmar_),
            ZX_OK);
  set_process(std::move(process));

  // Initialize a log to pass ExpectLog statements in load-tests.cc.
  fbl::unique_fd log_fd;
  ASSERT_NO_FATAL_FAILURE(InitLog(log_fd));

  ASSERT_EQ(zx::thread::create(this->process(), name.data(), static_cast<uint32_t>(name.size()), 0,
                               &thread_),
            ZX_OK);
}

int64_t LdRemoteProcessTests::Run() {
  return LdLoadZirconProcessTestsBase::Run(nullptr, stack_size_, thread_, entry_, vdso_base_,
                                           root_vmar());
}

}  // namespace ld::testing
