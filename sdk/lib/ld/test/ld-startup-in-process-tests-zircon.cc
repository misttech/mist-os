// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-startup-in-process-tests-zircon.h"

#include <dlfcn.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/ld/abi.h>
#include <lib/zx/channel.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

#include <cstddef>
#include <filesystem>
#include <string>

#include <gtest/gtest.h>

namespace ld::testing {
namespace {

constexpr std::string_view kLibprefix = LD_STARTUP_TEST_LIBPREFIX;

// The dynamic linker gets loaded into this same test process, but it's given
// a sub-VMAR to consider its "root" or allocation range so hopefully it will
// confine its pointer references to that part of the address space.  The
// dynamic linker doesn't necessarily clean up all its mappings--on success,
// it leaves many mappings in place.  Test VMAR is always destroyed when the
// InProcessTestLaunch object goes out of scope.
constexpr size_t kVmarSize = 1 << 30;

void* GetVdso() {
  static void* vdso = [] {
    Dl_info info;
    EXPECT_TRUE(dladdr(reinterpret_cast<void*>(&_zx_process_exit), &info));
    EXPECT_STREQ(info.dli_fname, "<vDSO>");
    return info.dli_fbase;
  }();
  return vdso;
}

}  // namespace

const std::string kLdStartupName =
    std::string("test/lib/") + std::string(kLibprefix) + std::string(ld::abi::kInterp);

void LdStartupInProcessTests::Init(std::initializer_list<std::string_view> args) {
  zx_vaddr_t test_base;
  ASSERT_EQ(zx::vmar::root_self()->allocate(
                ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE, 0, kVmarSize,
                &test_vmar_, &test_base),
            ZX_OK);

  log_ = std::make_unique<elfldltl::testing::TestPipeReader>();
  fbl::unique_fd log_fd;
  ASSERT_NO_FATAL_FAILURE(log_->Init(log_fd));

  procargs_  //
      .AddInProcessTestHandles()
      .AddDuplicateHandle(PA_VMAR_ROOT, test_vmar_.borrow())
      .AddFd(STDERR_FILENO, std::move(log_fd))
      .SetArgs(args);
}

void LdStartupInProcessTests::Load(std::string_view executable_name) {
  ASSERT_TRUE(test_vmar_);  // Init must have been called already.

  std::optional<LoadResult> result;
  ASSERT_NO_FATAL_FAILURE(Load(kLdStartupName, result, test_vmar_));

  entry_ = result->entry + result->loader.load_bias();

  // The ends the useful lifetime of the loader object by extracting the VMAR
  // where it loaded the test image.  This VMAR handle doesn't need to be
  // saved here, since it's a sub-VMAR of the test_vmar_ that will be
  // destroyed when this InProcessTestLaunch object dies.
  zx::vmar load_image_vmar = std::move(result->loader).Commit();

  // Pass along that handle in the bootstrap message.
  ASSERT_NO_FATAL_FAILURE(procargs_.AddHandle(PA_VMAR_LOADED, std::move(load_image_vmar)));

  // Send the executable VMO.
  const std::string executable_path = std::filesystem::path("test") / "bin" / executable_name;
  zx::vmo vmo = elfldltl::testing::GetTestLibVmo(executable_path);
  ASSERT_TRUE(vmo);
  ASSERT_NO_FATAL_FAILURE(procargs_.AddHandle(PA_VMO_EXECUTABLE, std::move(vmo)));
}

int64_t LdStartupInProcessTests::Run() {
  using EntryFunction = int(zx_handle_t, void*);
  auto fn = reinterpret_cast<EntryFunction*>(entry_);
  zx::channel bootstrap = procargs_.PackBootstrap();
  return fn(bootstrap.release(), GetVdso());
}

LdStartupInProcessTests::~LdStartupInProcessTests() {
  // The log should have been collected by CheckLog.
  EXPECT_FALSE(log_);

  if (test_vmar_) {
    EXPECT_EQ(test_vmar_.destroy(), ZX_OK);
  }
}

void LdStartupInProcessTests::ExpectLog(std::string_view expected_log) {
  ASSERT_TRUE(log_);
  std::string log = std::move(*std::exchange(log_, {})).Finish();

  EXPECT_EQ(log, expected_log);
}

}  // namespace ld::testing
