// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_STARNIX_TESTS_SYSCALLS_CPP_TEST_HELPER_H_
#define SRC_STARNIX_TESTS_SYSCALLS_CPP_TEST_HELPER_H_

#include <lib/fit/result.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/uio.h>
#include <unistd.h>

#include <functional>
#include <optional>
#include <string_view>
#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/genetlink.h>
#include <linux/netlink.h>
#include <linux/taskstats.h>

#include "capabilities_helper.h"
#include "gtest/gtest.h"
#include "syscall_matchers.h"

#define SAFE_SYSCALL(X)                                                             \
  ({                                                                                \
    auto retval = (X);                                                              \
    if (retval < 0) {                                                               \
      ADD_FAILURE() << #X << " failed: " << strerror(errno) << "(" << errno << ")"; \
      retval = {};                                                                  \
    }                                                                               \
    retval;                                                                         \
  })

// TODO(https://fxbug.dev/317285180) don't skip on baseline
#define SAFE_SYSCALL_SKIP_ON_EPERM(X)                                          \
  ({                                                                           \
    auto retval = (X);                                                         \
    if (retval < 0) {                                                          \
      if (errno == EPERM) {                                                    \
        GTEST_SKIP() << "Permission denied for " << #X << ", skipping tests."; \
      } else {                                                                 \
        FAIL() << #X << " failed: " << strerror(errno) << "(" << errno << ")"; \
      }                                                                        \
    }                                                                          \
    retval;                                                                    \
  })

#define ASSERT_RESULT_SUCCESS_AND_RETURN(S)          \
  ({                                                 \
    auto retval = (S);                               \
    ASSERT_TRUE(retval.is_ok()) << #S << " failed."; \
    std::move(retval.value());                       \
  })

namespace test_helper {

// Helper class to handle test that needs to fork and do assertion on the child
// process.
class ForkHelper {
 public:
  ForkHelper();
  ~ForkHelper();

  // Wait for all children of the current process, and return true if all exited
  // with a 0 status or with the expected signal.
  testing::AssertionResult WaitForChildren();

  // For the current process and execute the given |action| inside the child,
  // then exit with a status equals to the number of failed expectation and
  // assertion. Return immediately with the pid of the child.
  pid_t RunInForkedProcess(std::function<void()> action);

  // If called, checks for process termination by the given signal, instead of expecting
  // the forked process to terminate normally.
  void ExpectSignal(int signum);

  // If called, only waits for the children explicitly forked by the ForkHelper, instead
  // of waiting for all children.
  void OnlyWaitForForkedChildren();

 private:
  std::vector<pid_t> child_pids_;
  bool wait_for_all_children_;
  int death_signum_;

  ::testing::AssertionResult WaitForChildrenInternal(int death_signum);
};

// Helper class to handle tests that needs to clone processes.
class CloneHelper {
 public:
  CloneHelper();
  ~CloneHelper();

  // Call clone with the specified childFunction and cloneFlags.
  // Perform the necessary asserts to ensure the clone was performed with
  // no errors and return the new process ID.
  int runInClonedChild(unsigned int cloneFlags, int (*childFunction)(void *));

  // Handy trivial function for passing clone when we want the child to
  // sleep for 1 second and return 0.
  static int sleep_1sec(void *);

  // Handy trivial function for passing clone when we want the child to
  // do nothing and return 0.
  static int doNothing(void *);

 private:
  uint8_t *_childStack;
  uint8_t *_childStackBegin;
  static constexpr size_t _childStackSize = 0x5000;
};

// Helper class to modify signal masks of processes
class SignalMaskHelper {
 public:
  // Blocks the specified signal and saves the original signal mask
  // to _sigmaskCopy.
  void blockSignal(int signal);

  // Blocks the execution until the specified signal is received.
  void waitForSignal(int signal);

  // Blocks the execution until the specified signal is received or timed out.
  int timedWaitForSignal(int signal, time_t sec);

