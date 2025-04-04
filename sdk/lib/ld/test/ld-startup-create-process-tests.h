// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_STARTUP_CREATE_PROCESS_TESTS_H_
#define LIB_LD_TEST_LD_STARTUP_CREATE_PROCESS_TESTS_H_

#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/testing/loader.h>
#include <lib/ld/abi.h>
#include <lib/ld/testing/test-processargs.h>
#include <lib/ld/testing/test-vmo.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include <cstdint>
#include <initializer_list>
#include <optional>
#include <string_view>

#include <gtest/gtest.h>

#include "ld-load-zircon-process-tests-base.h"

namespace ld::testing {

// This is the common base class for template instantiations.  It handles the
// process mechanics, while the templated subclass does the loading.
class LdStartupCreateProcessTestsBase : public LdLoadZirconProcessTestsBase {
 public:
  void Init(std::initializer_list<std::string_view> args = {},
            std::initializer_list<std::string_view> env = {});

  int64_t Run();

  ~LdStartupCreateProcessTestsBase();

  TestProcessArgs& bootstrap() { return procargs_; }

  zx::channel& bootstrap_sender() { return procargs_.bootstrap_sender(); }

 protected:
  const zx::vmar& root_vmar() { return root_vmar_; }

  void set_entry(uintptr_t entry) { entry_ = entry; }

  void set_vdso_base(uintptr_t vdso_base) { vdso_base_ = vdso_base; }

  void set_stack_size(std::optional<size_t> stack_size) { stack_size_ = stack_size; }

  void FinishLoad(zx::vmo executable_vmo);

 private:
  uintptr_t entry_ = 0;
  uintptr_t vdso_base_ = 0;
  std::optional<size_t> stack_size_;
  TestProcessArgs procargs_;
  zx::vmar root_vmar_;
  zx::thread thread_;
};

template <class Elf = elfldltl::Elf<>>
class LdStartupCreateProcessTests
    : public elfldltl::testing::LoadTests<elfldltl::testing::RemoteVmarLoaderTraits, Elf>,
      public LdStartupCreateProcessTestsBase {
 public:
  using LdStartupCreateProcessTestsBase::Run;

  void Load(std::string_view executable_name,
            std::optional<std::string_view> expected_config = std::nullopt) {
    ASSERT_TRUE(root_vmar());  // Init must have been called already.

    // This points GetLibVmo() to the right place.
    LdsvcPathPrefix(executable_name);

    // This will adjust GetLibVmo() to use the libprefix from PT_INTERP so the
    // fetch of abi::kInterp will get the right install location.
    zx::vmo executable_vmo = GetExecutableVmoWithInterpConfig(executable_name, expected_config);
    ASSERT_TRUE(executable_vmo);

    // Load the dynamic linker and record its entry point.
    std::optional<LoadResult> result;
    ASSERT_NO_FATAL_FAILURE(
        this->Load(GetInterp(executable_name, expected_config), result, root_vmar()));
    set_entry(result->entry + result->loader.load_bias());
    set_stack_size(result->stack_size);

    // The bootstrap message gets the VMAR where the dynamic linker resides.
    zx::vmar self_vmar = std::move(result->loader).Commit(kNoRelro).TakeVmar();
    ASSERT_NO_FATAL_FAILURE(bootstrap().AddSelfVmar(std::move(self_vmar)));

    // Load the vDSO and record its base address.
    // No handles to the VMAR where it was loaded survive.
    ASSERT_NO_FATAL_FAILURE(this->Load(*GetVdsoVmo(), result, root_vmar()));
    set_vdso_base(result->info.vaddr_start() + result->loader.load_bias());
    std::ignore = std::move(result->loader).Commit(kNoRelro);

    ASSERT_NO_FATAL_FAILURE(FinishLoad(std::move(executable_vmo)));
  }

  template <class... Reports>
  void LoadAndFail(std::string_view name, Reports&&... reports) {
    ASSERT_NO_FATAL_FAILURE(StartupLoadAndFail(*this, name, std::forward<Reports>(reports)...));
  }

 private:
  using Base = elfldltl::testing::LoadTests<elfldltl::testing::RemoteVmarLoaderTraits, Elf>;
  using Base::kNoRelro;
  using Base::Load;
  using typename Base::LoadResult;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_STARTUP_CREATE_PROCESS_TESTS_H_
