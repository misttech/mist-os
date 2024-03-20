// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-impl-tests.h"

#include <fcntl.h>
#include <lib/elfldltl/testing/get-test-data.h>

#include <filesystem>

namespace dl::testing {

#ifdef __Fuchsia__

fit::result<Error, TestFuchsia::File> TestFuchsia::RetrieveFile(std::string_view filename) {
  // Use the lib prefix for library paths to the same prefix used in libld,
  // which generates the testing modules used by libdl.
  std::filesystem::path path = std::filesystem::path("test") / "lib" / LD_TEST_LIBPREFIX / filename;
  if (auto vmo = elfldltl::testing::TryGetTestLibVmo(path.c_str())) {
    return fit::ok(std::move(vmo));
  }
  return fit::error{Error{"cannot open %.*s", static_cast<int>(filename.size()), filename.data()}};
}

#endif

fit::result<Error, TestPosix::File> TestPosix::RetrieveFile(std::string_view filename) {
  std::filesystem::path path = elfldltl::testing::GetTestDataPath(filename);
  if (fbl::unique_fd fd{open(path.c_str(), O_RDONLY)}) {
    return fit::ok(std::move(fd));
  }
  return fit::error{Error{"cannot open %.*s", static_cast<int>(filename.size()), filename.data()}};
}

}  // namespace dl::testing
