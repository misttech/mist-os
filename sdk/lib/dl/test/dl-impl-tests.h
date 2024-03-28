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
#include "dl-tests-base.h"

#ifdef __Fuchsia__
#include <lib/elfldltl/vmar-loader.h>
#include <lib/elfldltl/vmo.h>
#endif

namespace dl::testing {

#ifdef __Fuchsia__
class TestFuchsia {
 public:
  using File = elfldltl::VmoFile<Diagnostics>;
  using Loader = elfldltl::LocalVmarLoader;
  using RelroCapability = zx::vmar;

  static RelroCapability Commit(Loader loader) { return std::move(loader).Commit(); }

  static std::optional<File> RetrieveFile(Diagnostics& diag, std::string_view filename);
};
#endif

class TestPosix {
 public:
  using File = elfldltl::UniqueFdFile<Diagnostics>;
  using Loader = elfldltl::MmapLoader;
  // The MmapLoader does not return anything after successfully committing to
  // memory, and `mprotect` does not need any information from the Loader to
  // apply relro protections. Hence the RelroCapability is an empty struct that
  // will become a no-op to the function that performs relro protections.
  struct RelroCapability {};

  static RelroCapability Commit(Loader loader) {
    std::move(loader).Commit();
    return {};
  }

  static std::optional<File> RetrieveFile(Diagnostics& diag, std::string_view filename);
};

template <class TestOS>
class DlImplTests : public DlTestsBase {
 public:
  // Error messages in tests can be matched exactly with this test fixture,
  // since the error message returned from the libdl implementation will be the
  // same regardless of the OS.
  static constexpr bool kCanMatchExactError = true;

  fit::result<Error, void*> DlOpen(const char* file, int mode) {
    return dynamic_linker_.Open<TestOS>(file, mode);
  }

 private:
  RuntimeDynamicLinker dynamic_linker_;
};

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_IMPL_TESTS_H_
