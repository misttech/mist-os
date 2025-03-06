// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <lib/fit/defer.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/xattr.h>

#include <filesystem>

#include <fbl/unique_fd.h>
#include <gtest/gtest-spi.h>
#include <gtest/gtest.h>
#include <linux/capability.h>
#include <linux/prctl.h>
#include <linux/securebits.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

struct capability_t {
  int cap_num;
  int effective;
  int permitted;
  int inheritable;
  int bounding;
  int ambient;
};

static char kPrintHelperBinary[] = "print_helper";

constexpr size_t kRootUid = 0;
constexpr size_t kRootGid = 0;
constexpr size_t kUser1Uid = 65533;
constexpr size_t kUser1Gid = 65534;

// Runs a program with a single argument in a childe process, wiring stdout to a
// given file descriptor. Runs the given prelude code inside the child process
// before calling execve.
testing::AssertionResult RunSimpleProgram(std::function<void()> prelude, std::string program_path,
                                          std::string argv1, int stdout_fd = 1) {
  ::testing::AssertionResult result = ::testing::AssertionSuccess();

  pid_t pid = SAFE_SYSCALL(fork());
  if (pid == 0) {
    prelude();

    if (stdout_fd != 1) {
      SAFE_SYSCALL(dup2(stdout_fd, 1));
    }

    char *const argv[] = {const_cast<char *>(program_path.c_str()),
                          const_cast<char *>(argv1.c_str()), nullptr};
    char *const envp[] = {nullptr};

    SAFE_SYSCALL(execve(program_path.c_str(), argv, envp));

    _exit(EXIT_FAILURE);
  }

  int wstatus = 0;
  SAFE_SYSCALL(waitpid(pid, &wstatus, 0));
  if (!WIFEXITED(wstatus) || WEXITSTATUS(wstatus) != 0) {
    result = ::testing::AssertionFailure()
             << "wait_status: WIFEXITED(wstatus) = " << WIFEXITED(wstatus)
             << ", WEXITSTATUS(wstatus) = " << WEXITSTATUS(wstatus)
             << ", WTERMSIG(wstatus) = " << WTERMSIG(wstatus);
  }

  return result;
}

std::string GetTestResourcePath(const std::string &resource) {
  std::filesystem::path test_file = std::filesystem::path("/data/tests") / resource;

  std::error_code ec;
  bool file_exists = std::filesystem::exists(test_file, ec);
  EXPECT_FALSE(ec) << "failed to check if file exists: " << ec;

  if (!file_exists) {
    char self_path[PATH_MAX];
    realpath("/proc/self/exe", self_path);
    std::filesystem::path directory = std::filesystem::path(self_path).parent_path();
    return directory / resource;
  }

  return test_file;
}

class CapsExecTest : public ::testing::Test {
 protected:
  void SetUp() {
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities. skipping.";
    }

    // The securebits flag should have no bits set. Individual tests can enable
    // what they use.
    ASSERT_EQ(SAFE_SYSCALL(prctl(PR_GET_SECUREBITS)), 0);

    std::error_code ec;
    std::filesystem::path root = std::filesystem::temp_directory_path(ec);
    ASSERT_FALSE(ec) << "failed to get temp dir: " << ec;

    std::string path_template = root / "capexectest.XXXXXX";
    std::vector<char> mut_path_template(path_template.size() + 1, '\0');
    strncpy(mut_path_template.data(), path_template.c_str(), mut_path_template.size());

    char *tmpdir = mkdtemp(mut_path_template.data());
    ASSERT_NE(tmpdir, nullptr) << "mkdtemp failed: " << strerror(errno);

    path_ = std::string(tmpdir);

    // The test should be able to control the mount flags.
    SAFE_SYSCALL(mount(nullptr, path_.c_str(), "tmpfs", 0, nullptr));

    constexpr int kDirPerms = S_IRWXU | S_IXGRP | S_IXOTH;
    SAFE_SYSCALL(chmod(path_.c_str(), kDirPerms));

    // Copy out the test binary into the temporary directory.
    std::filesystem::path print_helper_binary = GetTestResourcePath(kPrintHelperBinary);

    print_helper_ = path_ / kPrintHelperBinary;
    std::filesystem::copy_file(print_helper_binary.c_str(), print_helper_.c_str(), ec);
    ASSERT_FALSE(ec) << "failed to copy file: " << ec;

