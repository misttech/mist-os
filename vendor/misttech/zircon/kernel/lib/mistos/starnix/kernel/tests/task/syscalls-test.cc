// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>

#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <zxtest/zxtest.h>

#include <linux/prctl.h>

using namespace starnix::testing;

namespace {

TEST(Task, test_prctl_set_vma_anon_name) {
  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto mapped_address = map_memory(*current_task, UserAddress(), PAGE_SIZE);
  auto name_addr = mapped_address + 128u;

  ASSERT_TRUE((*current_task).write_memory(name_addr, {(uint8_t*)"test-name\0", 10}).is_ok(),
              "failed to write name");
  ASSERT_EQ(starnix_syscalls::SUCCESS, sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME,
                                                 mapped_address.ptr(), 32, name_addr.ptr()));

  ASSERT_EQ(fbl::String("test-name"),
            (*current_task)->mm()->get_mapping_name(mapped_address + 24u));

  auto munmap_result = sys_munmap(*current_task, mapped_address, PAGE_SIZE);
  ASSERT_TRUE(munmap_result.is_ok(), "failed to unmap memory [error = %d]",
              munmap_result.error_value().error_code());

  auto mapping_name_result = current_task->mm()->get_mapping_name(mapped_address + 24u);
  ASSERT_TRUE(mapping_name_result.is_error());
  ASSERT_EQ(errno(EFAULT), mapping_name_result.error_value());
}

TEST(Task, test_set_vma_name_special_chars) {
  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto name_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);

  auto mapping_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);

  for (int c = 1; c <= 255; c++) {
    fbl::String vma_name(1, static_cast<char>(c));
    ASSERT_TRUE((*current_task).write_memory(name_addr, {(uint8_t*)vma_name.data(), 2}).is_ok());

    auto result = sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                            PAGE_SIZE, name_addr.ptr());

    if (c > 0x1f && c < 0x7f && c != '\\' && c != '`' && c != '$' && c != '[' && c != ']') {
      ASSERT_EQ(starnix_syscalls::SUCCESS, result);
    } else {
      ASSERT_TRUE(result.is_error(), "c[0x%x]", c);
      ASSERT_EQ(errno(EINVAL), result.error_value());
    }
  }
}

TEST(Task, test_set_vma_name_long) {
  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto name_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);

  auto mapping_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);

  fbl::String name_too_long(256, 'a');
  ASSERT_TRUE(
      (*current_task).write_memory(name_addr, {(uint8_t*)name_too_long.data(), 257}).is_ok());

  auto result = sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                          PAGE_SIZE, name_addr.ptr());
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(errno(EINVAL), result.error_value());

  fbl::String name_just_long_enough(255, 'a');

  ASSERT_TRUE((*current_task)
                  .write_memory(name_addr, {(uint8_t*)name_just_long_enough.data(), 256})
                  .is_ok());

  ASSERT_EQ(starnix_syscalls::SUCCESS, sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME,
                                                 mapping_addr.ptr(), PAGE_SIZE, name_addr.ptr()));
}

TEST(Task, test_set_vma_name_misaligned) {
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto name_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);
  auto mapping_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);

  fbl::String name("name");
  ASSERT_TRUE((*current_task).write_memory(name_addr, {(uint8_t*)name.data(), 5}).is_ok());

  // Passing a misaligned pointer to the start of the named region fails.
  auto result = sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, 1 + mapping_addr.ptr(),
                          PAGE_SIZE - 1, name_addr.ptr());
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(errno(EINVAL), result.error_value());

  // Passing an unaligned length does work, however.
  ASSERT_EQ(starnix_syscalls::SUCCESS,
            sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                      PAGE_SIZE - 1, name_addr.ptr()));
}

TEST(Task, test_sys_getsid) {
  auto [kernel, current_task] = create_kernel_and_task();

  auto result = starnix::sys_getsid(*current_task, 0);
  ASSERT_FALSE(result.is_error(), "failed to get sid");

  ASSERT_EQ(current_task->get_tid(), result.value());

  auto second_current = create_task(kernel, "second task");

  ASSERT_EQ(second_current->get_tid(),
            starnix::sys_getsid(*current_task, second_current->get_tid()));
}

}  // namespace
