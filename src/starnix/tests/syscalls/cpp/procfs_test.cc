// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/fsuid.h>
#include <sys/prctl.h>
#include <unistd.h>

#include <filesystem>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/cpp/proc_test_base.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

using testing::ContainsRegex;
using testing::MatchesRegex;

class ProcUptimeTest : public ProcTestBase {
 protected:
  void SetUp() override {
    ProcTestBase::SetUp();
    Open();
  }

  void Close() { fd_.reset(); }

  void Open() {
    std::string path = proc_path() + "/uptime";
    fd_.reset(open(path.c_str(), O_RDONLY));
    ASSERT_TRUE(fd_.is_valid());
  }

  double Parse(const char* buf) {
    double uptime, idle;
    int s = sscanf(buf, "%lf %lf\n", &uptime, &idle);
    EXPECT_EQ(s, 2);

    // On Linux `idle` value may decrease, i.e. we cannot expect it to
    // increase with `uptime` (see https://fxbug.dev/42080772). Ignore it.

    return uptime;
  }

  double Read() {
    char buf[100];
    long r = read(fd_.get(), buf, sizeof(buf));
    EXPECT_GT(r, 0);

    return Parse(buf);
  }

  ~ProcUptimeTest() override { Close(); }

  fbl::unique_fd fd_;
};

TEST_F(ProcUptimeTest, UptimeRead) { Read(); }

TEST_F(ProcUptimeTest, UptimeProgressReopen) {
  auto v1 = Read();
  Close();
  sleep(1);
  Open();
  auto v2 = Read();
  EXPECT_GT(v2, v1);
}

// Verify that the reported value is updated after seeking /proc/uptime to the beginning.
TEST_F(ProcUptimeTest, UptimeProgressSeek) {
  auto v1 = Read();

  off_t pos = lseek(fd_.get(), 0, SEEK_SET);
  ASSERT_EQ(pos, 0);

  sleep(1);
  auto v2 = Read();

  EXPECT_GT(v2, v1);
}

// Verify that a valid value is produced even when reading by single char.
TEST_F(ProcUptimeTest, UptimeByChar) {
  auto v1 = Read();
  Open();

  std::string buf;
  char c;
  ssize_t r = read(fd_.get(), &c, 1);
  ASSERT_EQ(r, 1);
  buf.push_back(c);

  // Keep the FD, and then read from a new FD.
  fbl::unique_fd old_fd = std::move(fd_);
  Open();
  auto v2 = Read();

  // Wait for a bit and then read the old FD to the end.
  sleep(1);
  while ((r = read(old_fd.get(), &c, 1)) == 1) {
    buf.push_back(c);
  }

  auto v3 = Parse(buf.c_str());

  // `v3` should be between `v1` and `v2`.
  EXPECT_LE(v1, v3);
  EXPECT_LE(v3, v2);
}

class ProcSysNetTest : public ProcTestBase,
                       public ::testing::WithParamInterface<std::tuple<const char*, const char*>> {
 protected:
  void SetUp() override {
    ProcTestBase::SetUp();
    // Required to open the path below for writing.
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities, skipping.";
    }

    auto const& [dev, path_fmt] = GetParam();
    char buf[100] = {};
    sprintf(buf, path_fmt, dev);
    std::string path = proc_path() + "/sys/net" + buf;
    fd_.reset(open(path.c_str(), O_RDWR));
    ASSERT_TRUE(fd_.is_valid()) << "path: " + path + "; " + strerror(errno);
  }

  fbl::unique_fd fd_;
};

TEST_P(ProcSysNetTest, Write) {
  constexpr uint8_t kWriteBuf = 127;
  ASSERT_EQ(write(fd_.get(), &kWriteBuf, sizeof(kWriteBuf)),
            static_cast<ssize_t>(sizeof(kWriteBuf)))
      << strerror(errno);
}

INSTANTIATE_TEST_SUITE_P(
    ProcSysNetTest, ProcSysNetTest,
    ::testing::Combine(
        ::testing::Values("all", "default"),
        ::testing::Values("/ipv4/neigh/%s/ucast_solicit", "/ipv4/neigh/%s/retrans_time_ms",
                          "/ipv4/neigh/%s/mcast_resolicit", "/ipv6/conf/%s/accept_ra",
                          "/ipv6/conf/%s/dad_transmits", "/ipv6/conf/%s/use_tempaddr",
                          "/ipv6/conf/%s/addr_gen_mode", "/ipv6/conf/%s/stable_secret",
                          "/ipv6/conf/%s/disable_ipv6", "/ipv6/neigh/%s/ucast_solicit",
                          "/ipv6/neigh/%s/retrans_time_ms", "/ipv6/neigh/%s/mcast_resolicit")));

