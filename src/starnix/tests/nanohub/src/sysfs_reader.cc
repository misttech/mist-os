// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <string>

bool check_sysfs_file(const char* path, const char* expected_value) {
  auto fd = open(path, 'r');
  if (!fd) {
    printf("Path did not open successfully: %s\n", path);
    return false;
  }

  // Max sysfs return size: 4096
  char buffer[4096];

  // Read the main contents of the buffer
  auto bytes_read = read(fd, buffer, 4096);
  if (bytes_read < 0) {
    printf("Error: Failed to read file contents from %s\n", path);
    return false;
  }
  if (bytes_read == 0) {
    printf("Error: read returned zero bytes from %s\n", path);
    return false;
  }

  std::string content(buffer, bytes_read);
  printf("Bytes read: %zd  \nContents: %s\n", bytes_read, content.c_str());

  // Read again, to check for EOF
  printf("Reading again to await EOF\n");

  // Flushing here as a failure case will cause a hang
  fflush(stdout);
  bytes_read = read(fd, buffer, 4096);

  if (bytes_read < 0) {
    printf("Error: Read failed when expecting EOF from %s\n", path);
    return false;
  }
  if (bytes_read > 0) {
    // Unexpected buffer contents
    std::string unexpected_content(buffer, bytes_read);
    printf("Error: Bytes read: %zd  \nUnexpected content: %s from %s", bytes_read,
           unexpected_content.c_str(), path);
    return false;
  }

  // To reach here, bytes_read was 0, correctly marking EOF
  printf("Successfully read EOF from %s\n", path);

  // Make sure we received the correct string...
  if (content != expected_value) {
    printf("Error: Received incorrect string from %s\n", path);
    return false;
  }

  return true;
}

int main(int argc, char** argv) {
  if (!check_sysfs_file("/sys/class/nanohub/nanohub_comms/firmware_name", "test_firmware_name")) {
    return 1;
  }
  if (!check_sysfs_file("/sys/class/display/display_comms/display_state", "4\n")) {
    return 1;
  }
  if (!check_sysfs_file("/sys/class/display/display_comms/display_info",
                        "display_mode: 4\npanel_mode: 1\nnbm_brightness: 2\naod_brightness: 3\n")) {
    return 1;
  }
  if (!check_sysfs_file("/sys/class/display/display_comms/display_select", "0\n")) {
    return 1;
  }

  return 0;
}
