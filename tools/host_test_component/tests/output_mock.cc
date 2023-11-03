// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>

#include <cstdio>

int main(int argc, char* argv[]) {
  printf("Line 1\n");
  fprintf(stderr, "Error Line 1\n");
  printf("Line 2\n");
  printf("Line 3\n");
  printf("Line 4\n");
  fprintf(stderr, "Error Line 2\n");
  fprintf(stderr, "Error Line 3\n");
  fprintf(stderr, "Error Line 4\n");
  printf("Line 5\n");
  fprintf(stderr, "Error Line 5\n");
  fflush(stdout);
  fflush(stderr);
  return 0;
}