    SAFE_SYSCALL(chown(print_helper_.c_str(), kUser1Uid, kUser1Gid));
    SAFE_SYSCALL(chmod(print_helper_.c_str(), S_IRWXU | S_IXGRP | S_IXOTH));

    // The file has no capabilities.
    ASSERT_EQ(getxattr(print_helper_.c_str(), "security.capability", nullptr, 0), -1);
    ASSERT_EQ(errno, ENODATA);

    FILE *fp = fopen("/proc/sys/kernel/cap_last_cap", "r");
    ASSERT_NE(fp, nullptr);
    int n = fscanf(fp, "%d\n", &cap_last_cap_);
    ASSERT_EQ(n, 1);
    fclose(fp);
  }

  void TearDown() {
    if (IsSkipped() && !test_helper::HasSysAdmin()) {
      // We shouldn't run the cleanup step if we skipped the SetUp step.
      return;
    }
    SAFE_SYSCALL(umount2(path_.c_str(), MNT_DETACH));

    std::error_code ec;
    std::filesystem::remove_all(path_, ec);
    ASSERT_FALSE(ec) << "failed to remove temp dir at " << path_ << ": " << ec;
  }

  testing::AssertionResult RunPrintSecurebits(std::function<void()> prelude,
                                              unsigned int *securebits) {
    fbl::unique_fd stdout_fd(SAFE_SYSCALL(test_helper::MemFdCreate("output", 0)));

    testing::AssertionResult result =
        RunSimpleProgram(prelude, print_helper_, "securebits", stdout_fd.get());

    if (!result) {
      return result;
    }

    SAFE_SYSCALL(lseek(stdout_fd.get(), 0, SEEK_SET));
    FILE *fp = fdopen(stdout_fd.release(), "r");
    if (fp == nullptr) {
      return ::testing::AssertionFailure() << "failed to open output fd\n" << strerror(errno);
    }
    auto cleanup = fit::defer([fp]() { SAFE_SYSCALL(fclose(fp)); });

    if (fscanf(fp, "%x\n", securebits) != 1) {
      return ::testing::AssertionFailure() << "failed to read securebits\n";
    }
    return ::testing::AssertionSuccess();
  }

  testing::AssertionResult RunPrintCapabilities(std::function<void()> prelude,
                                                std::vector<capability_t> &capabilities) {
    capabilities.clear();
    ::testing::AssertionResult result = ::testing::AssertionSuccess();
    fbl::unique_fd stdout_fd(SAFE_SYSCALL(test_helper::MemFdCreate("output", 0)));

    result = RunSimpleProgram(prelude, print_helper_, "capabilities", stdout_fd.get());
    if (!result) {
      return result;
    }

    SAFE_SYSCALL(lseek(stdout_fd.get(), 0, SEEK_SET));
    FILE *fp = fdopen(stdout_fd.release(), "r");
    if (fp == nullptr) {
      return ::testing::AssertionFailure() << "failed to open output fd\n" << strerror(errno);
    }
    auto cleanup = fit::defer([fp]() { SAFE_SYSCALL(fclose(fp)); });

    char buf[100] = {0};
    int n = fscanf(fp, "%99s\n", &buf[0]);
    if (n != 1) {
      return ::testing::AssertionFailure() << "failed to read result header";
    }

    if (strcmp(buf, "CAP_NUM,EFFECTIVE,PERMITTED,INHERITABLE,BOUNDING,AMBIENT") != 0) {
      return ::testing::AssertionFailure() << "Header doesn't match expected value. Got: " << buf;
    }

    for (int expected_cap = 0; expected_cap <= cap_last_cap_; expected_cap++) {
      capability_t capability{};
      int n = fscanf(fp, "%d,%d,%d,%d,%d,%d\n", &capability.cap_num, &capability.effective,
                     &capability.permitted, &capability.inheritable, &capability.bounding,
                     &capability.ambient);
      if (n != 6) {
        return ::testing::AssertionFailure() << "invalid row";
      }

      if (capability.cap_num != expected_cap) {
        return ::testing::AssertionFailure() << "Unexpected capability number";
      }

      auto is_valid = [](int val) { return (val == 1 || val == 0); };
      if (!is_valid(capability.effective) || !is_valid(capability.permitted) ||
          !is_valid(capability.inheritable) || !is_valid(capability.bounding) ||
          !is_valid(capability.ambient)) {
        return ::testing::AssertionFailure() << "Invalid capability value";
      }
      capabilities.push_back(capability);
    }

    return result;
  }

  std::filesystem::path path_;
  std::filesystem::path print_helper_;
  int cap_last_cap_;
};

}  // namespace

