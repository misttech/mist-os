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

  // Read again, to check for EOF
  printf("Reading again to await EOF\n");

  // Flushing here as a failure case will cause a hang
  fflush(stdout);
  bytes_read = read(fd, buffer, 4096);

  if (bytes_read < 0) {
    printf("Error: Read failed when expecting EOF\n");
    return 1;
  }
  if (bytes_read > 0) {
    // Unexpected buffer contents
    std::string unexpected_content(buffer, bytes_read);
    printf("Error: Bytes read: %zd  \nUnexpected content: %s", bytes_read,
           unexpected_content.c_str());
    return 1;
  }

  // To reach here, bytes_read was 0, correctly marking EOF
  printf("Successfully read EOF from firmware_name path\n");

  // Make sure we received the correct string...
  if (content != expected_firmware_name) {
    printf("Error: Received incorrect firmware_name string\n");
    return 1;
  }

  return 0;
}
