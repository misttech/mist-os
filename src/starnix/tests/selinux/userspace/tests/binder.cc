// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/fit/defer.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/stat.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/tests/binder_helper.h"
#include "src/starnix/tests/selinux/userspace/util.h"
#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

extern std::string DoPrePolicyLoadWork() { return "binder.pp"; }

namespace {

constexpr int kServiceManagerHandle = 0;
const size_t kBinderMMapSize = sysconf(_SC_PAGESIZE);

// Opens the binder
fbl::unique_fd OpenBinder(std::string_view dir) {
  return fbl::unique_fd(open((std::string(dir) + "/binder").c_str(), O_RDWR | O_CLOEXEC));
}

// Makes the current process register itself as a binder context manager,
// and reads in an infinite loop the messages.
void ContextManagerLoop(std::string_view dir) {
  fbl::unique_fd binder = OpenBinder(dir);
  EXPECT_TRUE(binder) << strerror(errno);

  auto mapping = test_helper::ScopedMMap::MMap(nullptr, kBinderMMapSize, PROT_READ, MAP_PRIVATE,
                                               binder.get(), 0);
  ASSERT_TRUE(mapping.is_ok()) << mapping.error_value();

  // Register itself as the context manager
  EXPECT_THAT(ioctl(binder.get(), BINDER_SET_CONTEXT_MGR, 0), SyscallSucceeds());

  // Enter looper
  {
    binder_helper::EnterLooperPayload enter_looper_payload;
    struct binder_write_read payload = {};
    payload.write_buffer = (binder_uintptr_t)&enter_looper_payload;
    payload.write_size = sizeof(enter_looper_payload);
    payload.write_consumed = 0;
    EXPECT_THAT(ioctl(binder.get(), BINDER_WRITE_READ, &payload), SyscallSucceeds());
  }

  while (true) {
    struct binder_write_read payload = {};
    std::array<int32_t, 32> read_buffer = {};
    payload.read_size = sizeof(read_buffer);
    payload.read_buffer = (binder_uintptr_t)read_buffer.data();
    payload.read_consumed = 0;
    EXPECT_THAT(ioctl(binder.get(), BINDER_WRITE_READ, &payload), SyscallSucceeds());
  }
}

// Starts a context manager process.
// Returns a value that on destruction kills the process.
auto ScopedContextManagerProcess(std::string_view dir) {
  std::unique_ptr<test_helper::ForkHelper> helper = std::make_unique<test_helper::ForkHelper>();
  pid_t pid = RunInForkedProcessWithLabel(*helper, "test_u:test_r:binder_context_manager_test_t:s0",
                                          [dir] { ContextManagerLoop(dir); });
  auto cleanup = fit::defer([pid, helper = std::move(helper)]() {
    ASSERT_THAT(kill(pid, SIGKILL), SyscallSucceeds());
    helper->ExpectSignal(SIGKILL);
    ASSERT_TRUE(helper->WaitForChildren());
  });
  return cleanup;
}

class BinderTest : public ::testing::Test {
 protected:
  void SetUp() override {
    EXPECT_THAT(mount(nullptr, temp_dir_.path().c_str(), "binder", 0, nullptr), SyscallSucceeds());
  }

  void TearDown() override { EXPECT_THAT(umount(temp_dir_.path().c_str()), SyscallSucceeds()); }

  test_helper::ScopedTempDir temp_dir_;
};

// Test opening binder from the default domain.
TEST_F(BinderTest, OpenBinderNoTestDomain) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  fbl::unique_fd binder = OpenBinder(temp_dir_.path());
  EXPECT_TRUE(binder) << strerror(errno);
}

// Test opening binder from the test domain.
TEST_F(BinderTest, OpenBinderWithTestDomain) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:binder_open_test_t:s0", [&] {
    fbl::unique_fd binder = OpenBinder(temp_dir_.path());
    EXPECT_TRUE(binder) << strerror(errno);
  }));
}

class ContextManagerPermission : public BinderTest,
                                 public testing::WithParamInterface<std::pair<const char*, bool>> {
};

// Test becoming the service manager with and without the `set_context_mgr` permission.
TEST_P(ContextManagerPermission, BecomeServiceManager) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  const auto [label, expect_success] = ContextManagerPermission::GetParam();
  ASSERT_TRUE(RunSubprocessAs(label, [&] {
    fbl::unique_fd binder = OpenBinder(temp_dir_.path());
    EXPECT_TRUE(binder) << strerror(errno);
    if (expect_success) {
      EXPECT_THAT(ioctl(binder.get(), BINDER_SET_CONTEXT_MGR, 0), SyscallSucceeds());
    } else {
      EXPECT_THAT(ioctl(binder.get(), BINDER_SET_CONTEXT_MGR, 0), SyscallFailsWithErrno(EACCES));
    }
  }));
}

const auto kContextManagerPermissionValues =
    ::testing::Values(std::make_pair("test_u:test_r:binder_context_manager_test_t:s0", true),
                      std::make_pair("test_u:test_r:binder_no_context_manager_test_t:s0", false));
INSTANTIATE_TEST_SUITE_P(ContextManagerPermission, ContextManagerPermission,
                         kContextManagerPermissionValues);

class CallPermission : public BinderTest,
                       public testing::WithParamInterface<std::pair<const char*, bool>> {};

// Test doing a binder call with and without the `call` permission.
TEST_P(CallPermission, DoCall) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  const auto [label, expect_success] = CallPermission::GetParam();

  auto context_manager = ScopedContextManagerProcess(temp_dir_.path());

  ASSERT_TRUE(RunSubprocessAs(label, [&] {
    fbl::unique_fd binder = OpenBinder(temp_dir_.path());
    ASSERT_TRUE(binder) << strerror(errno);

    auto mapping = test_helper::ScopedMMap::MMap(nullptr, kBinderMMapSize, PROT_READ, MAP_PRIVATE,
                                                 binder.get(), 0);
    ASSERT_TRUE(mapping.is_ok()) << mapping.error_value();

    binder_helper::TransactionPayload transaction_payload;
    transaction_payload.command_code = BC_TRANSACTION;
    transaction_payload.data.target.handle = kServiceManagerHandle;

    struct binder_write_read payload = {};
    payload.write_buffer = (binder_uintptr_t)&transaction_payload;
    payload.write_size = sizeof(binder_helper::TransactionPayload);

    std::array<int32_t, 32> read_buffer = {};
    payload.read_size = sizeof(read_buffer);
    payload.read_buffer = (binder_uintptr_t)read_buffer.data();
    payload.read_consumed = 0;

    ASSERT_THAT(ioctl(binder.get(), BINDER_WRITE_READ, &payload), SyscallSucceeds());
    binder_helper::ParsedMessage message =
        binder_helper::ParseMessage((binder_uintptr_t)read_buffer.data(), payload.read_consumed);
    if (expect_success) {
      ASSERT_THAT(message.commands_, ::testing::ElementsAre(BR_NOOP, BR_TRANSACTION_COMPLETE));
    } else {
      ASSERT_THAT(message.commands_, ::testing::ElementsAre(BR_NOOP, BR_FAILED_REPLY));
    }
  }));
}

const auto kCallPermissionValues =
    ::testing::Values(std::make_pair("test_u:test_r:binder_ioctl_call_test_t:s0", true),
                      std::make_pair("test_u:test_r:binder_ioctl_no_call_test_t:s0", false));
INSTANTIATE_TEST_SUITE_P(CallPermission, CallPermission, kCallPermissionValues);

}  // namespace