TEST_F(CapsExecTest, SecurebitKeepCapsIsNotPreservedAcrossExecve) {
  unsigned int securebits;

  ASSERT_TRUE(RunPrintSecurebits([]() { SAFE_SYSCALL(prctl(PR_SET_SECUREBITS, SECBIT_KEEP_CAPS)); },
                                 &securebits));

  EXPECT_NE(securebits & SECBIT_KEEP_CAPS, static_cast<unsigned int>(SECBIT_KEEP_CAPS));
}

TEST_F(CapsExecTest, SecurebitFlagsArePreservedAcrossExecve) {
  unsigned int securebits;

  constexpr unsigned int kSecurebitInheritableFlags =
      SECBIT_NO_SETUID_FIXUP | SECBIT_NO_SETUID_FIXUP_LOCKED | SECBIT_NOROOT |
      SECBIT_NOROOT_LOCKED | SECBIT_NO_CAP_AMBIENT_RAISE | SECBIT_NO_CAP_AMBIENT_RAISE_LOCKED;
  ASSERT_TRUE(RunPrintSecurebits(
      []() { SAFE_SYSCALL(prctl(PR_SET_SECUREBITS, kSecurebitInheritableFlags)); }, &securebits));

  EXPECT_EQ(securebits & kSecurebitInheritableFlags, kSecurebitInheritableFlags);
}

TEST_F(CapsExecTest, NonRootExecutingRegularBinaryClearsPermittedAndEffectiveCaps) {
  // If a non-root user executes a regular binary, without file capabilities,
  // it should clear its permitted and effective capabilities.
  std::vector<capability_t> capabilities;

  ASSERT_TRUE(RunPrintCapabilities(
      []() {
        // We want to keep capabilities after switching uids.
        SAFE_SYSCALL(prctl(PR_SET_SECUREBITS, SECBIT_KEEP_CAPS));

        // After switching to a non-root uid, with the secbits
        // all our effective capabilities are cleaned, but the permitted are
        // still there.
        SAFE_SYSCALL(setresuid(kUser1Uid, kUser1Uid, kUser1Gid));

        // CAP_AUDIT_READ will be effective and permitted.
        test_helper::SetCapabilityEffective(CAP_AUDIT_READ);

        // CAP_AUDIT_WRITE will be effective, permitted, and inheritable.
        test_helper::SetCapabilityEffective(CAP_AUDIT_WRITE);
        test_helper::SetCapabilityInheritable(CAP_AUDIT_WRITE);
      },
      capabilities));

  for (const auto &capability : capabilities) {
    EXPECT_EQ(capability.effective, 0);
    EXPECT_EQ(capability.permitted, 0);
  }
}

TEST_F(CapsExecTest, RootExecutingRegularBinaryGetsAllCapabilitiesBack) {
  // A program with euid root will get all capabilities back when it executes a
  // regular binary.
  std::vector<capability_t> capabilities;

  ASSERT_TRUE(RunPrintCapabilities(
      [&]() {
        // This test doesn't care about CAP_SYS_ADMIN, it only cares about
        // having the root uid.
        SAFE_SYSCALL(setresuid(kRootUid, kRootUid, kRootUid));

        // We expect to have all bounding capabilities set.
        for (int cap = 0; cap <= cap_last_cap_; cap++) {
          ASSERT_TRUE(test_helper::HasCapabilityBounding(cap));
        }

        // Disable one: this should not be enabled after execve.
        test_helper::UnsetCapabilityBounding(CAP_AUDIT_READ);

        test_helper::DropAllCapabilities();
        test_helper::DropAllAmbientCapabilities();
      },
      capabilities));

  for (int cap = 0; cap <= cap_last_cap_; cap++) {
    if (cap == CAP_AUDIT_READ) {
      continue;
    }

    EXPECT_EQ(capabilities[cap].effective, 1);
    EXPECT_EQ(capabilities[cap].permitted, 1);
    EXPECT_EQ(capabilities[cap].inheritable, 0);
    EXPECT_EQ(capabilities[cap].bounding, 1);
    EXPECT_EQ(capabilities[cap].ambient, 0);
  }

  // The one we removed from the bounding set was not added.
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].effective, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].permitted, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].inheritable, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].bounding, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].ambient, 0);
}

