// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <stdlib.h>

#include <filesystem>
#include <string_view>

#ifdef __APPLE__
#include <mach-o/dyld.h>
#endif

#include <lib/elfldltl/testing/get-test-data.h>

namespace elfldltl::testing {

std::filesystem::path GetTestDataPath(std::string_view filename) {
  std::filesystem::path path;
#ifdef __linux__
  char self_path[PATH_MAX];
  path.append(realpath("/proc/self/exe", self_path)).remove_filename();
#elif defined(__Fuchsia__)
  path.append("/pkg/data");
#elif defined(__APPLE__)
  uint32_t length = PATH_MAX;
  char self_path[PATH_MAX];
  char self_path_symlink[PATH_MAX];
  _NSGetExecutablePath(self_path_symlink, &length);
  path.append(realpath(self_path_symlink, self_path)).remove_filename();
#else
#error unknown platform.
#endif
  return path / "test_data/elfldltl" / filename;
}

#ifndef __Fuchsia__
// See get-test-lib.cc for the Fuchsia case; elsewhere this is a normal open.
fbl::unique_fd GetTestLib(std::string_view libname) {
  return fbl::unique_fd(open(GetTestDataPath(libname).c_str(), O_RDONLY));
}
#endif

}  // namespace elfldltl::testing
