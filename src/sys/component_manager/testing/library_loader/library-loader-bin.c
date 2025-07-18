// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/sys/component_manager/testing/library_loader/library-loader-helper.h"

// Simple program that exits with a different exit code depending on which
// helper library variant was loaded (either library-loader-helper.so or
// library-loader-helper-alt.so). It is used to test component manager's library
// injection feature.

int main(int argc, const char **argv) {
  // Just exit with the value provided by the helper library.
  return helper_value;
}