TEST_F(CapsExecTest, ChangingFromRootUidDropsAllCapabilities) {
  test_helper::ForkHelper helper;

  ASSERT_EQ(geteuid(), kRootUid);
  helper.RunInForkedProcess([&]() {
    // Changing from a root uid to a non-root uid drops all capabilities.
    SAFE_SYSCALL(setresuid(kUser1Uid, kUser1Uid, kUser1Uid));

    for (int cap = 0; cap <= cap_last_cap_; cap++) {
      EXPECT_TRUE(test_helper::HasCapabilityBounding(cap));
      EXPECT_FALSE(test_helper::HasCapabilityAmbient(cap));
      EXPECT_FALSE(test_helper::HasCapabilityEffective(cap));
      EXPECT_FALSE(test_helper::HasCapabilityPermitted(cap));
      EXPECT_FALSE(test_helper::HasCapabilityInheritable(cap));
    }
  });
}

TEST_F(CapsExecTest, RegularUserExecutingSUIDRootGetsAllCapabilities) {
  /*
     When a process with nonzero UIDs execves a set-user-ID root program
     that does not have capabilities attached, the calculation of the permitted
     and effective capabilities is as follows:

     P'(permitted)   = P(inheritable) | P(bounding)
     P'(effective)   = P'(permitted)
  */
  std::vector<capability_t> capabilities;

  SAFE_SYSCALL(chown(print_helper_.c_str(), kRootUid, kRootGid));
  SAFE_SYSCALL(chmod(print_helper_.c_str(), S_ISUID | S_IXOTH | S_IRWXU));

  ASSERT_TRUE(RunPrintCapabilities(
      [&]() {
        SAFE_SYSCALL(setresgid(kUser1Gid, kUser1Gid, kUser1Gid));
        SAFE_SYSCALL(setgroups(0, nullptr));

        // Will not drop permitted capabilities.
        SAFE_SYSCALL(prctl(PR_SET_SECUREBITS, SECBIT_KEEP_CAPS));
        SAFE_SYSCALL(setresuid(kUser1Uid, kUser1Uid, kUser1Uid));

        test_helper::SetCapabilityEffective(CAP_SETPCAP);

        // Leave only CAP_SETPCAP in effective capabilities.
        for (int cap = 0; cap < cap_last_cap_; cap++) {
          if (cap == CAP_SETPCAP) {
            continue;
          }
          test_helper::UnsetCapabilityEffective(cap);
          test_helper::UnsetCapabilityPermitted(cap);
          test_helper::UnsetCapabilityInheritable(cap);
          test_helper::UnsetCapabilityAmbient(cap);
        }

        // CAP_SYSLOG will be in the bounding set but not in inheritable.
        ASSERT_TRUE(test_helper::HasCapabilityBounding(CAP_SYSLOG));
        ASSERT_FALSE(test_helper::HasCapabilityInheritable(CAP_SYSLOG));

        // CAP_AUDIT_READ will not be in bounding nor in inheritable.
        test_helper::UnsetCapabilityBounding(CAP_AUDIT_READ);

        // CAP_AUDIT_CONTROL will be both inheritable and bounding.
        test_helper::SetCapabilityInheritable(CAP_AUDIT_CONTROL);

        // CAP_AUDIT_WRITE will be inheritable but not in bounding.
        test_helper::SetCapabilityInheritable(CAP_AUDIT_WRITE);
        test_helper::UnsetCapabilityBounding(CAP_AUDIT_WRITE);

        test_helper::UnsetCapabilityEffective(CAP_SETPCAP);
        test_helper::UnsetCapabilityPermitted(CAP_SETPCAP);
        test_helper::UnsetCapabilityInheritable(CAP_SETPCAP);
        test_helper::UnsetCapabilityAmbient(CAP_SETPCAP);
      },
      capabilities));

  EXPECT_EQ(capabilities[CAP_SYSLOG].effective, 1);
  EXPECT_EQ(capabilities[CAP_SYSLOG].permitted, 1);
  EXPECT_EQ(capabilities[CAP_SYSLOG].inheritable, 0);
  EXPECT_EQ(capabilities[CAP_SYSLOG].bounding, 1);
  EXPECT_EQ(capabilities[CAP_SYSLOG].ambient, 0);

  EXPECT_EQ(capabilities[CAP_AUDIT_READ].effective, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].permitted, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].inheritable, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].bounding, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_READ].ambient, 0);

  EXPECT_EQ(capabilities[CAP_AUDIT_WRITE].effective, 1);
  EXPECT_EQ(capabilities[CAP_AUDIT_WRITE].permitted, 1);
  EXPECT_EQ(capabilities[CAP_AUDIT_WRITE].inheritable, 1);
  EXPECT_EQ(capabilities[CAP_AUDIT_WRITE].bounding, 0);
  EXPECT_EQ(capabilities[CAP_AUDIT_WRITE].ambient, 0);

  EXPECT_EQ(capabilities[CAP_AUDIT_CONTROL].effective, 1);
  EXPECT_EQ(capabilities[CAP_AUDIT_CONTROL].permitted, 1);
  EXPECT_EQ(capabilities[CAP_AUDIT_CONTROL].inheritable, 1);
  EXPECT_EQ(capabilities[CAP_AUDIT_CONTROL].bounding, 1);
  EXPECT_EQ(capabilities[CAP_AUDIT_CONTROL].ambient, 0);
}

