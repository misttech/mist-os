// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zircon-internal/ktrace.h>
#include <lib/zx/resource.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/status.h>

#include <fbl/string.h>
#include <fbl/unique_fd.h>

namespace {

constexpr char kUsage[] =
    "\
Usage: ktrace [options] <control>\n\
Where <control> is one of:\n\
  start <group_mask>  - start tracing\n\
  stop                - stop tracing\n\
  rewind              - rewind trace buffer\n\
  written             - print bytes written to trace buffer\n\
    Note: This value doesn't reset on \"rewind\". Instead, the rewind\n\
    takes effect on the next \"start\".\n\
  save <path>         - save contents of trace buffer to <path>\n\
\n\
Options:\n\
  --help  - Duh.\n\
";

void PrintUsage(FILE* f) { fputs(kUsage, f); }

int DoStart(const zx::resource& debug_resource, uint32_t group_mask) {
  if (zx_status_t status =
          zx_ktrace_control(debug_resource.get(), KTRACE_ACTION_START, group_mask, nullptr);
      status != ZX_OK) {
    fprintf(stderr, "Error starting ktrace: %s(%d)\n", zx_status_get_string(status), status);
    return EXIT_FAILURE;
  }
  return EXIT_SUCCESS;
}

int DoStop(const zx::resource& debug_resource) {
  if (zx_status_t status = zx_ktrace_control(debug_resource.get(), KTRACE_ACTION_STOP, 0, nullptr);
      status != ZX_OK) {
    fprintf(stderr, "Error stopping ktrace: %s(%d)\n", zx_status_get_string(status), status);
    return EXIT_FAILURE;
  }
  return EXIT_SUCCESS;
}

int DoRewind(const zx::resource& debug_resource) {
  if (zx_status_t status =
          zx_ktrace_control(debug_resource.get(), KTRACE_ACTION_REWIND, 0, nullptr);
      status != ZX_OK) {
    fprintf(stderr, "Error rewinding ktrace: %s(%d)\n", zx_status_get_string(status), status);
    return EXIT_FAILURE;
  }
  return EXIT_SUCCESS;
}

int DoWritten(const zx::resource& debug_resource) {
  uint64_t bytes_written;
  if (zx_status_t status = zx_ktrace_read(debug_resource.get(), nullptr, 0, 0, &bytes_written);
      status != ZX_OK) {
    fprintf(stderr, "Error getting bytes written: %s(%d)\n", zx_status_get_string(status), status);
    return EXIT_FAILURE;
  }
  printf("Bytes written: %ld\n", bytes_written);
  return EXIT_SUCCESS;
}

int DoSave(const zx::resource& debug_resource, const char* path) {
  fbl::unique_fd out_fd(open(path, O_CREAT | O_TRUNC | O_WRONLY, 0666));
  if (!out_fd.is_valid()) {
    fprintf(stderr, "Unable to open file for writing: %s, %s\n", path, strerror(errno));
    return EXIT_FAILURE;
  }

  // Read/write this many bytes at a time.
  constexpr size_t read_size = 4096;
  uint8_t buf[read_size];
  uint32_t offset = 0;
  zx_status_t status;
  size_t actual;
  while ((status = zx_ktrace_read(debug_resource.get(), buf, offset, read_size, &actual)) ==
         ZX_OK) {
    if (actual == 0) {
      break;
    }
    offset += actual;
    size_t bytes_written = write(out_fd.get(), buf, actual);
    if (bytes_written < 0) {
      fprintf(stderr, "I/O error saving buffer: %s\n", strerror(errno));
      return EXIT_FAILURE;
    }
    if (bytes_written != actual) {
      fprintf(stderr, "Short write saving buffer: %zd vs %zd\n", bytes_written, actual);
      return EXIT_FAILURE;
    }
  }

  return EXIT_SUCCESS;
}

void EnsureNArgs(const fbl::String& cmd, int argc, int expected_argc) {
  if (argc != expected_argc) {
    fprintf(stderr, "Unexpected number of args for command %s\n", cmd.c_str());
    PrintUsage(stderr);
    exit(EXIT_FAILURE);
  }
}
}  // namespace

int main(int argc, char** argv) {
  if (argc >= 2 && strcmp(argv[1], "--help") == 0) {
    PrintUsage(stdout);
    return EXIT_SUCCESS;
  }

  if (argc < 2) {
    PrintUsage(stderr);
    return EXIT_FAILURE;
  }
  const fbl::String cmd{argv[1]};

  auto debug_client_end = component::Connect<fuchsia_kernel::DebugResource>();
  if (debug_client_end.is_error()) {
    fprintf(stderr, "Error in getting debug resource: %s(%d)\n", debug_client_end.status_string(),
            debug_client_end.status_value());
    return 1;
  }
  auto debug_result = fidl::SyncClient(std::move(*debug_client_end))->Get();
  if (!debug_result.is_ok()) {
    fprintf(stderr, "Error in getting debug resource: %s\n",
            debug_result.error_value().status_string());
    return 1;
  }

  zx::resource debug_resource = std::move(debug_result->resource());

  if (cmd == "start") {
    EnsureNArgs(cmd, argc, 3);
    int group_mask = atoi(argv[2]);
    if (group_mask < 0) {
      fprintf(stderr, "Invalid group mask\n");
      return EXIT_FAILURE;
    }
    return DoStart(debug_resource, group_mask);
  } else if (cmd == "stop") {
    EnsureNArgs(cmd, argc, 2);
    return DoStop(debug_resource);
  } else if (cmd == "rewind") {
    EnsureNArgs(cmd, argc, 2);
    return DoRewind(debug_resource);
  } else if (cmd == "written") {
    EnsureNArgs(cmd, argc, 2);
    return DoWritten(debug_resource);
  } else if (cmd == "save") {
    EnsureNArgs(cmd, argc, 3);
    const char* path = argv[2];
    return DoSave(debug_resource, path);
  }

  PrintUsage(stderr);
  return EXIT_FAILURE;
}