using ProcTest = ProcTestBase;

// Test that after forking without execing, /proc/self/cmdline still works.
TEST_F(ProcTest, CmdlineAfterFork) {
  char cmdline[100];
  int cmdline_fd = open("/proc/self/cmdline", O_RDONLY);
  ASSERT_GT(cmdline_fd, 0) << strerror(errno);
  ssize_t cmdline_len = read(cmdline_fd, cmdline, sizeof(cmdline));
  ASSERT_GT(cmdline_len, 0) << strerror(errno);
  close(cmdline_fd);

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    char child_cmdline[100];
    int cmdline_fd = open("/proc/self/cmdline", O_RDONLY);
    ASSERT_GT(cmdline_fd, 0) << strerror(errno);
    ssize_t child_cmdline_len = read(cmdline_fd, child_cmdline, sizeof(child_cmdline));
    ASSERT_GT(child_cmdline_len, 0) << strerror(errno);
    close(cmdline_fd);

    ASSERT_EQ(cmdline_len, child_cmdline_len);
    ASSERT_TRUE(memcmp(cmdline, child_cmdline, cmdline_len) == 0);
  });

  ASSERT_TRUE(helper.WaitForChildren());
}

class ProcTaskDirTest : public ProcTestBase {
 protected:
  void SetUp() override { ProcTestBase::SetUp(); }
};

bool ends_with(const std::string& haystack, const std::string needle) {
  return haystack.rfind(needle) == (haystack.size() - needle.size());
}

std::string ProcSelfDirName() {
  std::string proc_self = fxl::StringPrintf("/proc/%d", getpid());
  struct stat statbuf;
  SAFE_SYSCALL(stat(proc_self.c_str(), &statbuf));
  return proc_self;
}

// Ensure that entries in /proc/pid/. have the correct ownership.  proc(2) says
// that entries are owned by the effective uid of the task. This doesn't seem to
// be exactly what Linux does - Linux seems to assign most directories to the
// euid, and everything else to the real id - but it's not clear exactly what
// Linux is doing from looking at its behavior, so we hew to the man page.
//
// This test ensures that the proc directories *are* set to be owned by the euid
// of the task.
TEST_F(ProcTaskDirTest, PidDirCorrectUidIsEuid) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::HasSysAdmin() || !test_helper::IsStarnix()) {
    GTEST_SKIP() << "PidDirCorrectUid requires root access (to change euid), "
                 << "and currently only works on Starnix";
  }

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    std::string proc_path = ProcSelfDirName();

    int dirfd;
    struct stat pre_stat;
    struct stat euid_stat;

    // Get the original (presumably real) ownership
    ASSERT_NE(-1, dirfd = open(proc_path.c_str(), O_RDONLY))
        << "Error trying to open " << proc_path << ": " << strerror(errno);
    SAFE_SYSCALL(fstat(dirfd, &pre_stat));
    SAFE_SYSCALL(close(dirfd));

    // Set the effective uid.
    uid_t newuid = geteuid() + 1;
    SAFE_SYSCALL(seteuid(newuid));

    // From proc(5): The files inside each /proc/pid directory are normally
    // owned by the effective user and effective group ID of the process.
    // However, as a security measure, the ownership is made root:root
    // if the process's "dumpable" attribute is set to a value other than 1.
    SAFE_SYSCALL(prctl(PR_SET_DUMPABLE, 1));

    // Make sure the effective uid appears to own /proc/self.
    SAFE_SYSCALL(dirfd = open(proc_path.c_str(), O_RDONLY));
    SAFE_SYSCALL(fstat(dirfd, &euid_stat));
    SAFE_SYSCALL(close(dirfd));

    EXPECT_EQ(euid_stat.st_uid, newuid)
        << "owner for " << proc_path << " did not change to correct value";

    std::vector<std::string> dirs;
    files::ReadDirContents(proc_path, &dirs);
    for (const auto& entry : dirs) {
      if ((entry == ".") || (entry == "..")) {
        continue;
      }
      struct stat info;
      auto fname =
          (entry[0] == '/') ? entry : fxl::StringPrintf("%s/%s", proc_path.c_str(), entry.c_str());

      ASSERT_NE(-1, stat(fname.c_str(), &info))
          << "Error reading " << fname << ": " << strerror(errno);

      // Links to other parts of the fs have their original ownership.  S_ISLNK does
      // not currently work on these files.  See https://fxbug.dev/331990255.
      if (ends_with(fname, "/cwd") || ends_with(fname, "/root") || ends_with(fname, "/exe")) {
        continue;
      }

      EXPECT_EQ(info.st_uid, euid_stat.st_uid) << "Wrong owner for file " << fname;
    }
  });
}