void SetUpCapabilities(capability_t cap) {
  test_helper::SetCapabilityEffective(CAP_SETPCAP);
  int cap_num = cap.cap_num;

  test_helper::SetCapabilityEffective(cap_num);
  test_helper::SetCapabilityPermitted(cap_num);
  test_helper::SetCapabilityInheritable(cap_num);
  test_helper::SetCapabilityAmbient(cap_num);

  if (cap.effective == 0) {
    test_helper::UnsetCapabilityEffective(cap_num);
  }
  if (cap.permitted == 0) {
    test_helper::UnsetCapabilityPermitted(cap_num);
  }
  if (cap.inheritable == 0) {
    test_helper::UnsetCapabilityInheritable(cap_num);
  }
  if (cap.ambient == 0) {
    test_helper::UnsetCapabilityAmbient(cap_num);
  }
  if (cap.bounding == 0) {
    test_helper::UnsetCapabilityBounding(cap_num);
  }

  EXPECT_EQ(test_helper::HasCapabilityEffective(cap_num), cap.effective);
  EXPECT_EQ(test_helper::HasCapabilityPermitted(cap_num), cap.permitted);
  EXPECT_EQ(test_helper::HasCapabilityInheritable(cap_num), cap.inheritable);
  EXPECT_EQ(test_helper::HasCapabilityAmbient(cap_num), cap.ambient);
  EXPECT_EQ(test_helper::HasCapabilityBounding(cap_num), cap.bounding);
}

TEST(CapsTest, PermittedCapabilitiesCannotBeRegained) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities. skipping.";
  }

  std::vector<capability_t> caps;

  // Capabilities combinations that don't have the permitted bit.
  caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 0, 0});
  caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 1, 0});
  caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 0, 0});
  caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 1, 0});

  test_helper::ForkHelper helper;
  for (const auto &cap : caps) {
    helper.RunInForkedProcess([cap]() {
      SetUpCapabilities(cap);
      EXPECT_NONFATAL_FAILURE(test_helper::SetCapabilityPermitted(cap.cap_num),
                              "Operation not permitted");
    });
    EXPECT_TRUE(helper.WaitForChildren());
  }
}

TEST(CapsTest, PermittedCapabilitiesCannotBeDroppedIfTheyAreEffective) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities. skipping.";
  }

  std::vector<capability_t> caps;

  // Capabilities combinations that have effective and permitted bits.
  caps.push_back({CAP_AUDIT_READ, 1, 1, 0, 0, 0});
  caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 0, 0});
  caps.push_back({CAP_AUDIT_READ, 1, 1, 0, 1, 0});
  caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 1, 0});
  caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 1, 1});

  test_helper::ForkHelper helper;
  for (const auto &cap : caps) {
    helper.RunInForkedProcess([cap]() {
      SetUpCapabilities(cap);
      EXPECT_NONFATAL_FAILURE(test_helper::UnsetCapabilityPermitted(cap.cap_num),
                              "Operation not permitted");
    });
    EXPECT_TRUE(helper.WaitForChildren());
  }
}

