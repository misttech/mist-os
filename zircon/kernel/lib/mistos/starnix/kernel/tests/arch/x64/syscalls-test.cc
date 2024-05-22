// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/open_flags.h>

#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <zxtest/zxtest.h>

using namespace starnix::testing;
using namespace starnix;

namespace {

TEST(Arch, test_sys_creat) {
  auto [kernel, current_task] = create_kernel_and_task();
  auto path_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);
  fbl::String path("newfile.txt");
  auto result = (*current_task).write_memory(path_addr, {(uint8_t*)path.data(), path.size()});
  ASSERT_TRUE(result.is_ok());
  auto fd_or_error = sys_creat(*current_task, path_addr, starnix_uapi::FileMode());
  ASSERT_TRUE(fd_or_error.is_ok());
  auto file_handle = (*current_task).open_file(path, OpenFlags(OpenFlagsEnum::RDONLY));

  auto flag_or_error = current_task->files.get_fd_flags(fd_or_error.value());
  ASSERT_TRUE(flag_or_error.is_ok());
}

}  // namespace
