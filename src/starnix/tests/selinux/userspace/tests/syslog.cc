// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/sysmacros.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"
#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

extern std::string DoPrePolicyLoadWork() {
  EXPECT_THAT(mknod("/tmp/dev_kmsg", S_IFCHR | 0600, makedev(1, 11)), SyscallSucceeds());
  return "syslog.pp";
}

namespace {

TEST(SyslogTest, ProcKmsgAllowed) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_has_syslog_mod_t:s0", [&] {
    fbl::unique_fd proc_kmsg = fbl::unique_fd(open("/proc/kmsg", O_RDONLY | O_NONBLOCK));
    ASSERT_TRUE(proc_kmsg) << strerror(errno);
    char buf;
    EXPECT_THAT(read(proc_kmsg.get(), &buf, 1),
                ::testing::AnyOf(SyscallSucceeds(), SyscallFailsWithErrno(EAGAIN)));
  }));
}

TEST(SyslogTest, ProcKmsgDenied) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_no_syslog_perm_t:s0", [&] {
    EXPECT_THAT(open("/proc/kmsg", O_RDONLY | O_NONBLOCK), SyscallFailsWithErrno(EACCES));
  }));
}

TEST(SyslogTest, ProcKmsgReadDeniedIfDroppedPermission) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_has_syslog_mod_t:s0", [&] {
    fbl::unique_fd proc_kmsg = fbl::unique_fd(open("/proc/kmsg", O_RDONLY | O_NONBLOCK));
    ASSERT_TRUE(proc_kmsg) << strerror(errno);
    ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:syslog_test_no_syslog_perm_t:s0"), fit::ok());
    char buf;
    EXPECT_THAT(read(proc_kmsg.get(), &buf, 1), SyscallFailsWithErrno(EACCES));
  }));
}

TEST(SyslogTest, DevKmsgReadAllowed) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_has_syslog_read_t:s0", [&] {
    fbl::unique_fd dev_kmsg = fbl::unique_fd(open("/tmp/dev_kmsg", O_RDONLY | O_NONBLOCK));
    ASSERT_TRUE(dev_kmsg) << strerror(errno);
    char buf[4096];
    EXPECT_THAT(read(dev_kmsg.get(), buf, sizeof(buf)),
                ::testing::AnyOf(SyscallSucceeds(), SyscallFailsWithErrno(EAGAIN)));
  }));
}

TEST(SyslogTest, DevKmsgReadDenied) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_no_syslog_perm_t:s0", [&] {
    EXPECT_THAT(open("/tmp/dev_kmsg", O_RDONLY | O_NONBLOCK), SyscallFailsWithErrno(EACCES));
  }));
}

TEST(SyslogTest, DevKmsgReadAllowedIfDroppedPermission) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_has_syslog_read_t:s0", [&] {
    fbl::unique_fd dev_kmsg = fbl::unique_fd(open("/tmp/dev_kmsg", O_RDONLY | O_NONBLOCK));
    ASSERT_TRUE(dev_kmsg) << strerror(errno);

    // Permissions are not re-checked after opening.
    ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:syslog_test_no_syslog_perm_t:s0"), fit::ok());
    char buf[4096];
    EXPECT_THAT(read(dev_kmsg.get(), buf, sizeof(buf)),
                ::testing::AnyOf(SyscallSucceeds(), SyscallFailsWithErrno(EAGAIN)));
  }));
}

TEST(SyslogTest, DevKmsgWriteAllowed) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_no_syslog_perm_t:s0", [&] {
    fbl::unique_fd dev_kmsg = fbl::unique_fd(open("/tmp/dev_kmsg", O_WRONLY | O_NONBLOCK));
    ASSERT_TRUE(dev_kmsg) << strerror(errno);
    char buf[] = "hello, world\n";
    EXPECT_THAT(write(dev_kmsg.get(), buf, sizeof(buf)), SyscallSucceeds());
  }));
}

}  // namespace
