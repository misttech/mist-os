// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-remote-process-tests.h"

#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/ld/abi.h>
#include <lib/ld/remote-load-module.h>
#include <lib/ld/testing/test-vmo.h>
#include <lib/zx/job.h>
#include <zircon/process.h>

#include <string_view>

namespace ld::testing {

constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;
constexpr auto kVdsoName = elfldltl::Soname<>{"libzircon.so"};
constexpr auto kLinkerName = ld::abi::Abi<>::kSoname;
constexpr size_t kDefaultStackSize = 4096;

zx::vmo LdRemoteProcessTests::GetTestVmo(std::string_view path) {
  zx::vmo vmo;
  auto get_vmo = [&]() { ASSERT_NO_FATAL_FAILURE(vmo = elfldltl::testing::GetTestLibVmo(path)); };
  get_vmo();
  return vmo;
}

class LdRemoteProcessTests::MockLoader {
 public:
  MOCK_METHOD(zx::vmo, LoadObject, (std::string));

  void ExpectLoadObject(std::string_view name) {
    const std::string path = std::filesystem::path("test") / "lib" / kLibprefix / name;
    EXPECT_CALL(*this, LoadObject(std::string{name})).WillOnce(::testing::Return(GetTestVmo(path)));
  }

 private:
  ::testing::InSequence sequence_guard_;
};

void LdRemoteProcessTests::SetUp() {
  GTEST_SKIP() << "TODO(fxb/134320): Skip until relocation and loader is implemented.";
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
}

// Set the expectations that these dependencies will be loaded in the given order.
void LdRemoteProcessTests::Needed(std::initializer_list<std::string_view> names) {
  for (std::string_view name : names) {
    // The linker and vdso should not be included in any `Needed` list for a
    // test, because load requests for them bypass the loader.
    ASSERT_TRUE(name != kLinkerName.str() && name != kVdsoName.c_str())
        << std::string{name} + " should not be included in Needed list.";
    mock_loader_->ExpectLoadObject(name);
  }
}

void LdRemoteProcessTests::Load(std::string_view executable_name) {
  using RemoteModule = RemoteLoadModule<>;

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  const std::string executable_path =
      std::filesystem::path("test") / "bin" / kLibprefix / executable_name;
  zx::vmo vmo;
  ASSERT_NO_FATAL_FAILURE(vmo = elfldltl::testing::GetTestLibVmo(executable_path));

  auto exec = std::make_unique<RemoteModule>(abi::Abi<>::kExecutableName);
  auto exec_info = exec->Decode(diag, std::move(vmo));
  ASSERT_TRUE(exec_info);
  set_stack_size(exec_info->stack_size);

  auto get_dep_vmo = [this](const elfldltl::Soname<>& soname) -> zx::vmo {
    // Executables may depend on the stub linker and vdso implicitly, so fetch
    // these VMO directly without going through the mock loader.
    if (soname == kVdsoName) {
      zx::vmo vmo;
      GetVdsoVmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
      return vmo;
    }
    if (soname == kLinkerName) {
      return GetTestVmo("ld-stub.so");
    }
    return mock_loader_->LoadObject(std::string{soname.str()});
  };

  auto modules = RemoteModule::LinkModules(diag, std::move(exec), exec_info->needed, get_dep_vmo);
  EXPECT_TRUE(!modules.is_empty());
}

int64_t LdRemoteProcessTests::Run() {
  // Since the LdRemoteProcessTests fixture does not yet implement a bootstrap
  // channel from which to calculate a stack size, make sure some value gets
  // passed in for allocating the stack.
  auto stack_size = stack_size_ ? *stack_size_ : kDefaultStackSize;
  return LdLoadZirconProcessTestsBase::Run(nullptr, stack_size, thread_, entry_, vdso_base_,
                                           root_vmar());
}

}  // namespace ld::testing
