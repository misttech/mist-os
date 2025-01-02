// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#include <dirent.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <fstream>
#include <optional>
#include <string_view>

#include <gtest/gtest.h>
#include <linux/capability.h>

#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/string_number_conversions.h"

namespace test_helper {

::testing::AssertionResult ForkHelper::WaitForChildrenInternal(int death_signum) {
  ::testing::AssertionResult result = ::testing::AssertionSuccess();
  while (wait_for_all_children_ || !child_pids_.empty()) {
    int wstatus;
    pid_t pid;
    if ((pid = wait(&wstatus)) == -1) {
      if (errno == EINTR) {
        continue;
      }
      if (errno == ECHILD) {
        // No more children, reaping is done.
        return result;
      }
      // Another error is unexpected.
      result = ::testing::AssertionFailure()
               << "wait error: " << strerror(errno) << "(" << errno << ")";
    }
    bool check_result = wait_for_all_children_;
    if (!check_result) {
      auto it = std::find(child_pids_.begin(), child_pids_.end(), pid);
      if (it != child_pids_.end()) {
        child_pids_.erase(it);
        check_result = true;
      }
    }

    if (check_result) {
      if (death_signum == 0) {
        if (!WIFEXITED(wstatus) || WEXITSTATUS(wstatus) != 0) {
          result = ::testing::AssertionFailure()
                   << "wait_status: WIFEXITED(wstatus) = " << WIFEXITED(wstatus)
                   << ", WEXITSTATUS(wstatus) = " << WEXITSTATUS(wstatus)
                   << ", WTERMSIG(wstatus) = " << WTERMSIG(wstatus);
        }
      } else {
        if (!WIFSIGNALED(wstatus) || WTERMSIG(wstatus) != death_signum) {
          result = ::testing::AssertionFailure()
                   << "wait_status: WIFSIGNALED(wstatus) = " << WIFSIGNALED(wstatus)
                   << ", WEXITSTATUS(wstatus) = " << WEXITSTATUS(wstatus)
                   << ", WTERMSIG(wstatus) = " << WTERMSIG(wstatus);
        }
      }
    }
  }
  return result;
}

ForkHelper::ForkHelper() : wait_for_all_children_(true), death_signum_(0) {
  // Ensure that all children will ends up being parented to the process that
  // created the helper.
  prctl(PR_SET_CHILD_SUBREAPER, 1);
}

ForkHelper::~ForkHelper() {
  // Wait for all remaining children, and ensure none failed.
  EXPECT_TRUE(WaitForChildrenInternal(death_signum_)) << ": at least a child had a failure";
}

void ForkHelper::OnlyWaitForForkedChildren() { wait_for_all_children_ = false; }

void ForkHelper::ExpectSignal(int signum) { death_signum_ = signum; }

testing::AssertionResult ForkHelper::WaitForChildren() {
  return WaitForChildrenInternal(death_signum_);
}

pid_t ForkHelper::RunInForkedProcess(std::function<void()> action) {
  pid_t pid = SAFE_SYSCALL(fork());
  if (pid != 0) {
    child_pids_.push_back(pid);
    return pid;
  }
  action();
  _exit(testing::Test::HasFailure());
}

CloneHelper::CloneHelper() {
  // Stack setup
  this->_childStack = (uint8_t *)mmap(NULL, CloneHelper::_childStackSize, PROT_WRITE | PROT_READ,
                                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (this->_childStack == MAP_FAILED) {
    std::cerr << "CloneHelper mmap failed, errno was set to '" << strerror(errno) << "' (" << errno
              << ").\n";
    assert(false);
  }
  this->_childStackBegin = this->_childStack + CloneHelper::_childStackSize;
}

CloneHelper::~CloneHelper() { munmap(this->_childStack, CloneHelper::_childStackSize); }

int CloneHelper::runInClonedChild(unsigned int cloneFlags, int (*childFunction)(void *)) {
  int childPid = clone(childFunction, this->_childStackBegin, cloneFlags, NULL);
  assert(errno == 0);
  assert(childPid != -1);
  return childPid;
}

int CloneHelper::sleep_1sec(void *) {
  struct timespec res;
  res.tv_sec = 1;
  res.tv_nsec = 0;
  clock_nanosleep(CLOCK_MONOTONIC, 0, &res, &res);
  return 0;
}

int CloneHelper::doNothing(void *) { return 0; }

void SignalMaskHelper::blockSignal(int signal) {
  sigemptyset(&this->_sigset);
  sigaddset(&this->_sigset, signal);
  sigprocmask(SIG_BLOCK, &this->_sigset, &this->_sigmaskCopy);
}

void SignalMaskHelper::waitForSignal(int signal) {
  int sig;
  int result = sigwait(&this->_sigset, &sig);
  ASSERT_EQ(result, 0);
  ASSERT_EQ(sig, signal);
}

int SignalMaskHelper::timedWaitForSignal(int signal, time_t sec) {
  siginfo_t siginfo;
  struct timespec ts;
  ts.tv_sec = sec;
  ts.tv_nsec = 0;
  return sigtimedwait(&this->_sigset, &siginfo, &ts);
}

void SignalMaskHelper::restoreSigmask() { sigprocmask(SIG_SETMASK, &this->_sigmaskCopy, NULL); }

ScopedTempFD::ScopedTempFD() : name_("/tmp/proc_test_file_XXXXXX") {
  char *mut_name = const_cast<char *>(name_.c_str());
  fd_ = fbl::unique_fd(mkstemp(mut_name));
}

ScopedTempDir::ScopedTempDir() {
  path_ = get_tmp_path() + "/testdirXXXXXX";
  if (!mkdtemp(path_.data())) {
    path_.clear();
  }
}

ScopedTempDir::~ScopedTempDir() {
  if (!path_.empty()) {
    RecursiveUnmountAndRemove(path_);
  }
}

ScopedTempSymlink::ScopedTempSymlink(const char *target_path) {
  std::string prefix = "/tmp/syscall_test_symlink_";
  size_t retries = 100;
  while (retries--) {
    std::string path = prefix + RandomHexString(6);
    if (symlink(target_path, path.c_str()) == 0) {
      path_ = path;
      return;
    }
  }
  path_.clear();
}

ScopedTempSymlink::~ScopedTempSymlink() {
  if (!path_.empty()) {
    unlink(path_.c_str());
  }
}

void waitForChildSucceeds(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                          int (*parentRunFunction)(void *)) {
  CloneHelper cloneHelper;
  int expectedWaitPid = cloneHelper.runInClonedChild(cloneFlags, childRunFunction);

  parentRunFunction(NULL);

  int expectedWaitStatus = 0;
  int expectedErrno = 0;
  int actualWaitStatus;
  int actualWaitPid = waitpid(expectedWaitPid, &actualWaitStatus, waitFlag);
  EXPECT_EQ(actualWaitPid, expectedWaitPid);
  EXPECT_EQ(actualWaitStatus, expectedWaitStatus);
  EXPECT_EQ(errno, expectedErrno);
}

void waitForChildFails(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                       int (*parentRunFunction)(void *)) {
  CloneHelper cloneHelper;
  int pid = cloneHelper.runInClonedChild(cloneFlags, childRunFunction);

  parentRunFunction(NULL);

  int expectedWaitPid = -1;
  int actualWaitPid = waitpid(pid, NULL, waitFlag);
  EXPECT_EQ(actualWaitPid, expectedWaitPid);
  EXPECT_EQ(errno, ECHILD);
  errno = 0;
}

std::string get_tmp_path() {
  static std::string tmp_path = [] {
    const char *tmp = getenv("TEST_TMPDIR");
    if (tmp == nullptr)
      tmp = "/tmp";
    return tmp;
  }();
  return tmp_path;
}

namespace {
std::optional<MemoryMapping> parse_mapping_entry(std::string_view line) {
  // format:
  // start-end perms offset device inode path
  std::vector<std::string_view> parts =
      fxl::SplitString(line, " ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  if (parts.size() < 5) {
    return std::nullopt;
  }
  std::vector<std::string_view> addrs =
      fxl::SplitString(parts[0], "-", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  if (addrs.size() != 2) {
    return std::nullopt;
  }

  uintptr_t start;
  uintptr_t end;

  if (!fxl::StringToNumberWithError(addrs[0], &start, fxl::Base::k16) ||
      !fxl::StringToNumberWithError(addrs[1], &end, fxl::Base::k16)) {
    return std::nullopt;
  }

  size_t offset;
  size_t inode;
  if (!fxl::StringToNumberWithError(parts[2], &offset, fxl::Base::k16) ||
      !fxl::StringToNumberWithError(parts[4], &inode, fxl::Base::k10)) {
    return std::nullopt;
  }

  std::string pathname;
  if (parts.size() > 5) {
    // The pathname always starts at pos 73.
    pathname = line.substr(73);
  }

  return MemoryMapping{
      start, end, std::string(parts[1]), offset, std::string(parts[3]), inode, pathname,
  };
}

std::optional<size_t> parse_field_in_kb(std::string_view value) {
  const std::string_view suffix = " kB";
  if (!value.ends_with(suffix)) {
    return std::nullopt;
  }

  value.remove_suffix(suffix.length());
  size_t result;
  if (!fxl::StringToNumberWithError(value, &result, fxl::Base::k10)) {
    return std::nullopt;
  }
  return result;
}
}  // namespace

std::optional<MemoryMapping> find_memory_mapping(std::function<bool(const MemoryMapping &)> match,
                                                 std::string_view maps) {
  std::vector<std::string_view> lines =
      fxl::SplitString(maps, "\n", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  for (auto line : lines) {
    std::optional<MemoryMapping> mapping = parse_mapping_entry(line);
    if (!mapping) {
      return std::nullopt;
    }

    if (match(*mapping)) {
      return mapping;
    }
  }
  return std::nullopt;
}

std::optional<MemoryMapping> find_memory_mapping(uintptr_t addr, std::string_view maps) {
  return find_memory_mapping(
      [addr](const MemoryMapping &mapping) { return mapping.start <= addr && addr < mapping.end; },
      maps);
}

std::optional<MemoryMappingExt> find_memory_mapping_ext(
    std::function<bool(const MemoryMappingExt &)> match, std::string_view maps) {
  std::vector<std::string_view> lines =
      fxl::SplitString(maps, "\n", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  std::optional<MemoryMappingExt> current_mapping;
  for (auto line : lines) {
    std::optional<MemoryMapping> maybe_new_mapping = parse_mapping_entry(line);
    if (maybe_new_mapping) {
      if (current_mapping && match(*current_mapping)) {
        return current_mapping;
      }

      current_mapping = *maybe_new_mapping;
      continue;
    }
    std::vector<std::string_view> fields =
        fxl::SplitString(line, ":", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    if (fields.size() != 2) {
      return std::nullopt;
    }
    if (fields[0] == "Rss") {
      if (std::optional<size_t> rss = parse_field_in_kb(fields[1])) {
        current_mapping->rss = *rss;
      } else {
        return std::nullopt;
      }
    }
    if (fields[0] == "VmFlags") {
      current_mapping->vm_flags =
          fxl::SplitStringCopy(fields[1], " ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    }
  }
  if (current_mapping && match(*current_mapping)) {
    return current_mapping;
  }
  return std::nullopt;
}

std::optional<MemoryMappingExt> find_memory_mapping_ext(uintptr_t addr, std::string_view maps) {
  return find_memory_mapping_ext(
      [addr](const MemoryMappingExt &mapping) {
        return mapping.start <= addr && addr < mapping.end;
      },
      maps);
}

std::string RandomHexString(size_t length) {
  constexpr char kHexCharacters[] = "0123456789ABCDEF";
  constexpr size_t kRadix = sizeof(kHexCharacters) - 1;

  std::string value(length, '\0');
  for (size_t i = 0; i < length; ++i) {
    value[i] = kHexCharacters[random() % kRadix];
  }

  return value;
}

bool HasSysAdmin() { return HasCapability(CAP_SYS_ADMIN); }

bool IsStarnix() {
  struct utsname buf;
  return uname(&buf) == 0 && strstr(buf.release, "starnix") != nullptr;
}

bool IsKernelVersionAtLeast(int min_major, int min_minor) {
  struct utsname buf;
  int major, minor;
  if (uname(&buf) != 0) {
    return false;
  }
  if (sscanf(buf.release, "%d.%d:", &major, &minor) != 2) {
    return false;
  }
  return major > min_major || (major == min_major && minor >= min_minor);
}

void RecursiveUnmountAndRemove(const std::string &path) {
  if (HasSysAdmin()) {
    // Repeatedly call umount to handle shadowed mounts properly.
    do {
      errno = 0;
      ASSERT_THAT(umount2(path.c_str(), MNT_DETACH),
                  AnyOf(SyscallSucceeds(), SyscallFailsWithErrno(EINVAL)))
          << path;
    } while (errno != EINVAL);
  }

  int dir_fd = open(path.c_str(), O_DIRECTORY | O_NOFOLLOW);
  if (dir_fd >= 0) {
    DIR *dir = fdopendir(dir_fd);
    EXPECT_NE(dir, nullptr) << "fdopendir: " << std::strerror(errno);
    while (struct dirent *entry = readdir(dir)) {
      std::string name(entry->d_name);
      if (name == "." || name == "..")
        continue;
      std::string subpath = std::string(path) + "/" + name;
      if (entry->d_type == DT_DIR) {
        RecursiveUnmountAndRemove(subpath);
      } else {
        EXPECT_THAT(unlink(subpath.c_str()), SyscallSucceeds()) << subpath;
      }
    }
    EXPECT_EQ(closedir(dir), 0) << "closedir: " << std::strerror(errno);
  }

  EXPECT_THAT(rmdir(path.c_str()), SyscallSucceeds());
}

int MemFdCreate(const char *name, unsigned int flags) {
  return static_cast<int>(syscall(SYS_memfd_create, name, flags));
}

// Attempts to read a byte from the given memory address.
// Returns whether the read succeeded or not.
bool TryRead(uintptr_t addr) {
  fbl::unique_fd mem_fd(MemFdCreate("try_read", O_WRONLY));
  EXPECT_TRUE(mem_fd.is_valid());

  return write(mem_fd.get(), reinterpret_cast<void *>(addr), 1) == 1;
}

// Attempts to write a zero byte to the given memory address.
// Returns whether the write succeeded or not.
bool TryWrite(uintptr_t addr) {
  fbl::unique_fd zero_fd(open("/dev/zero", O_RDONLY));
  EXPECT_TRUE(zero_fd.is_valid());

  return read(zero_fd.get(), reinterpret_cast<void *>(addr), 1) == 1;
}

// Loop until the target process indicates a sleeping state in /proc/pid/stat.
void WaitUntilBlocked(pid_t target, bool ignore_tracer) {
  for (int i = 0; i < 100000; i++) {
    // Loop until the target task is paused.
    std::string fname = "/proc/" + std::to_string(target) + "/stat";
    std::ifstream t(fname);
    if (!t.is_open()) {
      FAIL() << "File " << fname << " not found";
    }
    std::stringstream buffer;
    buffer << t.rdbuf();
    if (buffer.str().find("S") != std::string::npos ||
        (!ignore_tracer && buffer.str().find("t") != std::string::npos)) {
      break;
    }
    // Give up if we don't seem to be getting to sleep.
    if (i == 99999)
      FAIL() << "Failed to wait for pid " << target
             << " to block. resulting status: " << buffer.str();
  }
}

// This variable is accessed from within a signal handler and thus must be declared volatile.
static volatile void *expected_fault_address;

testing::AssertionResult TestThatAccessSegfaults(void *test_address, AccessType type) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([test_address, type] {
    struct sigaction action;
    action.sa_sigaction = [](int signo, siginfo_t *info, void *ucontext) {
      if (signo == SIGSEGV && info->si_addr == expected_fault_address) {
        _exit(EXIT_SUCCESS);
      } else {
        _exit(EXIT_FAILURE);
      }
    };
    action.sa_flags = SA_SIGINFO;
    SAFE_SYSCALL(sigaction(SIGSEGV, &action, nullptr));
    expected_fault_address = test_address;
    if (type == AccessType::Read) {
      *static_cast<volatile std::byte *>(test_address);
    } else {
      *static_cast<volatile std::byte *>(test_address) = std::byte{};
    }
    FAIL() << "Must have observed segfault after access.";
  });
  return helper.WaitForChildren();
}

}  // namespace test_helper
