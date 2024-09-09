// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/tests/test_pkg.h"

namespace test_utils {

TestPkg::TestPkg(fidl::ServerEnd<fuchsia_io::Directory> server,
                 std::string_view module_test_pkg_path, std::string_view module_open_path,
                 const std::vector<std::string_view> expected_libs)
    : module_(module_test_pkg_path) {
  EXPECT_EQ(ZX_OK, loop_.StartThread());

  // Construct the test files for the expected libs.
  for (auto name : expected_libs) {
    auto path = std::string(kLibPathPrefix).append(name);
    libname_to_file_.emplace(std::piecewise_construct, std::forward_as_tuple(name),
                             std::forward_as_tuple(path.c_str()));
  }

  lib_dir_.SetOpenHandler([this](fuchsia::io::OpenFlags flags, std::string path, auto object) {
    EXPECT_EQ(fuchsia::io::OpenFlags::RIGHT_READABLE | fuchsia::io::OpenFlags::RIGHT_EXECUTABLE,
              flags);
    auto it = libname_to_file_.find(path);
    EXPECT_NE(it, libname_to_file_.end());
    lib_file_bindings_.push_back(std::make_unique<fidl::Binding<fuchsia::io::File>>(
        &(it->second), object.TakeChannel(), loop_.dispatcher()));
  });

  pkg_binding_.Bind(server.TakeChannel(), loop_.dispatcher());
  pkg_dir_.SetOpenHandler([this, module_open_path = std::string(module_open_path)](
                              fuchsia::io::OpenFlags flags, std::string path, auto object) {
    if (strcmp(path.c_str(), "lib") == 0) {
      EXPECT_EQ(fuchsia::io::OpenFlags::DIRECTORY | fuchsia::io::OpenFlags::RIGHT_READABLE |
                    fuchsia::io::OpenFlags::RIGHT_EXECUTABLE,
                flags);
      lib_dir_binding_.Bind(object.TakeChannel(), loop_.dispatcher());

    } else if (strcmp(path.c_str(), module_open_path.c_str()) == 0) {
      EXPECT_EQ(fuchsia::io::OpenFlags::RIGHT_READABLE | fuchsia::io::OpenFlags::RIGHT_EXECUTABLE,
                flags);
      module_binding_.Bind(object.TakeChannel(), loop_.dispatcher());
    }
  });
}

}  // namespace test_utils
