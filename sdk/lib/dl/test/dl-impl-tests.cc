// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-impl-tests.h"

#include <fcntl.h>
#include <lib/elfldltl/testing/get-test-data.h>

#include <filesystem>

namespace dl::testing {

#ifdef __Fuchsia__

std::optional<TestFuchsia::File> TestFuchsia::RetrieveFile(Diagnostics& diag,
                                                           std::string_view filename) {
  // Use the lib prefix for library paths to the same prefix used in libld,
  // which generates the testing modules used by libdl.
  std::filesystem::path path = std::filesystem::path("test") / "lib" / LD_TEST_LIBPREFIX / filename;
  if (auto vmo = elfldltl::testing::TryGetTestLibVmo(path.c_str())) {
    return File{std::move(vmo), diag};
  }
  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

#endif

std::optional<TestPosix::File> TestPosix::RetrieveFile(Diagnostics& diag,
                                                       std::string_view filename) {
  std::filesystem::path path = elfldltl::testing::GetTestDataPath(filename);
  if (fbl::unique_fd fd{open(path.c_str(), O_RDONLY)}) {
    return File{std::move(fd), diag};
  }
  diag.SystemError("cannot open ", filename);
  return std::nullopt;
}

}  // namespace dl::testing
