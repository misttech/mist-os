// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <string>

int main(int argc, char** argv) {
  constexpr auto firmware_name_path = "/sys/class/nanohub/nanohub_comms/firmware_name";
  constexpr auto expected_firmware_name = "test_firmware_name";

  auto fd = open(firmware_name_path, 'r');
  if (!fd) {
    printf("Firmware_name path did not open successfully\n");
    return 1;
  }

  // Max sysfs return size: 4096
  char buffer[4096];

  // Read the main contents of the buffer
  auto bytes_read = read(fd, buffer, 4096);
  if (bytes_read < 0) {
    printf("Error: Failed to read file contents\n");
    return 1;
  }
  if (bytes_read == 0) {
    printf("Error: firmware_name read returned zero bytes\n");
    return 1;
  }

  std::string content(buffer, bytes_read);
  printf("Bytes read: %zd  \nContents: %s\n", bytes_read, content.c_str());

  // Make sure we received the correct string...
  if (content != expected_firmware_name) {
    printf("Error: Received incorrect firmware_name string\n");
    return 1;
  }

  return 0;
}
