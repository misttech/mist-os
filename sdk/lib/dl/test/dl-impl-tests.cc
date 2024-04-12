// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-impl-tests.h"

#include <fcntl.h>
#include <lib/elfldltl/testing/get-test-data.h>
#ifdef __Fuchsia__
#include <lib/ld/testing/test-vmo.h>
#endif

#include <filesystem>

#include <gtest/gtest.h>

namespace dl::testing {

namespace {

// These startup modules should not be retrieved from the filesystem.
void FileCheck(std::string_view filename) {
  EXPECT_NE(filename, ld::abi::Abi<>::kSoname.str());
#ifdef __Fuchsia__
  EXPECT_NE(filename, ld::testing::GetVdsoSoname().str());
#endif
}

}  // namespace

#ifdef __Fuchsia__

std::optional<TestFuchsia::File> TestFuchsia::RetrieveFile(Diagnostics& diag,
                                                           std::string_view filename) {
  FileCheck(filename);
  auto prefix = "";
  // TODO(https://fxbug.dev/323419430): dlopen shouldn't know if it's loading a
  // loadable_module or shared library. Shared libraries reside in a
  // lib/$libprefix directory on instrumented builds; this is a temporary hack
  // to amend the filepath of a shared library (but not a loadable module) for
  // an instrumented build so dlopen can locate the file.
  // Eventually, the mock loader will be primed with the module/shlib files
  // and this function will only pass `filename` to `TryGetTestLibVmo()` to
  // retrieve the file from the mock loader.
  if (std::string{filename}.find("module") == std::string::npos) {
    prefix = LD_TEST_LIBPREFIX;
  }
  std::filesystem::path path = std::filesystem::path("test") / "lib" / prefix / filename;
  if (auto vmo = elfldltl::testing::TryGetTestLibVmo(path.c_str())) {
    return File{std::move(vmo), diag};
  }
  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

#endif

std::optional<TestPosix::File> TestPosix::RetrieveFile(Diagnostics& diag,
                                                       std::string_view filename) {
  FileCheck(filename);
  std::filesystem::path path = elfldltl::testing::GetTestDataPath(filename);
  if (fbl::unique_fd fd{open(path.c_str(), O_RDONLY)}) {
    return File{std::move(fd), diag};
  }
  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

}  // namespace dl::testing
