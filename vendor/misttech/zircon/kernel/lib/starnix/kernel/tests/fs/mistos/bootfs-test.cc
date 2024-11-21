// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/fs/mistos/bootfs.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

#include <object/handle.h>

namespace unit_testing {
namespace {

using starnix::VecOutputBuffer;

bool test_bootfs() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked_with_bootfs();

  {
    auto file = (*current_task).open_file("A.txt", OpenFlags(OpenFlagsEnum::RDONLY));
    ASSERT_TRUE(file.is_ok(), "failed to open_file");

    auto read_buffer = VecOutputBuffer::New(256);
    auto read_result = file->read(*current_task, &read_buffer);
    ASSERT_TRUE(read_result.is_ok());

    const char* content =
        "Four score and seven years ago our fathers brought forth on this "
        "continent, a new nation, conceived in Liberty, and dedicated to the "
        "proposition that all men are created equal.";
    auto data_span = read_buffer.data();
    ASSERT_EQ(strlen(content), read_result.value());
    ASSERT_BYTES_EQ((const uint8_t*)content, data_span.data(), read_result.value());
  }

  {
    auto file = (*current_task).open_file("/nested/B.txt", OpenFlags(OpenFlagsEnum::RDONLY));
    ASSERT_TRUE(file.is_ok(), "failed to open_file");

    auto read_buffer = VecOutputBuffer::New(256);
    auto read_result = file->read(*current_task, &read_buffer);
    ASSERT_TRUE(read_result.is_ok());

    const char* content =
        "Now we are engaged in a great civil war, testing whether that nation, "
        "or any nation so conceived and so dedicated, can long endure.";
    auto data_span = read_buffer.data();
    ASSERT_EQ(strlen(content), read_result.value());
    ASSERT_BYTES_EQ((const uint8_t*)content, data_span.data(), read_result.value());
  }

  {
    auto file = (*current_task).open_file("/nested/again/C.txt", OpenFlags(OpenFlagsEnum::RDONLY));
    ASSERT_TRUE(file.is_ok(), "failed to open_file");

    auto read_buffer = VecOutputBuffer::New(128);
    auto read_result = file->read(*current_task, &read_buffer);
    ASSERT_TRUE(read_result.is_ok());

    const char* content = "We are met on a great battle-field of that war.";
    auto data_span = read_buffer.data();
    ASSERT_EQ(strlen(content), read_result.value());
    ASSERT_BYTES_EQ((const uint8_t*)content, data_span.data(), read_result.value());
  }

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_bootfs)
UNITTEST("test bootfs", unit_testing::test_bootfs)
UNITTEST_END_TESTCASE(starnix_fs_bootfs, "starnix_fs_bootfs", "Tests for starnix bootfs")
