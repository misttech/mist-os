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

constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;
constexpr auto kLinkerName = ld::abi::Abi<>::kSoname;

class LdRemoteProcessTests::MockLoader {
 public:
  MOCK_METHOD(zx::vmo, LoadObject, (std::string));

  void ExpectLoadObject(std::string_view name) {
    const std::string path = std::filesystem::path("test") / "lib" / kLibprefix / name;
    EXPECT_CALL(*this, LoadObject(std::string{name}))
        .WillOnce(::testing::Return(elfldltl::testing::GetTestLibVmo(path)));
  }

  // This is used in LoadAndFail tests when a specific request is expected and
  // the test scenario requires that it be refused.  Without such an
  // expectation, the test would succeed if no request were ever made, and
  // would fail when the expected request was made.
  void ExpectLoadObjectFail(std::string_view name) {
    EXPECT_CALL(*this, LoadObject(std::string{name})).WillOnce(::testing::Return(zx::vmo{}));
  }

 private:
  ::testing::InSequence sequence_guard_;
};

LdRemoteProcessTests::LdRemoteProcessTests() = default;

LdRemoteProcessTests::~LdRemoteProcessTests() = default;

void LdRemoteProcessTests::Init(std::initializer_list<std::string_view> args,
                                std::initializer_list<std::string_view> env) {
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
}

zx::vmo LdRemoteProcessTests::GetDepVmo(const RemoteModule::Soname& soname) {
  return mock_loader_->LoadObject(std::string{soname.str()});
}

// Set the expectations that these dependencies will be loaded in the given order.
void LdRemoteProcessTests::Needed(std::initializer_list<std::string_view> names) {
  for (std::string_view name : names) {
    // The linker and vdso should not be included in any `Needed` list for a
    // test, because load requests for them bypass the loader.
    ASSERT_TRUE(name != kLinkerName.str() && name != GetVdsoSoname().str())
        << std::string{name} + " should not be included in Needed list.";
    mock_loader_->ExpectLoadObject(name);
  }
}

void LdRemoteProcessTests::Needed(
    std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs) {
  for (auto [name, found] : name_found_pairs) {
    ASSERT_TRUE(name != kLinkerName.str() && name != GetVdsoSoname().str())
        << std::string{name} + " should not be included in Needed list.";
    if (found) {
      mock_loader_->ExpectLoadObject(name);
    } else {
      mock_loader_->ExpectLoadObjectFail(name);
    }
  }
}

int64_t LdRemoteProcessTests::Run() {
  return LdLoadZirconProcessTestsBase::Run(nullptr, stack_size_, thread_, entry_, vdso_base_,
                                           root_vmar());
}

}  // namespace ld::testing
