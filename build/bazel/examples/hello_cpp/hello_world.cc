// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <iostream>

#include <re2/re2.h>

#include "src/lib/files/file.h"
#include "src/lib/files/glob.h"
#include "src/lib/fxl/command_line.h"

int main() {
  // Unused, only tests if re2 include is working.
  const re2::RE2 kUnused(".*_unused_re");
  std::cout << "Hello, my dear in-tree Bazel world!\n";
  fxl::CommandLine();
  files::IsFile("/tmp/test");
  files::Glob("mypath");
  return 0;
}
