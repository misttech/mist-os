// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <iostream>

int main(int argc, char* argv[]) {
  if (argc != 2) {
    std::cerr << "Usage: " << argv[0] << " <exit_code>\n";
    return 1;
  }

  std::string exitCodeStr = argv[1];
  int exitCode = std::stoi(exitCodeStr);
  return exitCode;
}
