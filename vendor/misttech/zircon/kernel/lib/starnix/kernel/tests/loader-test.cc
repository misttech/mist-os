// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/loader.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/mm/memory_accessor.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/util/bstring.h>
#include <lib/unittest/unittest.h>
#include <zircon/assert.h>

#include <fbl/vector.h>
#include <ktl/string_view.h>

namespace unit_testing {

using namespace starnix::testing;
using namespace starnix;

const UserAddress TEST_STACK_ADDR = UserAddress::const_from(0x30000000);

class StackVmo : public starnix::MemoryAccessor {
 public:
  StackVmo(zx::vmo vmo) : vmo_(ktl::move(vmo)) {}

  // impl StackVmo
  uint64_t address_to_offset(UserAddress addr) const {
    return static_cast<uint64_t>(addr - TEST_STACK_ADDR);
  }

  // impl MemoryAccessor
  fit::result<Errno, ktl::span<uint8_t>> read_memory(UserAddress addr,
                                                     ktl::span<uint8_t>& bytes) const final {
    PANIC("not yet implemented");
  }

  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial_until_null_byte(
      UserAddress addr, ktl::span<uint8_t>& bytes) const final {
    PANIC("not yet implemented");
  }

  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial(
      UserAddress addr, ktl::span<uint8_t>& bytes) const final {
    PANIC("not yet implemented");
  }

  fit::result<Errno, size_t> write_memory(UserAddress addr,
                                          const ktl::span<const uint8_t>& bytes) const final {
    auto status = vmo_.write(bytes.data(), address_to_offset(addr), bytes.size());
    if (status != ZX_OK) {
      return fit::error(errno(EFAULT));
    }
    return fit::ok(bytes.size());
  }

  fit::result<Errno, size_t> write_memory_partial(
      UserAddress addr, const ktl::span<const uint8_t>& bytes) const final {
    PANIC("not yet implemented");
  }

  fit::result<Errno, size_t> zero(UserAddress addr, size_t length) const final {
    PANIC("not yet implemented");
  }

 private:
  zx::vmo vmo_;
};

bool test_trivial_initial_stack() {
  BEGIN_TEST;

  zx::vmo vmo;
  auto s = zx::vmo::create(0x4000, 0, &vmo);
  ASSERT_OK(s, "VMO creation should succeed.");

  StackVmo stack_vmo(ktl::move(vmo));
  auto original_stack_start_addr = TEST_STACK_ADDR + 0x1000ul;

  ktl::string_view path("");
  fbl::Vector<BString> argv;
  fbl::Vector<BString> environ;
  fbl::Vector<ktl::pair<uint32_t, uint64_t>> auxv;

  auto stack_start_addr =
      test_populate_initial_stack(stack_vmo, path, argv, environ, auxv, original_stack_start_addr);
  ASSERT_TRUE(stack_start_addr.is_ok(), "Populate initial stack should succeed.");

  auto argc_size = 8u;
  auto argv_terminator_size = 8u;
  auto environ_terminator_size = 8u;
  auto aux_execfn_terminator_size = 8u;
  auto aux_execfn = 16u;
  auto aux_random = 16u;
  auto aux_null = 16u;
  auto random_seed = 16u;

  auto payload_size = argc_size + argv_terminator_size + environ_terminator_size +
                      aux_execfn_terminator_size + aux_execfn + aux_random + aux_null + random_seed;
  payload_size += payload_size % 16;

  ASSERT_EQ(stack_start_addr.value().stack_pointer.ptr(),
            (original_stack_start_addr - payload_size).ptr());

  END_TEST;
}

fit::result<Errno> exec_hello_starnix(starnix::CurrentTask& current_task) {
  fbl::Vector<BString> argv;
  fbl::AllocChecker ac;
  argv.push_back("bin/hello_starnix", &ac);
  ZX_ASSERT(ac.check());

  auto executable =
      current_task.open_file(argv[0], OpenFlags(OpenFlagsEnum::RDONLY)) _EP(executable);
  return current_task.exec(executable.value(), argv[0], argv, fbl::Vector<BString>());
}

bool test_load_hello_starnix() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked_with_bootfs_current_zbi();

  auto errno = exec_hello_starnix(*current_task);
  ASSERT_FALSE(errno.is_error(), "failed to load executable");
  ASSERT_GT((*current_task)->mm()->get_mapping_count(), 0u);

  END_TEST;
}

bool test_snapshot_hello_starnix() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked_with_bootfs_current_zbi();

  auto errno = exec_hello_starnix(*current_task);
  ASSERT_FALSE(errno.is_error(), "failed to load executable");

  auto current2 = create_task(kernel, "another-task");
  auto snapshot_to = (*current_task)->mm()->snapshot_to((*current2)->mm());
  ASSERT_FALSE(snapshot_to.is_error(), "failed to snapshot mm");

  ASSERT_EQ((*current_task)->mm()->get_mapping_count(), (*current2)->mm()->get_mapping_count());

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_loader)
UNITTEST("test trivial initial stack", unit_testing::test_trivial_initial_stack)
UNITTEST("test load hello starnix", unit_testing::test_load_hello_starnix)
UNITTEST("test snapshot hello starnix", unit_testing::test_snapshot_hello_starnix)
UNITTEST_END_TESTCASE(starnix_loader, "starnix_loader", "Tests for Loader")
