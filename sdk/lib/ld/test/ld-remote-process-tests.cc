// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-remote-process-tests.h"

#include <lib/zx/job.h>
#include <zircon/process.h>

namespace ld::testing {

constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;

class LdRemoteProcessTests::MockLoader {
 public:
  MOCK_METHOD(zx::vmo, LoadObject, (std::string));

  void ExpectLoadObject(std::string_view name) {
    zx::vmo vmo;
    const std::string path = std::filesystem::path("test") / "lib" / kLibprefix / name;
    ASSERT_NO_FATAL_FAILURE(vmo = elfldltl::testing::GetTestLibVmo(path));
    EXPECT_CALL(*this, LoadObject(std::string{name})).WillOnce(::testing::Return(std::move(vmo)));
  }

 private:
  ::testing::InSequence sequence_guard_;
};

void LdRemoteProcessTests::SetUp() {
  GTEST_SKIP() << "TODO(fxb/134320): Skip until remote loading is implemented.";
}

LdRemoteProcessTests::LdRemoteProcessTests() = default;

LdRemoteProcessTests::~LdRemoteProcessTests() = default;

void LdRemoteProcessTests::Init(std::initializer_list<std::string_view> args) {
  mock_loader_ = std::make_unique<MockLoader>();

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

  // TODO(fxbug.dev/134320): Load the stub dynamic linker and vdso.
}

void LdRemoteProcessTests::Needed(std::initializer_list<std::string_view> names) {
  for (std::string_view name : names) {
    mock_loader_->ExpectLoadObject(name);
  }
}

void LdRemoteProcessTests::Load(std::string_view executable_name) {
  // TODO(fxbug.dev/134320): implement remote loading.
}

int64_t LdRemoteProcessTests::Run() {
  return LdLoadZirconProcessTestsBase::Run(nullptr, stack_size_, thread_, entry_, vdso_base_,
                                           root_vmar());
}

}  // namespace ld::testing