TEST(CapsTest, CannotSetEffectiveCapsIfNotPermitted) {
  // Tests that you cannot set a capability as effective if it's not permitted.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities. skipping.";
  }

  std::vector<capability_t> test_cases;

  test_cases.push_back({CAP_AUDIT_READ, 0, 0, 0, 0, 0});
  test_cases.push_back({CAP_AUDIT_READ, 0, 0, 0, 1, 0});
  test_cases.push_back({CAP_AUDIT_READ, 0, 0, 1, 0, 0});
  test_cases.push_back({CAP_AUDIT_READ, 0, 0, 1, 1, 0});

  test_helper::ForkHelper helper;

  for (const auto &test_case : test_cases) {
    helper.RunInForkedProcess([test_case]() {
      SetUpCapabilities(test_case);
      EXPECT_NONFATAL_FAILURE(test_helper::SetCapabilityEffective(CAP_AUDIT_READ), "SYS_capset");
      EXPECT_FALSE(test_helper::HasCapabilityEffective(CAP_AUDIT_READ));
    });
    EXPECT_TRUE(helper.WaitForChildren());
  }
}

TEST(CapsTest, AmbientCapabilitiesRequirePermittedAndInheritable) {
  // Tests that ambient capabilities will be unset if inheritable or permitted
  // are unset.
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities. skipping.";
  }

  struct test_case_t {
    capability_t caps;
    const char *failure;
  };

  std::vector<test_case_t> test_cases;

  test_cases.push_back({{CAP_AUDIT_READ, 0, 0, 0, 0, 1}, "HasCapabilityAmbient"});
  test_cases.push_back({{CAP_AUDIT_READ, 0, 0, 0, 1, 1}, "HasCapabilityAmbient"});
  test_cases.push_back({{CAP_AUDIT_READ, 0, 0, 1, 0, 1}, "HasCapabilityAmbient"});
  test_cases.push_back({{CAP_AUDIT_READ, 0, 0, 1, 1, 1}, "HasCapabilityAmbient"});
  test_cases.push_back({{CAP_AUDIT_READ, 0, 1, 0, 0, 1}, "HasCapabilityAmbient"});
  test_cases.push_back({{CAP_AUDIT_READ, 0, 1, 0, 1, 1}, "HasCapabilityAmbient"});
  test_cases.push_back({{CAP_AUDIT_READ, 1, 1, 0, 0, 1}, "HasCapabilityAmbient"});
  test_cases.push_back({{CAP_AUDIT_READ, 1, 1, 0, 1, 1}, "HasCapabilityAmbient"});

  test_helper::ForkHelper helper;

  for (const auto &test_case : test_cases) {
    SCOPED_TRACE(fxl::StringPrintf("(Eff Per Inh Bnd Amb) (%d %d %d %d %d)",
                                   test_case.caps.effective, test_case.caps.permitted,
                                   test_case.caps.inheritable, test_case.caps.bounding,
                                   test_case.caps.ambient));
    helper.RunInForkedProcess([test_case]() {
      EXPECT_NONFATAL_FAILURE(SetUpCapabilities(test_case.caps), test_case.failure);
    });
    EXPECT_TRUE(helper.WaitForChildren());
  }
}

