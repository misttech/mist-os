// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdio>

#include "src/lib/files/file.h"

int main(int argc, char** argv) {
  printf("suspend_linux, suspending ...\n");
  bool ok = files::WriteFile("/sys/power/state", "mem");
  if (!ok) {
    printf("suspend_linux, error. Could not open /sys/power/state.\n");
    return 1;
  }
  printf("suspend_linux, resumed, and exiting.\n");
  return 0;
}
