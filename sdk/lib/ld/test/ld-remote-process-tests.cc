// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-remote-process-tests.h"

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/abi.h>
#include <lib/ld/remote-abi-stub.h>
#include <lib/zx/job.h>
#include <zircon/process.h>

#include <string_view>

namespace ld::testing {

LdRemoteProcessTests::LdRemoteProcessTests() = default;

void LdRemoteProcessTests::SetUp() {
  ASSERT_NO_FATAL_FAILURE(stub_ld_vmo_ =
                              elfldltl::testing::GetTestLibVmo(RemoteAbiStub<>::kFilename));
}

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

void LdRemoteProcessTests::MakeBootstrapChannel(zx::channel& bootstrap_receiver) {
  // Create the bootstrap channel and keep the sender's end for test logic that
  // uses it to communicate with the test process.
  ASSERT_FALSE(bootstrap_sender_);
  zx_status_t status = zx::channel::create(0, &bootstrap_sender_, &bootstrap_receiver);
  ASSERT_EQ(status, ZX_OK) << "zx_channel_create: " << zx_status_get_string(status);
}

void LdRemoteProcessTests::Start() {
  zx::channel bootstrap_receiver;
  ASSERT_NO_FATAL_FAILURE(MakeBootstrapChannel(bootstrap_receiver));
  LdLoadZirconProcessTestsBase::Start(nullptr, std::move(bootstrap_receiver), stack_size_, thread_,
                                      entry_, vdso_base_, root_vmar());
}

int64_t LdRemoteProcessTests::Run() {
  zx::channel bootstrap_receiver;
  MakeBootstrapChannel(bootstrap_receiver);
  return bootstrap_receiver ? LdLoadZirconProcessTestsBase::Run(
                                  nullptr, std::move(bootstrap_receiver), stack_size_, thread_,
                                  entry_, vdso_base_, root_vmar())
                            : -1;
}

}  // namespace ld::testing
