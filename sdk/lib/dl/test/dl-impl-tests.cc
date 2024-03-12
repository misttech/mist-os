// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-impl-tests.h"

#include <filesystem>

#include "../runtime-dynamic-linker.h"

namespace dl::testing {

// Set the lib prefix used in library paths to the same prefix used in libld,
// which generates the testing modules used by libdl.
constexpr std::string_view kLibprefix = LD_TEST_LIBPREFIX;

fit::result<Error, void*> DlImplTests::DlOpen(const char* name, int mode) {
  std::string path = std::filesystem::path("test") / "lib" / kLibprefix / name;
  return dynamic_linker_.Open(path.c_str(), mode);
}

}  // namespace dl::testing
