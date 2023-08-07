// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-startup-in-process-tests-zircon.h"

#include <dlfcn.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/zx/channel.h>

#include <filesystem>

namespace ld::testing {

void InProcessTestLaunch::Init(std::initializer_list<std::string_view> args) {
  zx_vaddr_t test_base;
  ASSERT_EQ(zx::vmar::root_self()->allocate(
                ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE, 0, kVmarSize,
                &test_vmar_, &test_base),
            ZX_OK);

  ASSERT_NO_FATAL_FAILURE(log_.Init());
  procargs()  //
      .AddInProcessTestHandles()
      .AddDuplicateHandle(PA_VMAR_ROOT, test_vmar_.borrow())
      .AddFd(STDERR_FILENO, log_.TakeSocket())
      .SetArgs(args);
}

void InProcessTestLaunch::SendExecutable(std::string_view name) {
  const std::string path = std::filesystem::path("test") / "bin" / name;
  zx::vmo vmo = elfldltl::testing::GetTestLibVmo(path);
  ASSERT_TRUE(vmo);
  ASSERT_NO_FATAL_FAILURE(procargs().AddHandle(PA_VMO_EXECUTABLE, std::move(vmo)));
}

int InProcessTestLaunch::Call(uintptr_t entry) {
  auto fn = reinterpret_cast<EntryFunction*>(entry);
  zx::channel bootstrap = procargs_.PackBootstrap();
  return fn(bootstrap.release(), GetVdso());
}

InProcessTestLaunch::~InProcessTestLaunch() {
  if (test_vmar_) {
    EXPECT_EQ(test_vmar_.destroy(), ZX_OK);
  }
}

void* InProcessTestLaunch::GetVdso() {
  static void* vdso = [] {
    Dl_info info;
    EXPECT_TRUE(dladdr(reinterpret_cast<void*>(&_zx_process_exit), &info));
    EXPECT_STREQ(info.dli_fname, "<vDSO>");
    return info.dli_fbase;
  }();
  return vdso;
}

}  // namespace ld::testing