  // Sets the signal mask of the process with _sigmaskCopy.
  void restoreSigmask();

 private:
  sigset_t _sigset;
  sigset_t _sigmaskCopy;
};

class ScopedTempFD {
 public:
  ScopedTempFD();
  ~ScopedTempFD() { unlink(name_.c_str()); }

  bool is_valid() const { return fd_.is_valid(); }
  explicit operator bool() const { return is_valid(); }

  const std::string &name() const { return name_; }
  int fd() const { return fd_.get(); }

 public:
  std::string name_;
  fbl::unique_fd fd_;
};

class ScopedTempDir {
 public:
  ScopedTempDir();
  ~ScopedTempDir();

  const std::string &path() const { return path_; }

 private:
  std::string path_;
};

class ScopedTempSymlink {
 public:
  explicit ScopedTempSymlink(const char *target_path);
  ~ScopedTempSymlink();

  bool is_valid() const { return !path_.empty(); }
  explicit operator bool() const { return is_valid(); }

  const std::string &path() const { return path_; }

 private:
  std::string path_;
};

#define HANDLE_EINTR(x)                                     \
  ({                                                        \
    decltype(x) eintr_wrapper_result;                       \
    do {                                                    \
      eintr_wrapper_result = (x);                           \
    } while (eintr_wrapper_result == -1 && errno == EINTR); \
    eintr_wrapper_result;                                   \
  })

void waitForChildSucceeds(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                          int (*parentRunFunction)(void *));

void waitForChildFails(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                       int (*parentRunFunction)(void *));

std::string get_tmp_path();

struct MemoryMapping {
  uintptr_t start;
  uintptr_t end;
  std::string perms;
  size_t offset;
  std::string device;
  size_t inode;
  std::string pathname;
};

// Encoder for serializing netlink messages
class NetlinkEncoder {
 public:
  // Writes a value to the buffer at the current offset
  template <typename T>
  void Write(const T &value) {
    Write(&value, sizeof(T));
  }

  // Writes a value to the buffer at a specified offset
  template <typename T>
  void Write(T &value, size_t offset) {
    Write(&value, offset, sizeof(T));
  }

  // Reads a value from the buffer at a specified offset
  template <typename T>
  void Read(T &out, size_t offset) {
    Read(&out, offset, sizeof(T));
  }

  NetlinkEncoder(__u16 type, __u16 flags) { StartMessage(type, flags); }

  // Starts a new netlink message
  void StartMessage(__u16 type, __u16 flags) {
    nlmsghdr header = {};
    header.nlmsg_type = type;
    // Length is encoded when the message is finalized
    header.nlmsg_len = sizeof(nlmsghdr);
    header.nlmsg_flags = flags;
    header.nlmsg_pid = getpid();
    header.nlmsg_seq = sequence_;
    netlink_header = offset_;
    Write(header);
  }

  // Begins a genetlink message
  void BeginGenetlinkHeader(__u8 cmd) {
    genlmsghdr hdr = {};
    hdr.cmd = cmd;
    hdr.version = 1;
    genetlink_header = offset_;
    Write(hdr);
  }

  // Starts encoding an NLA
  void BeginNla(__u16 type) {
    nlattr attr = {};
    attr.nla_type = type;
    // Updated when the NLA is finalized
    attr.nla_len = 0;
    nla_start = offset_;
    Write(attr);
  }

  // Finishes encoding an NLA
  void EndNla() {
    nlattr attr;
    Read(attr, nla_start);
    attr.nla_len += static_cast<__u16>(offset_ - nla_start);
    Write(attr, nla_start);
  }

  // Finalizes the message, allowing it to be sent using sendmsg.
  void Finalize(iovec &out) {
    nlmsghdr hdr;
    Read(hdr, netlink_header);
    out.iov_base = data_.data() + netlink_header;
    hdr.nlmsg_len += offset_ - genetlink_header;
    out.iov_len = hdr.nlmsg_len;
    sequence_++;
    Write(hdr, netlink_header);
  }

