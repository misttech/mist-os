// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

constexpr int kOutputFd = 100;

int main(void) {
  FILE* fp = fdopen(kOutputFd, "w");

  uid_t ruid, euid, suid;
  if (getresuid(&ruid, &euid, &suid) == -1) {
    perror("getresuid");
    exit(EXIT_FAILURE);
  }
  fprintf(fp, "ruid: %u euid: %d suid: %d\n", ruid, euid, suid);

  gid_t rgid, egid, sgid;
  if (getresgid(&rgid, &egid, &sgid) == -1) {
    perror("getresgid");
    exit(EXIT_FAILURE);
  }
  fprintf(fp, "rgid: %u egid: %d sgid: %d\n", rgid, egid, sgid);

  fclose(fp);
  return 0;
}
