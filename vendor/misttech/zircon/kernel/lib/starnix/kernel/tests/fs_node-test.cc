// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/device/mem.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

namespace {

bool open_device_file() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  starnix::mem_device_init(*current_task);

  // Create a device file that points to the `zero` device (which is automatically
  // registered in the kernel).
  auto result = (*current_task)
                    ->fs()
                    ->root()
                    .create_node(*current_task, "zero", FILE_MODE(IFCHR, 0666), DeviceType::ZERO);
  ASSERT_TRUE(result.is_ok(), "create_node");

  constexpr size_t CONTENT_LEN = 10;
  auto buffer = starnix::VecOutputBuffer::New(CONTENT_LEN);

  // Read from the zero device.
  auto device_file = (*current_task).open_file("zero", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(device_file.is_ok(), "open device file");

  auto read_result = device_file->read(*current_task, &buffer);
  ASSERT_TRUE(read_result.is_ok(), "read from zero");

  // Assert the contents
  ktl::array<uint8_t, CONTENT_LEN> expected = {0};

  ASSERT_BYTES_EQ(expected.data(), buffer.data(), CONTENT_LEN);

  END_TEST;
}

bool node_info_is_reflected_in_stat() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();

  // Create a node
  auto result = (*current_task)
                    ->fs()
                    ->root()
                    .create_node(*current_task, "zero", FileMode::IFCHR, DeviceType::ZERO);
  ASSERT_TRUE(result.is_ok(), "create_node");
  auto& node = result.value().entry_->node_;
  node->update_info<void>([](starnix::FsNodeInfo& info) {
    info.mode = FileMode::IFSOCK;
    info.size = 1;
    info.blocks = 2;
    info.blksize = 4;
    info.uid = 9;
    info.gid = 10;
    info.link_count = 11;
    // info.time_status_change = UtcInstant::from_nanos(1);
    // info.time_access = UtcInstant::from_nanos(2);
    // info.time_modify = UtcInstant::from_nanos(3);
    info.rdev = DeviceType::New(13, 13);
  });

  auto stat_result = node->stat(*current_task);
  ASSERT_TRUE(stat_result.is_ok(), "stat");
  auto& stat = stat_result.value();

  ASSERT_EQ(FileMode::IFSOCK.bits(), stat.st_mode);
  ASSERT_EQ(1, stat.st_size);
  ASSERT_EQ(4, stat.st_blksize);
  ASSERT_EQ(2, stat.st_blocks);
  ASSERT_EQ(9u, stat.st_uid);
  ASSERT_EQ(10u, stat.st_gid);
  ASSERT_EQ(11u, stat.st_nlink);
  ASSERT_EQ(0u, stat.st_ctime);
  ASSERT_EQ(1u, stat.st_ctime_nsec);
  ASSERT_EQ(0u, stat.st_atime);
  ASSERT_EQ(2u, stat.st_atime_nsec);
  ASSERT_EQ(0u, stat.st_mtime);
  ASSERT_EQ(3u, stat.st_mtime_nsec);
  ASSERT_EQ(DeviceType::New(13, 13).bits(), stat.st_rdev);

  END_TEST;
}

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_node)
UNITTEST("test open device file", unit_testing::open_device_file)
UNITTEST("node info is reflected in stats", unit_testing::node_info_is_reflected_in_stat)
UNITTEST_END_TESTCASE(starnix_fs_node, "starnix_fs_node", "Tests for FsNode")
