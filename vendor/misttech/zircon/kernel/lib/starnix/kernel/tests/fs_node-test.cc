// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using namespace starnix_uapi;
using namespace starnix;

bool open_device_file() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  // Create a device file that points to the `zero` device (which is automatically
  // registered in the kernel).
  auto result = (*current_task)
                    ->fs()
                    ->root()
                    .create_node(*current_task, "zero", FILE_MODE(IFCHR, 0666), DeviceType::ZERO);
  EXPECT_TRUE(
      result.is_ok());  //, "create_node error [errno=%d]", result.error_value().error_code());

  constexpr size_t CONTENT_LEN = 10;
  auto buffer = VecOutputBuffer::New(CONTENT_LEN);

  // Read from the zero device.
  auto flags = OpenFlags(OpenFlagsEnum::RDONLY);
  auto device_file = (*current_task).open_file("zero", flags);
  EXPECT_TRUE(
      device_file
          .is_ok());  //, "open device file [errno=%d]",device_file.error_value().error_code());

  // device_file.read(&mut locked, &current_task, &mut buffer).expect("read from zero");
  /*
        // Read from the zero device.
        let device_file = current_task
            .open_file(&mut locked, "zero".into(), OpenFlags::RDONLY)
            .expect("open device file");
        device_file.read(&mut locked, &current_task, &mut buffer).expect("read from zero");

        // Assert the contents.
        assert_eq!(&[0; CONTENT_LEN], buffer.data());
  */
  END_TEST;
}

bool node_info_is_reflected_in_stat() {
  BEGIN_TEST;
  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_node)
UNITTEST("test open device file", unit_testing::open_device_file)
// UNITTEST("node_info_is_reflected_in_stats", node_info_is_reflected_in_stat)
UNITTEST_END_TESTCASE(starnix_fs_node, "starnix_fs_node", "Tests for FsNode")