// This test ensures that the proc directories aren't set to be owned by the
// fsuid of the task. This is separate from PidDirCorrectUidIsEuid because
// setting the euid implicitly sets the fsuid. We want to check we're not
// accidentally relying on the fsuid.
TEST_F(ProcTaskDirTest, PidDirSetFsuidDoesntChangeOwnership) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::HasSysAdmin() || !test_helper::IsStarnix()) {
    GTEST_SKIP() << "PidDirCorrectUid requires root access (to change euid), "
                 << "and currently only works on Starnix";
  }

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    std::string proc_path = ProcSelfDirName();

    int dirfd;
    struct stat pre_stat;
    struct stat fsuid_stat;
    uid_t newuid;

    // Get the original (presumably real) ownership
    ASSERT_NE(-1, dirfd = open(proc_path.c_str(), O_RDONLY))
        << "Error trying to open " << proc_path << ": " << strerror(errno);
    SAFE_SYSCALL(fstat(dirfd, &pre_stat));
    SAFE_SYSCALL(close(dirfd));

    // Set the fsuid.
    newuid = pre_stat.st_uid + 1;
    ASSERT_EQ(static_cast<const int>(pre_stat.st_uid), setfsuid(newuid)) << "Unexpected fsuid";
    // This is how you check to see if a call to setfsuid worked correctly.
    ASSERT_EQ(static_cast<const int>(newuid), setfsuid(-1)) << "setfsuid not supported";

    // Make sure that the current owner has *not* changed to the fsuid
    SAFE_SYSCALL(dirfd = open(proc_path.c_str(), O_RDONLY));
    SAFE_SYSCALL(fstat(dirfd, &fsuid_stat));
    SAFE_SYSCALL(close(dirfd));

    EXPECT_NE(fsuid_stat.st_uid, newuid) << "fsuid seen to change incorrectly";
    // Revert the fsuid
    ASSERT_EQ(static_cast<const int>(newuid), setfsuid(pre_stat.st_uid));
  });
}

TEST_F(ProcTaskDirTest, PidDirCorrectIno) {
  const char kProcPath[] = "/proc/self/status";
  int fd;
  struct stat pre_stat;
  struct stat post_stat;

  SAFE_SYSCALL(fd = open(kProcPath, O_RDONLY));
  SAFE_SYSCALL(fstat(fd, &pre_stat));
  SAFE_SYSCALL(close(fd));

  SAFE_SYSCALL(fd = open(kProcPath, O_RDONLY));
  SAFE_SYSCALL(fstat(fd, &post_stat));
  SAFE_SYSCALL(close(fd));

  ASSERT_EQ(pre_stat.st_ino, post_stat.st_ino) << "Inode number incorrectly seen to change";
}

constexpr char kProcSelfFdPath[] = "/proc/self/fd/%d";

TEST_F(ProcTaskDirTest, FdOpath) {
  test_helper::ScopedTempFD temp_file;
  ASSERT_TRUE(temp_file);

  fbl::unique_fd opath_fd(SAFE_SYSCALL(open(temp_file.name().c_str(), O_RDONLY | O_PATH)));
  ASSERT_TRUE(opath_fd.is_valid());

  std::string opath_fd_path = fxl::StringPrintf(kProcSelfFdPath, opath_fd.get());
  fbl::unique_fd regular_fd(SAFE_SYSCALL(open(opath_fd_path.c_str(), O_RDONLY)));
  ASSERT_TRUE(regular_fd.is_valid());
}

TEST_F(ProcTaskDirTest, FdOpathSymlink) {
  test_helper::ScopedTempFD temp_file;
  ASSERT_TRUE(temp_file);

  test_helper::ScopedTempSymlink temp_symlink(temp_file.name().c_str());
  ASSERT_TRUE(temp_symlink);

  fbl::unique_fd symlink_fd(SAFE_SYSCALL(open(temp_symlink.path().c_str(), O_PATH | O_NOFOLLOW)));
  ASSERT_TRUE(symlink_fd.is_valid());

  std::string symlink_fd_path = fxl::StringPrintf(kProcSelfFdPath, symlink_fd.get());
  ASSERT_THAT(open(symlink_fd_path.c_str(), O_RDONLY), SyscallFailsWithErrno(ELOOP));
}

