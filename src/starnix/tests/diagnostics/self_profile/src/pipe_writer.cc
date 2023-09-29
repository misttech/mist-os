// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <sys/mman.h>
#include <unistd.h>

#include <iostream>

int main(int argc, char** argv) {
  std::cout << "creating pipe...\n";
  int pipefd[2];

  // Use pipe2 explicitly since glibc appears to translate pipe() into it and we want this source
  // to match the test assertions in main.rs.
  if (pipe2(pipefd, 0) != 0) {
    std::cout << "couldn't open pipe: " << strerror(errno) << "\n";
    abort();
  }

  std::string bin_name(argv[0]);
  for (int i = 0; i < 1000; i++) {
    if (write(pipefd[1], bin_name.data(), bin_name.size()) < 0) {
      std::cout << "couldn't write to pipe: " << strerror(errno) << "\n";
      abort();
    }
  }
}
