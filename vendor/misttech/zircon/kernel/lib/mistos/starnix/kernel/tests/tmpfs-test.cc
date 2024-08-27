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

#include <algorithm>
#include <vector>

#include <ktl/span.h>
#include <zxtest/zxtest.h>

namespace starnix {

using namespace starnix_uapi;
using namespace starnix::testing;

TEST(TmpFs, test_tmpfs) {
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  auto fs = TmpFs::new_fs(kernel);
  auto root = fs->root();
  auto usr = root->create_dir(*current_task, "usr").value();
  auto _etc = root->create_dir(*current_task, "etc").value();
  auto _usr_bin = usr->create_dir(*current_task, "bin").value();

  auto names = root->copy_child_names();
  std::sort(names.begin(), names.end());
  ASSERT_EQ(std::vector<FsString>({"etc", "usr"}), names);
}

TEST(TmpFs, test_write_read) {
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  FsStr path("test.bin");
  auto _file = current_task->fs()
                   ->root()
                   .create_node(*current_task, path, FILE_MODE(IFREG, 0777), DeviceType::NONE)
                   .value();

  auto wr_file = (*current_task).open_file(path, OpenFlags(OpenFlagsEnum::RDWR)).value();

  std::vector<uint16_t> test_vec(10000);
  ktl::span<uint16_t> tmp(test_vec.data(), test_vec.size());
  ktl::span<uint8_t> test_bytes(reinterpret_cast<uint8_t*>(tmp.data()), tmp.size_bytes());

  auto buffer = VecInputBuffer::New(test_bytes);
  auto write_result = wr_file->write(*current_task, &buffer);
  ASSERT_TRUE(write_result.is_ok(), "error: %d", write_result.error_value().error_code());

  auto written = write_result.value();
  ASSERT_EQ(test_bytes.size(), written);

  auto read_buffer = VecOutputBuffer::New(test_bytes.size() + 1);
  auto read_result = wr_file->read_at(*current_task, 0, &read_buffer);
  ASSERT_TRUE(read_result.is_ok(), "error: %d", read_result.error_value().error_code());

  auto read = read_result.value();
  ASSERT_EQ(test_bytes.size(), read);

  ASSERT_BYTES_EQ(test_bytes.data(), read_buffer.data(), test_bytes.size());
}

TEST(TmpFs, test_data) {
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto fs = TmpFs::new_fs_with_options(kernel, {
                                                   "",
                                                   MountFlags::empty(),
                                                   "mode=0123,uid=42,gid=84",
                                               });
  EXPECT_TRUE(fs.is_ok(), "new_fs");

  auto info = fs.value()->root()->node->info();
  ASSERT_EQ(FILE_MODE(IFDIR, 0123), info->mode);
  ASSERT_EQ(42, info->uid);
  ASSERT_EQ(84, info->gid);
}

}  // namespace starnix
