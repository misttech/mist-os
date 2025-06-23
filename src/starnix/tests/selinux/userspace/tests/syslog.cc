// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"
#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

extern std::string DoPrePolicyLoadWork() { return "syslog.pp"; }

namespace {

TEST(SyslogTest, Allowed) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_has_syslog_mod_t:s0", [&] {
    fbl::unique_fd proc_kmsg = fbl::unique_fd(open("/proc/kmsg", O_RDONLY | O_NONBLOCK));
    ASSERT_TRUE(proc_kmsg) << strerror(errno);
    char buf;
    EXPECT_THAT(read(proc_kmsg.get(), &buf, 1),
                ::testing::AnyOf(SyscallSucceeds(), SyscallFailsWithErrno(EAGAIN)));
  }));
}

TEST(SyslogTest, Denied) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_no_syslog_mod_t:s0", [&] {
    EXPECT_THAT(open("/proc/kmsg", O_RDONLY | O_NONBLOCK), SyscallFailsWithErrno(EACCES));
  }));
}

TEST(SyslogTest, ReadDeniedIfDroppedPermission) {
  auto enforce = ScopedEnforcement::SetEnforcing();
  ASSERT_TRUE(RunSubprocessAs("test_u:test_r:syslog_test_has_syslog_mod_t:s0", [&] {
    fbl::unique_fd proc_kmsg = fbl::unique_fd(open("/proc/kmsg", O_RDONLY | O_NONBLOCK));
    ASSERT_TRUE(proc_kmsg) << strerror(errno);
    ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:syslog_test_no_syslog_mod_t:s0"), fit::ok());
    char buf;
    EXPECT_THAT(read(proc_kmsg.get(), &buf, 1), SyscallFailsWithErrno(EACCES));
  }));
}

}  // namespace
