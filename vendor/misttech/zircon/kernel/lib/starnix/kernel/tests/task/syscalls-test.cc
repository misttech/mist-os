// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

#include <fbl/ref_ptr.h>
#include <ktl/string_view.h>

#include <linux/prctl.h>

using namespace starnix::testing;

namespace unit_testing {

bool test_prctl_set_vma_anon_name() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto mapped_address = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  auto name_addr = mapped_address + 128u;

  ASSERT_TRUE((*current_task).write_memory(name_addr, {(uint8_t*)"test-name\0", 10}).is_ok(),
              "failed to write name");
  ASSERT_TRUE(starnix_syscalls::SUCCESS == sys_prctl(*current_task, PR_SET_VMA,
                                                     PR_SET_VMA_ANON_NAME, mapped_address.ptr(), 32,
                                                     name_addr.ptr()));

  auto mapping_name_result = (*current_task)->mm()->get_mapping_name(mapped_address + 24u);
  ASSERT_TRUE(mapping_name_result.is_ok(), "failed to get address");
  ASSERT_TRUE(starnix::FsString("test-name") == mapping_name_result.value());

  auto munmap_result = sys_munmap(*current_task, mapped_address, PAGE_SIZE);
  ASSERT_TRUE(munmap_result.is_ok(), "failed to unmap memory");

  mapping_name_result = (*current_task)->mm()->get_mapping_name(mapped_address + 24u);
  ASSERT_TRUE(mapping_name_result.is_error());
  ASSERT_TRUE(errno(EFAULT) == mapping_name_result.error_value());

  END_TEST;
}

bool test_set_vma_name_special_chars() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto name_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);

  auto mapping_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);

  for (int c = 1; c <= 255; c++) {
    BString vma_name(1, static_cast<char>(c));
    ASSERT_TRUE((*current_task).write_memory(name_addr, {(uint8_t*)vma_name.data(), 2}).is_ok());

    auto result = sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                            PAGE_SIZE, name_addr.ptr());

    if (c > 0x1f && c < 0x7f && c != '\\' && c != '`' && c != '$' && c != '[' && c != ']') {
      ASSERT_TRUE(result.is_ok());
      ASSERT_EQ(starnix_syscalls::SUCCESS.value(), result->value());
    } else {
      ASSERT_EQ(errno(EINVAL).error_code(), result.error_value().error_code());
    }
  }

  END_TEST;
}

bool test_set_vma_name_long() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto name_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);

  auto mapping_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);

  BString name_too_long(256, 'a');
  ASSERT_TRUE(
      (*current_task).write_memory(name_addr, {(uint8_t*)name_too_long.data(), 257}).is_ok());

  auto result = sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                          PAGE_SIZE, name_addr.ptr());
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(errno(EINVAL).error_code(), result.error_value().error_code());

  BString name_just_long_enough(255, 'a');

  ASSERT_TRUE((*current_task)
                  .write_memory(name_addr, {(uint8_t*)name_just_long_enough.data(), 256})
                  .is_ok());

  ASSERT_EQ(starnix_syscalls::SUCCESS.value(),
            sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                      PAGE_SIZE, name_addr.ptr())
                ->value());

  END_TEST;
}

bool test_set_vma_name_misaligned() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto name_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  auto mapping_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);

  BString name("name");
  ASSERT_TRUE((*current_task).write_memory(name_addr, {(uint8_t*)name.data(), 5}).is_ok());

  // Passing a misaligned pointer to the start of the named region fails.
  auto result = sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, 1 + mapping_addr.ptr(),
                          PAGE_SIZE - 1, name_addr.ptr());
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(errno(EINVAL).error_code(), result.error_value().error_code());

  // Passing an unaligned length does work, however.
  ASSERT_EQ(starnix_syscalls::SUCCESS.value(),
            sys_prctl(*current_task, PR_SET_VMA, PR_SET_VMA_ANON_NAME, mapping_addr.ptr(),
                      PAGE_SIZE - 1, name_addr.ptr())
                ->value());

  END_TEST;
}

bool test_sys_getsid() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_and_task();

  auto result = starnix::sys_getsid(*current_task, 0);
  ASSERT_FALSE(result.is_error(), "failed to get sid");

  ASSERT_EQ((*current_task)->get_tid(), result.value());

  auto second_current = create_task(kernel, "second task");

  ASSERT_EQ((*second_current)->get_tid(),
            starnix::sys_getsid(*current_task, (*second_current)->get_tid()).value());
  END_TEST;
}

bool test_read_c_string_vector() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();

  auto arg_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ktl::string_view arg = "test-arg\0"sv;
  ASSERT_TRUE(current_task
                  ->write_memory(arg_addr,
                                 ktl::span<const uint8_t>{
                                     reinterpret_cast<const uint8_t*>(arg.data()), arg.size()})
                  .is_ok(),
              "failed to write test arg");

  auto arg_usercstr = UserCString::New(arg_addr);
  auto null_usercstr = mtl::DefaultConstruct<UserCString>();

  auto argv_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ASSERT_TRUE(
      current_task->write_object(UserRef<UserCString>::From(argv_addr), arg_usercstr).is_ok(),
      "failed to write UserCString");
  ASSERT_TRUE(
      current_task
          ->write_object(UserRef<UserCString>::From(argv_addr + sizeof(UserCString)), null_usercstr)
          .is_ok(),
      "failed to write UserCString");
  auto argv_userref = UserRef<UserCString>::New(argv_addr);

  // The arguments size limit should include the null terminator.
  auto result = read_c_string_vector(*current_task, argv_userref, 100, arg.size());
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(arg.size(), result->second);
  ASSERT_STREQ("test-arg", result->first[0]);

  ASSERT_EQ(errno(E2BIG).error_code(),
            read_c_string_vector(*current_task, argv_userref, 100, strlen(arg.data()))
                .error_value()
                .error_code());

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_task_syscalls)
UNITTEST("test prctl set vma anon name", unit_testing::test_prctl_set_vma_anon_name)
UNITTEST("test set vma name special chars", unit_testing::test_set_vma_name_special_chars)
UNITTEST("test set vma name long", unit_testing::test_set_vma_name_long)
UNITTEST("test set vma name misaligned", unit_testing::test_set_vma_name_misaligned)
UNITTEST("test sys getsid", unit_testing::test_sys_getsid)
UNITTEST("test read c string vector", unit_testing::test_read_c_string_vector)
UNITTEST_END_TESTCASE(starnix_task_syscalls, "starnix_task_syscalls", "Tests for Tasks Syscalls")