class ProcfsTest : public ProcTestBase {
 protected:
  // Verifies whether the input string is a valid UUID in hyphenated format
  // Example:
  // af0af413-58e1-4210-bb57-bc9a9d3ca44a
  // Segment lengths:
  //     8   |  4 |  4 |  4 |   12
  void is_valid_uuid(std::string in) {
    size_t pos = 0;
    std::string token;
    std::array<size_t, 5> lengths = {8, 4, 4, 4, 12};  // Lengths of tokens
    // Some Linux kernels have a newline character at the end
    if (in[in.size() - 1] == '\n') {
      in.pop_back();
    }
    in.append("-");

    // For each hyphen-delimited token of the UUID string
    for (size_t length : lengths) {
      // Grab token
      ASSERT_TRUE((pos = in.find("-")) != std::string::npos);
      token = in.substr(0, pos);
      in.erase(0, pos + 1);
      ASSERT_TRUE(length == token.size());

      // All characters are hexadecimal digits
      ASSERT_TRUE(std::all_of(token.begin(), token.end(), [](char c) { return std::isxdigit(c); }));
    }
  }
};

// Verify /proc/sys/kernel/random/boot_id exists and has the boot UUID
TEST_F(ProcfsTest, ProcSysKernelRandomBootIdExists) {
  std::string uuid;
  EXPECT_EQ(0, access("/proc/sys/kernel/random/boot_id", R_OK));
  EXPECT_TRUE(files::ReadFileToString("/proc/sys/kernel/random/boot_id", &uuid));
  is_valid_uuid(uuid);
}

// Verify /proc/pressure/{cpu,io,memory} contains something reasonable.
TEST_F(ProcfsTest, ProcPressure) {
  for (auto path : {"/proc/pressure/cpu", "/proc/pressure/io", "/proc/pressure/memory"}) {
    EXPECT_EQ(0, access(path, R_OK));
    std::string content;
    EXPECT_TRUE(files::ReadFileToString(path, &content));
    // Some systems does not eport the `full` statistics for CPU.
    EXPECT_THAT(content,
                MatchesRegex("some avg10=[0-9.]+ avg60=[0-9.]+ avg300=[0-9.]+ total=[0-9.]+\n"
                             "(full avg10=[0-9.]+ avg60=[0-9.]+ avg300=[0-9.]+ total=[0-9.]+\n)?"));
  }
}

// Verify that /proc/zoneinfo contains something reasonable.
TEST_F(ProcfsTest, ZoneInfo) {
  auto path = "/proc/zoneinfo";
  EXPECT_EQ(0, access(path, R_OK));
  std::string content;
  ASSERT_TRUE(files::ReadFileToString(path, &content));
  // Ensures that one node has `nr_inactive_file` and `nr_inactive_file`.
  EXPECT_THAT(content, ContainsRegex("(\n|^)Node [0-9]+, zone +[a-zA-Z]+\n"
                                     "  per-node stats\n"
                                     "( {6}.*\n)*"
                                     "      nr_inactive_file [0-9]+\n"
                                     "      nr_active_file [0-9]+\n"));
  // Ensures that one node has `free`, `min`, `low`, `high`, `present`, among others.
  EXPECT_THAT(content, ContainsRegex("(\n|^)Node [0-9]+, zone +[a-zA-Z]+\n"
                                     "(  .*\n)*"
                                     "  pages free     [0-9]+\n"
                                     "(    .*\n)*"
                                     "        min      [0-9]+\n"
                                     "(    .*\n)*"
                                     "        low      [0-9]+\n"
                                     "(    .*\n)*"
                                     "        high     [0-9]+\n"
                                     "(    .*\n)*"
                                     "        present  [0-9]+\n"
                                     "(    .*\n)*"
                                     "  pagesets\n"));
}

// Verify that /proc/vmstat shape is reasonable.
TEST_F(ProcfsTest, VmStatFile) {
  auto path = "/proc/vmstat";
  EXPECT_EQ(0, access(path, R_OK));
  std::string content;
  ASSERT_TRUE(files::ReadFileToString(path, &content));
  // Ensures that one node has `nr_inactive_file` and `nr_inactive_file`.
  EXPECT_THAT(content, ContainsRegex("(\n|^)workingset_refault_file [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)nr_inactive_file [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)nr_active_file [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)pgscan_direct [0-9]+\n"));
  EXPECT_THAT(content, ContainsRegex("(\n|^)pgscan_kswapd [0-9]+\n"));
}

}  // namespace
