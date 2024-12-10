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

using starnix::CheckAccessReason;
using starnix::MountInfo;

#if 0
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

  ASSERT_BYTES_EQ(expected.data(), buffer.data().data(), CONTENT_LEN);

  END_TEST;
}
#endif

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
    info.mode_ = FileMode::IFSOCK;
    info.size_ = 1;
    info.blocks_ = 2;
    info.blksize_ = 4;
    info.uid_ = 9;
    info.gid_ = 10;
    info.link_count_ = 11;
    // info.time_status_change = UtcInstant::from_nanos(1);
    // info.time_access = UtcInstant::from_nanos(2);
    // info.time_modify = UtcInstant::from_nanos(3);
    info.rdev_ = DeviceType::New(13, 13);
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
  // ASSERT_EQ(0u, stat.st_ctime);
  // ASSERT_EQ(1u, stat.st_ctime_nsec);
  // ASSERT_EQ(0u, stat.st_atime);
  // ASSERT_EQ(2u, stat.st_atime_nsec);
  // ASSERT_EQ(0u, stat.st_mtime);
  // ASSERT_EQ(3u, stat.st_mtime_nsec);
  ASSERT_EQ(DeviceType::New(13, 13).bits(), stat.st_rdev);

  END_TEST;
}

bool test_check_access() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto creds = Credentials::with_ids(1, 2);

  fbl::AllocChecker ac;
  fbl::Vector<gid_t> vec;
  vec.push_back(3, &ac);
  ZX_ASSERT(ac.check());
  vec.push_back(4, &ac);
  ZX_ASSERT(ac.check());
  creds.groups_ = ktl::move(vec);
  current_task->set_creds(creds);

  // Create a node
  auto result = (*current_task)
                    ->fs()
                    ->root()
                    .create_node(*current_task, "foo", FileMode::IFREG, DeviceType::NONE);
  ASSERT_TRUE(result.is_ok(), "create_node");
  auto& node = result.value().entry_->node_;

  auto check_access = [&](uid_t uid, gid_t gid, uint32_t perm,
                          Access access) -> fit::result<Errno> {
    node->update_info<void>([&](starnix::FsNodeInfo& info) {
      info.mode_ = FILE_MODE(IFREG, perm);
      info.uid_ = uid;
      info.gid_ = gid;
    });
    return node->check_access(*current_task, MountInfo::detached(), access,
                              CheckAccessReason::InternalPermissionChecks);
  };

  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 0, 0700, Access(AccessEnum::EXEC)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 0, 0700, Access(AccessEnum::READ)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 0, 0700, Access(AccessEnum::WRITE)).error_value().error_code());

  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 0, 0070, Access(AccessEnum::EXEC)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 0, 0070, Access(AccessEnum::READ)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 0, 0070, Access(AccessEnum::WRITE)).error_value().error_code());

  ASSERT_TRUE(check_access(0, 0, 0007, Access(AccessEnum::EXEC)).is_ok());
  ASSERT_TRUE(check_access(0, 0, 0007, Access(AccessEnum::READ)).is_ok());
  ASSERT_TRUE(check_access(0, 0, 0007, Access(AccessEnum::WRITE)).is_ok());

  ASSERT_TRUE(check_access(1, 0, 0700, Access(AccessEnum::EXEC)).is_ok());
  ASSERT_TRUE(check_access(1, 0, 0700, Access(AccessEnum::READ)).is_ok());
  ASSERT_TRUE(check_access(1, 0, 0700, Access(AccessEnum::WRITE)).is_ok());

  ASSERT_TRUE(check_access(1, 0, 0100, Access(AccessEnum::EXEC)).is_ok());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(1, 0, 0100, Access(AccessEnum::READ)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(1, 0, 0100, Access(AccessEnum::WRITE)).error_value().error_code());

  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(1, 0, 0200, Access(AccessEnum::EXEC)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(1, 0, 0200, Access(AccessEnum::READ)).error_value().error_code());
  ASSERT_TRUE(check_access(1, 0, 0200, Access(AccessEnum::WRITE)).is_ok());

  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(1, 0, 0400, Access(AccessEnum::EXEC)).error_value().error_code());
  ASSERT_TRUE(check_access(1, 0, 0400, Access(AccessEnum::READ)).is_ok());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(1, 0, 0400, Access(AccessEnum::WRITE)).error_value().error_code());

  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 2, 0700, Access(AccessEnum::EXEC)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 2, 0700, Access(AccessEnum::READ)).error_value().error_code());
  ASSERT_EQ(errno(EACCES).error_code(),
            check_access(0, 2, 0700, Access(AccessEnum::WRITE)).error_value().error_code());

  ASSERT_TRUE(check_access(0, 2, 0070, Access(AccessEnum::EXEC)).is_ok());
  ASSERT_TRUE(check_access(0, 2, 0070, Access(AccessEnum::READ)).is_ok());
  ASSERT_TRUE(check_access(0, 2, 0070, Access(AccessEnum::WRITE)).is_ok());

  ASSERT_TRUE(check_access(0, 3, 0070, Access(AccessEnum::EXEC)).is_ok());
  ASSERT_TRUE(check_access(0, 3, 0070, Access(AccessEnum::READ)).is_ok());
  ASSERT_TRUE(check_access(0, 3, 0070, Access(AccessEnum::WRITE)).is_ok());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_node)
// UNITTEST("test open device file", unit_testing::open_device_file)
UNITTEST("node info is reflected in stats", unit_testing::node_info_is_reflected_in_stat)
UNITTEST("test check access", unit_testing::test_check_access)
UNITTEST_END_TESTCASE(starnix_fs_node, "starnix_fs_node", "Tests for FsNode")
