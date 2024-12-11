// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/unittest/unittest.h>

#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/span.h>

namespace unit_testing {

using namespace starnix;
using namespace starnix_uapi;
using namespace starnix::testing;

bool test_tmpfs() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  auto fs = TmpFs::new_fs(kernel);
  auto root = fs->root();
  auto usr = root->create_dir(*current_task, "usr").value();
  auto _etc = root->create_dir(*current_task, "etc").value();
  auto _usr_bin = usr->create_dir(*current_task, "bin").value();

  auto names = root->copy_child_names();
  ktl::sort(names.begin(), names.end());

  ASSERT_TRUE(FsString("etc") == names[0]);
  ASSERT_TRUE(FsString("usr") == names[1]);

  END_TEST;
}

bool test_write_read() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  FsStr path("test.bin");
  auto file = (*current_task)
                  ->fs()
                  ->root()
                  .create_node(*current_task, path, FILE_MODE(IFREG, 0777), DeviceType::NONE)
                  .value();

  auto wr_file = (*current_task).open_file(path, OpenFlags(OpenFlagsEnum::RDWR)).value();

  fbl::AllocChecker ac;
  fbl::Vector<uint16_t> test_vec;
  test_vec.reserve(10000, &ac);
  ASSERT(ac.check());

  ktl::span<uint16_t> tmp(test_vec.data(), test_vec.capacity());
  ktl::span<uint8_t> test_bytes(reinterpret_cast<uint8_t*>(tmp.data()), tmp.size_bytes());

  auto buffer = VecInputBuffer::New(test_bytes);
  auto write_result = wr_file->write(*current_task, &buffer);
  ASSERT_TRUE(write_result.is_ok());

  auto written = write_result.value();
  ASSERT_EQ(test_bytes.size(), written);

  auto read_buffer = VecOutputBuffer::New(test_bytes.size() + 1);
  auto read_result = wr_file->read_at(*current_task, 0, &read_buffer);
  ASSERT_TRUE(read_result.is_ok());

  auto read = read_result.value();
  ASSERT_EQ(test_bytes.size(), read);

  ASSERT_BYTES_EQ(test_bytes.data(), read_buffer.data().data(), test_bytes.size());

  END_TEST;
}

bool test_permissions() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();

  FsStr path("test.bin");
  auto file = (*current_task)
                  .open_file_at(FdNumber::AT_FDCWD_, path,
                                OpenFlags(OpenFlagsEnum::CREAT) | OpenFlags(OpenFlagsEnum::RDONLY),
                                FileMode::from_bits(0777), ResolveFlags::empty());

  ASSERT_TRUE(file.is_ok(), "failed to create file");

  END_TEST;
}

bool test_persistence() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();

  {
    auto root = (*current_task)->fs()->root().entry_;
    auto usr = root->create_dir(*current_task, "usr").value();
    auto _etc = root->create_dir(*current_task, "etc").value();
    auto _usr_bin = usr->create_dir(*current_task, "bin").value();
  }

  // At this point, all the nodes are dropped.

  auto _file = (*current_task)
                   .open_file("/usr/bin", OpenFlags(OpenFlagsEnum::RDONLY) |
                                              OpenFlags(OpenFlagsEnum::DIRECTORY));
  ASSERT_TRUE(_file.is_ok(), "failed to open /usr/bin");
  ASSERT_EQ(errno(ENOENT).error_code(),
            (*current_task)
                .open_file("/usr/bin/test.txt", OpenFlags(OpenFlagsEnum::RDWR))
                .error_value()
                .error_code());
  auto _txt = (*current_task)
                  .open_file_at(FdNumber::AT_FDCWD_, "/usr/bin/test.txt",
                                OpenFlags(OpenFlagsEnum::RDWR) | OpenFlags(OpenFlagsEnum::CREAT),
                                FileMode::from_bits(0777), ResolveFlags::empty());
  auto txt = (*current_task).open_file("/usr/bin/test.txt", OpenFlags(OpenFlagsEnum::RDWR));
  ASSERT_TRUE(txt.is_ok(), "failed to open test.txt");

  auto usr_bin = (*current_task).open_file("/usr/bin", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(usr_bin.is_ok(), "failed to open /usr/bin");

  // TODO (Herrera) add missing method unlink
  // usr_bin->name->unlink();

  END_TEST;
}

bool test_data() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto fs = TmpFs::new_fs_with_options(
      kernel, {
                  .source = "",
                  .flags = MountFlags::empty(),
                  .params = MountParams::parse("mode=0123,uid=42,gid=84").value(),
              });
  EXPECT_TRUE(fs.is_ok(), "new_fs");

  auto info = fs.value()->root()->node_->info();
  ASSERT_TRUE(FILE_MODE(IFDIR, 0123) == info->mode_);
  ASSERT_TRUE(42 == info->uid_);
  ASSERT_TRUE(84 == info->gid_);

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_tmpfs)
UNITTEST("test tmpfs", unit_testing::test_tmpfs)
UNITTEST("test write read", unit_testing::test_write_read)
UNITTEST("test permissions", unit_testing::test_permissions)
// UNITTEST("test persistence", unit_testing::test_persistence)
UNITTEST("test data", unit_testing::test_data)
UNITTEST_END_TESTCASE(starnix_fs_tmpfs, "starnix_fs_tmpfs", "Tests for starnix tempfs")
