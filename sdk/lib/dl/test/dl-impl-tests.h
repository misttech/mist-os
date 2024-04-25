// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_IMPL_TESTS_H_
#define LIB_DL_TEST_DL_IMPL_TESTS_H_

#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/mmap-loader.h>

#include <fbl/unique_fd.h>

#include "../diagnostics.h"
#include "../runtime-dynamic-linker.h"

#ifdef __Fuchsia__
#include <lib/elfldltl/vmar-loader.h>
#include <lib/elfldltl/vmo.h>

#include "dl-load-zircon-tests-base.h"
#endif

#include "dl-load-posix-tests-base.h"

namespace dl::testing {

#ifdef __Fuchsia__
class TestFuchsia {
 public:
  using File = elfldltl::VmoFile<Diagnostics>;
  using Loader = elfldltl::LocalVmarLoader;

  static std::optional<File> RetrieveFile(Diagnostics& diag, std::string_view filename);
};
#endif

class TestPosix {
 public:
  using File = elfldltl::UniqueFdFile<Diagnostics>;
  using Loader = elfldltl::MmapLoader;

  static std::optional<File> RetrieveFile(Diagnostics& diag, std::string_view filename);
};

#ifdef __Fuchsia__
using DlImplLoadTestsBase = DlLoadZirconTestsBase;
#else
using DlImplLoadTestsBase = DlLoadPosixTestsBase;
#endif

template <class TestOS>
class DlImplTests : public DlImplLoadTestsBase {
 public:
  // Error messages in tests can be matched exactly with this test fixture,
  // since the error message returned from the libdl implementation will be the
  // same regardless of the OS.
  static constexpr bool kCanMatchExactError = true;

  fit::result<Error, void*> DlOpen(const char* file, int mode) {
    // fit::result<...> must be initialized to something, but this will get
    // overwritten by the return value from dynamic_linker_.Open.
    fit::result<Error, void*> result = fit::error{Error{"dlopen result is not set"}};
    CallWithLdsvcInstalled([&]() { result = dynamic_linker_.Open<TestOS>(file, mode); });
    return result;
  }

  fit::result<Error, void*> DlSym(void* module, const char* ref) {
    return dynamic_linker_.LookupSymbol(static_cast<ModuleHandle*>(module), ref);
  }

 private:
  RuntimeDynamicLinker dynamic_linker_;
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_IMPL_TESTS_H_