TEST_F(CapsExecTest, NonRootUserExecutingNonSUIDProgram) {
  std::vector<capability_t> starting_caps;
  std::vector<capability_t> expected_caps;

  // Effective Permitted Inheritable Bounding Ambient
  //                                      Ef Pe In Bn Am
  starting_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 0, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 0, 0});

  // NOT POSSIBLE                          0, 0, 0, 0, 1

  starting_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 1, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 1, 0});

  // NOT POSSIBLE                          0, 0, 0, 1, 1

  starting_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 0, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 0, 0});

  // NOT POSSIBLE                          0, 0, 1, 0, 1

  starting_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 1, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 1, 0});

  // NOT POSSIBLE                          0, 0, 1, 1, 1

  starting_caps.push_back({CAP_AUDIT_READ, 0, 1, 0, 0, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 0, 0});

  // NOT POSSIBLE                          0, 1, 0, 0, 1

  starting_caps.push_back({CAP_AUDIT_READ, 0, 1, 0, 1, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 1, 0});

  // NOT POSSIBLE                          0, 1, 0, 1, 1

  starting_caps.push_back({CAP_AUDIT_READ, 0, 1, 1, 0, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 0, 0});

  starting_caps.push_back({CAP_AUDIT_READ, 0, 1, 1, 0, 1});
  expected_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 0, 1});

  starting_caps.push_back({CAP_AUDIT_READ, 0, 1, 1, 1, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 1, 0});

  starting_caps.push_back({CAP_AUDIT_READ, 0, 1, 1, 1, 1});
  expected_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 1, 1});

  // NOT POSSIBLE                          1, 0, 0, 0, 0
  // NOT POSSIBLE                          1, 0, 0, 0, 1
  // NOT POSSIBLE                          1, 0, 0, 1, 0
  // NOT POSSIBLE                          1, 0, 0, 1, 1
  // NOT POSSIBLE                          1, 0, 1, 0, 0
  // NOT POSSIBLE                          1, 0, 1, 0, 1
  // NOT POSSIBLE                          1, 0, 1, 1, 0
  // NOT POSSIBLE                          1, 0, 1, 1, 1

  starting_caps.push_back({CAP_AUDIT_READ, 1, 1, 0, 0, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 0, 0});

  // NOT POSSIBLE                          1, 1, 0, 0, 1

  starting_caps.push_back({CAP_AUDIT_READ, 1, 1, 0, 1, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 0, 1, 0});

  // NOT POSSIBLE                          1, 1, 0, 1, 1

  starting_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 0, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 0, 0});

  starting_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 0, 1});
  expected_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 0, 1});

  starting_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 1, 0});
  expected_caps.push_back({CAP_AUDIT_READ, 0, 0, 1, 1, 0});

  starting_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 1, 1});
  expected_caps.push_back({CAP_AUDIT_READ, 1, 1, 1, 1, 1});

  for (size_t test_case = 0; test_case < starting_caps.size(); test_case++) {
    SCOPED_TRACE(
        fxl::StringPrintf("(Eff Per Inh Bnd Amb) (%d %d %d %d %d) -> (%d %d %d %d %d)",
                          starting_caps[test_case].effective, starting_caps[test_case].permitted,
                          starting_caps[test_case].inheritable, starting_caps[test_case].bounding,
                          starting_caps[test_case].ambient, expected_caps[test_case].effective,
                          expected_caps[test_case].permitted, expected_caps[test_case].inheritable,
                          expected_caps[test_case].bounding, expected_caps[test_case].ambient));

    std::vector<capability_t> capabilities;
    ASSERT_TRUE(RunPrintCapabilities(
        [&]() {
          SAFE_SYSCALL(setresgid(kUser1Gid, kUser1Gid, kUser1Gid));
          SAFE_SYSCALL(setgroups(0, nullptr));

          // Will not drop permitted capabilities.
          SAFE_SYSCALL(prctl(PR_SET_SECUREBITS, SECBIT_KEEP_CAPS));
          SAFE_SYSCALL(setresuid(kUser1Uid, kUser1Uid, kUser1Uid));
          SetUpCapabilities(starting_caps[test_case]);
          // Drop all other capabilities.
          for (int cap = 0; cap <= cap_last_cap_; cap++) {
            if (cap == starting_caps[test_case].cap_num)
              continue;
            test_helper::UnsetCapabilityEffective(cap);
            test_helper::UnsetCapabilityPermitted(cap);
            test_helper::UnsetCapabilityInheritable(cap);
            test_helper::UnsetCapabilityAmbient(cap);
          }
        },
        capabilities));

    int cap_num = starting_caps[test_case].cap_num;

    EXPECT_EQ(capabilities[cap_num].effective, expected_caps[test_case].effective);
    EXPECT_EQ(capabilities[cap_num].permitted, expected_caps[test_case].permitted);
    EXPECT_EQ(capabilities[cap_num].inheritable, expected_caps[test_case].inheritable);
    EXPECT_EQ(capabilities[cap_num].bounding, expected_caps[test_case].bounding);
    EXPECT_EQ(capabilities[cap_num].ambient, expected_caps[test_case].ambient);
  }
}
