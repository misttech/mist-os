// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <ftw.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include <cerrno>
#include <fstream>
#include <iostream>

#include <gtest/gtest.h>
#include <linux/loop.h>

#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#ifndef LOOP_CONFIGURE
struct loop_config {
  uint32_t fd;
  uint32_t block_size;
  struct loop_info64 info;
  uint64_t __reserved[8];
};

#define LOOP_CONFIGURE 0x4C0A
#endif  // LOOP_CONFIGURE

namespace {

bool skip_loop_tests = false;

class LoopTest : public ::testing::Test {
 public:
  static void SetUpTestSuite() {
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    int fd = open("/dev/loop-control", O_RDWR, 0777);
    if (fd == -1 && (errno == EACCES || errno == ENOENT)) {
      // GTest does not support GTEST_SKIP() from a suite setup, so record that we want to skip
      // every test here and skip in SetUp().
      skip_loop_tests = true;
      return;
    }
    ASSERT_TRUE(fd >= 0) << "open(\"/dev/loop-control\") failed: " << strerror(errno) << "("
                         << errno << ")";
  }

  void SetUp() override {
    if (skip_loop_tests) {
      GTEST_SKIP() << "Permission denied for /dev/loop-control, skipping suite.";
    }
    loop_control_ = fbl::unique_fd(open("/dev/loop-control", O_RDWR));
    ASSERT_TRUE(loop_control_.is_valid());
  }

  int GetFreeLoopDeviceNumber() {
    int free_loop_device_num(ioctl(loop_control_.get(), LOOP_CTL_GET_FREE, nullptr));
    EXPECT_TRUE(free_loop_device_num >= 0);
    return free_loop_device_num;
  }

  int RemoveLoopDevice(int loop_device_num) {
    int removed_loop_device_num(ioctl(loop_control_.get(), LOOP_CTL_REMOVE, loop_device_num));
    EXPECT_TRUE(removed_loop_device_num >= 0);
    return removed_loop_device_num;
  }

 private:
  fbl::unique_fd loop_control_;
};

#define ASSERT_SUCCESS(call) ASSERT_THAT((call), SyscallSucceeds())

TEST_F(LoopTest, ReopeningDevicePreservesOffset) {
  std::string loop_device_path = "/dev/loop" + std::to_string(GetFreeLoopDeviceNumber());
  fbl::unique_fd free_loop_device(open(loop_device_path.c_str(), O_RDONLY, 0644));
  ASSERT_TRUE(free_loop_device.is_valid());

  fbl::unique_fd backing_file(open("data/hello_world.txt", O_RDONLY, 0644));
  ASSERT_TRUE(backing_file.is_valid());

  // Configure an offset that we'll check for after re-opening the device.
  loop_config config = {.fd = static_cast<__u32>(backing_file.get()),
                        .block_size = 4096,
                        .info = {.lo_offset = 4096}};
  ASSERT_SUCCESS(ioctl(free_loop_device.get(), LOOP_CONFIGURE, &config));

  loop_info64 first_observed_info;
  ASSERT_SUCCESS(ioctl(free_loop_device.get(), LOOP_GET_STATUS64, &first_observed_info));

  // Close the loop device fd and reopen it, confirming that the offset and other configuration are
  // the same and preserved even when there are no open files to the device.
  free_loop_device = fbl::unique_fd(open(loop_device_path.c_str(), O_RDONLY, 0644));
  loop_info64 second_observed_info;
  ASSERT_SUCCESS(ioctl(free_loop_device.get(), LOOP_GET_STATUS64, &second_observed_info));
  EXPECT_EQ(first_observed_info.lo_offset, second_observed_info.lo_offset);
}

TEST_F(LoopTest, RemoveLoopDeviceFromKernelDeviceRegistry) {
  int free_loop_device_num = GetFreeLoopDeviceNumber();
  int removed_loop_device_num = RemoveLoopDevice(free_loop_device_num);
  EXPECT_EQ(removed_loop_device_num, free_loop_device_num);
  std::string devfs_path = "/dev/";
  DIR* dir = opendir(devfs_path.c_str());
  ASSERT_TRUE(dir);
  while (struct dirent* entry = readdir(dir)) {
    std::string name = entry->d_name;
    if (name.find("loop") != std::string::npos) {
      std::string device_path = "/dev/" + name;
      int device_fd = open(device_path.c_str(), O_RDONLY);
      ASSERT_GT(device_fd, 0) << "device path is: " << device_path << strerror(errno);
    }
  }
  closedir(dir);
}

}  // namespace
