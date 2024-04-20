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

// TODO(https://fxbug.dev/323419430): For now, both versions of dl-impl-tests
// will use DlLoadPosixTestsBase, which is just a class of empty hooks so that
// dl-load-tests.cc can compile. Eventually, DlImplTests may use another base
// class to verify that module/dep files were loaded as expected.
using DlImplLoadTestsBase = DlLoadPosixTestsBase;

template <class TestOS>
class DlImplTests : public DlImplLoadTestsBase {
 public:
  // Error messages in tests can be matched exactly with this test fixture,
  // since the error message returned from the libdl implementation will be the
  // same regardless of the OS.
  static constexpr bool kCanMatchExactError = true;

  fit::result<Error, void*> DlOpen(const char* file, int mode) {
    return dynamic_linker_.Open<TestOS>(file, mode);
  }

  fit::result<Error, void*> DlSym(void* module, const char* ref) {
    return dynamic_linker_.LookupSymbol(static_cast<ModuleHandle*>(module), ref);
  }

 private:
  RuntimeDynamicLinker dynamic_linker_;
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_IMPL_TESTS_H_
