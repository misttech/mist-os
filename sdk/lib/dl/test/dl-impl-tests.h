// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_IMPL_TESTS_H_
#define LIB_DL_TEST_DL_IMPL_TESTS_H_

#include <lib/fit/defer.h>

#include "../runtime-dynamic-linker.h"
#include "dl-load-tests-base.h"

#ifdef __Fuchsia__
#include "dl-load-zircon-tests-base.h"
#endif

namespace dl::testing {

extern const ld::abi::Abi<>& gStartupLdAbi;

// The Base class provides testing facilities and logic specific to the platform
// the test is running on. DlImplTests invokes Base methods when functions
// need to operate differently depending on the OS.
template <class Base>
class DlImplTests : public Base {
 public:
  // Error messages in tests can be matched exactly with this test fixture,
  // since the error message returned from the libdl implementation will be the
  // same regardless of the OS.
  static constexpr bool kCanMatchExactError = true;
  // TODO(https://fxbug.dev/342480690): Support Dynamic TLS
  static constexpr bool kSupportsDynamicTls = false;
  // TODO(https://fxbug.dev/382529434): Have dlclose() run finalizers
  static constexpr bool kDlCloseCanRunFinalizers = false;

  void SetUp() override {
    Base::SetUp();

    fbl::AllocChecker ac;
    dynamic_linker_ = RuntimeDynamicLinker::Create(gStartupLdAbi, ac);
    ASSERT_TRUE(ac.check());
  }

  fit::result<Error, void*> DlOpen(const char* file, int mode) {
    // Check that all Needed/Expect* expectations for loaded objects were
    // satisfied and then clear the expectation set.
    auto verify_expectations = fit::defer([&]() { Base::VerifyAndClearNeeded(); });
    auto result = dynamic_linker_->Open<typename Base::Loader>(
        file, mode, std::bind_front(&Base::RetrieveFile, this));
    if (result.is_ok()) {
      // If RTLD_NOLOAD was passed and we have a NULL return value, there is no
      // module to track.
      if ((mode & RTLD_NOLOAD) && !result.value()) {
        return result;
      }
      // TODO(https://fxbug.dev/382527519): RuntimeDynamicLinker should have a
      // `RunInitializers` method that will run this with proper synchronization.
      static_cast<RuntimeModule*>(result.value())->InitializeModuleTree();
      Base::TrackModule(result.value(), std::string{file});
    }
    return result;
  }

  // TODO(https://fxbug.dev/342028933): Implement dlclose.
  fit::result<Error> DlClose(void* module) {
    auto untrack_file = fit::defer([&]() { Base::UntrackModule(module); });
    // At minimum check that a valid handle was passed and present in the
    // dynamic linker's list of modules.
    for (auto& m : dynamic_linker_->modules()) {
      if (&m == module) {
        return fit::ok();
      }
    }
    return fit::error<Error>{"Invalid library handle %p", module};
  }

  fit::result<Error, void*> DlSym(void* module, const char* ref) {
    const RuntimeModule* root = static_cast<RuntimeModule*>(module);
    return dynamic_linker_->LookupSymbol(*root, ref);
  }

  // The `dynamic_linker_-> dtor will also destroy and unmap modules remaining in
  // its modules list, so there is no need to do any extra clean up operation.
  void CleanUpOpenedFile(void* ptr) override {}

 private:
  std::unique_ptr<RuntimeDynamicLinker> dynamic_linker_;
};

using DlImplLoadPosixTests = DlImplTests<DlLoadTestsBase>;
#ifdef __Fuchsia__
using DlImplLoadZirconTests = DlImplTests<DlLoadZirconTestsBase>;
#endif

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_IMPL_TESTS_H_