  // Clears the buffer, invalidating any iovecs that were
  // obtained from this encoder.
  void Clear() { offset_ = 0; }

 private:
  void Write(const void *data, size_t len) {
    data_.resize(data_.size() + len);
    memcpy(data_.data() + offset_, data, len);
    offset_ += len;
  }
  void Read(void *data, size_t offset, size_t len) { memcpy(data, data_.data() + offset, len); }
  void Write(const void *data, size_t offset, size_t len) {
    memcpy(data_.data() + offset, data, len);
  }
  __u32 sequence_ = 0;
  size_t offset_ = 0;
  size_t nla_start;
  size_t netlink_header;
  size_t genetlink_header;
  std::vector<uint8_t> data_;
};

// A RRAI classes than handles a memory mapping. The container will ensure the
// mapping is destroyed when the object is deleted.
class ScopedMMap {
 public:
  static fit::result<int, ScopedMMap> MMap(void *addr, size_t length, int prot, int flags, int fd,
                                           off_t offset) {
    void *mapping = mmap(addr, length, prot, flags, fd, offset);
    if (mapping == MAP_FAILED) {
      int error = errno;
      return fit::error(error);
    }
    return fit::ok(ScopedMMap(mapping, length));
  }

  ScopedMMap(const ScopedMMap &) = delete;
  ScopedMMap &operator=(const ScopedMMap &) = delete;

  ScopedMMap(ScopedMMap &&other) noexcept : mapping_(MAP_FAILED), length_(0) {
    *this = std::move(other);
  }

  ~ScopedMMap() { Unmap(); }

  ScopedMMap &operator=(ScopedMMap &&other) noexcept {
    Unmap();
    mapping_ = other.mapping_;
    length_ = other.length_;
    other.mapping_ = MAP_FAILED;
    return *this;
  }

  void Unmap() {
    if (is_valid()) {
      munmap(mapping_, length_);
      mapping_ = MAP_FAILED;
    }
  }

  bool is_valid() const { return mapping_ != MAP_FAILED; }

  explicit operator bool() const { return is_valid(); }

  void *mapping() const { return mapping_; }

 private:
  explicit ScopedMMap(void *mapping, size_t length) : mapping_(mapping), length_(length) {}

  void *mapping_;
  size_t length_;
};

// Returns the first memory mapping that matches the given predicate.
std::optional<MemoryMapping> find_memory_mapping(std::function<bool(const MemoryMapping &)> match,
                                                 std::string_view maps);

std::optional<MemoryMapping> find_memory_mapping(uintptr_t addr, std::string_view maps);

// Returns a random hex string of the given length.
std::string RandomHexString(size_t length);

// Returns true if running with sysadmin capabilities.
bool HasSysAdmin();

// Returns true if running with the given capability.
bool HasCapability(uint32_t cap);

// Returns true if running on Starnix.  This is likely only necessary when there are known bugs.
bool IsStarnix();

// Returns true if reported kernel version is equal or greater than a given one.
bool IsKernelVersionAtLeast(int min_major, int min_minor);

/// Unmount anything mounted at or under `path` and remove it.
void RecursiveUnmountAndRemove(const std::string &path);

// Attempts to read a byte from the given memory address.
// Returns whether the read succeeded or not.
bool TryRead(uintptr_t addr);

// Attempts to write a zero byte to the given memory address.
// Returns whether the write succeeded or not.
bool TryWrite(uintptr_t addr);

// Wrapper for the memfd_create system call.
int MemFdCreate(const char *name, unsigned int flags);

void WaitUntilBlocked(pid_t target, bool ignore_tracer);

enum AccessType { Read, Write };

// Checks whether the provided access segfaults.
testing::AssertionResult TestThatAccessSegfaults(void *test_address, AccessType type);

}  // namespace test_helper

#endif  // SRC_STARNIX_TESTS_SYSCALLS_CPP_TEST_HELPER_H_
