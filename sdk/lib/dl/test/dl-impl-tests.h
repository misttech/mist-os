// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_IMPL_TESTS_H_
#define LIB_DL_TEST_DL_IMPL_TESTS_H_

#include <lib/stdcompat/functional.h>

#include "../runtime-dynamic-linker.h"
#include "dl-load-tests-base.h"

#ifdef __Fuchsia__
#include "dl-load-zircon-tests-base.h"
#endif

namespace dl::testing {

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

  fit::result<Error, void*> DlOpen(const char* file, int mode) {
    // Check that all Needed/Expect* expectations for loaded objects were
    // satisfied and then clear the expectation set.
    auto verify_expectations = fit::defer([&]() { Base::VerifyAndClearNeeded(); });
    return dynamic_linker_.Open<typename Base::Loader>(
        file, mode, cpp20::bind_front(&Base::RetrieveFile, this));
  }

  fit::result<Error, void*> DlSym(void* module, const char* ref) {
    return dynamic_linker_.LookupSymbol(static_cast<ModuleHandle*>(module), ref);
  }

 private:
  RuntimeDynamicLinker dynamic_linker_;
};

using DlImplLoadPosixTests = DlImplTests<DlLoadTestsBase>;
#ifdef __Fuchsia__
using DlImplLoadZirconTests = DlImplTests<DlLoadZirconTestsBase>;
#endif

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_IMPL_TESTS_H_
