// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/selinux/userspace/util.h"

#include <fcntl.h>
#include <lib/fit/defer.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/xattr.h>

#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/lib/files/file_descriptor.h"

namespace {
constexpr char kProcSelfAttrPath[] = "/proc/self/attr/";
}

fit::result<int> WriteExistingFile(const std::string& path, std::string_view data) {
  auto fd = fbl::unique_fd(open(path.c_str(), O_WRONLY | O_TRUNC, 0777));
  if (!fd.is_valid()) {
    return fit::error(errno);
  }
  if (!fxl::WriteFileDescriptor(fd.get(), data.data(), data.size())) {
    return fit::error(errno);
  }
  return fit::ok();
}

void LoadPolicy(const std::string& name) {
  auto fd = fbl::unique_fd(open(("data/policies/" + name).c_str(), O_RDONLY));
  ASSERT_THAT(fd.get(), SyscallSucceeds()) << "opening policy at: " << name;

  off_t fsize;
  ASSERT_THAT((fsize = lseek(fd.get(), 0, SEEK_END)), SyscallSucceeds());

  void* address = mmap(NULL, (size_t)fsize, PROT_READ, MAP_PRIVATE, fd.get(), 0);
  ASSERT_THAT(reinterpret_cast<intptr_t>(address), SyscallSucceeds());
  auto unmap =
      fit::defer([address, fsize] { ASSERT_THAT(munmap(address, fsize), SyscallSucceeds()); });

  ASSERT_TRUE(WriteExistingFile("/sys/fs/selinux/load",
                                std::string(reinterpret_cast<char*>(address), fsize))
                  .is_ok());
}

fit::result<int, std::string> ReadFile(const std::string& path) {
  std::string result;
  if (files::ReadFileToString(path, &result)) {
    return fit::ok(std::move(result));
  }
  return fit::error(errno);
}

std::string RemoveTrailingNul(std::string in) {
  if (in.size() > 0 && in[in.size() - 1] == 0) {
    in.pop_back();
  }
  return in;
}

fit::result<int, std::string> ReadTaskAttr(std::string_view attr_name) {
  std::string attr_path(kProcSelfAttrPath);
  attr_path.append(attr_name);

  auto attr = ReadFile(attr_path);
  if (attr.is_error()) {
    return attr;
  }
  return fit::ok(RemoveTrailingNul(attr.value()));
}

fit::result<int> WriteTaskAttr(std::string_view attr_name, std::string_view context) {
  std::string attr_path(kProcSelfAttrPath);
  attr_path.append(attr_name);

  return WriteExistingFile(attr_path, context);
}

ScopedTaskAttrResetter ScopedTaskAttrResetter::SetTaskAttr(std::string_view attr_name,
                                                           std::string_view context) {
  return ScopedTaskAttrResetter(attr_name, context);
}

ScopedTaskAttrResetter::ScopedTaskAttrResetter(std::string_view attr_name,
                                               std::string_view context) {
  EXPECT_EQ(WriteTaskAttr(attr_name, context), fit::ok());
  attr_name_ = std::string(attr_name);
  context_ = std::string(context);
}

ScopedTaskAttrResetter::~ScopedTaskAttrResetter() {
  EXPECT_EQ(WriteTaskAttr(attr_name_, "\n"), fit::ok());
}

fit::result<int, std::string> GetLabel(int fd) {
  char buf[256];
  ssize_t result = fgetxattr(fd, "security.selinux", buf, sizeof(buf));
  if (result < 0) {
    return fit::error(errno);
  }
  // Use `RemoveTrailingNul` to strip off the trailing NUL if present.
  return fit::ok(RemoveTrailingNul(std::string(buf, result)));
}

fit::result<int, std::string> GetLabel(const std::string& path) {
  char buf[256];
  ssize_t result = getxattr(path.c_str(), "security.selinux", buf, sizeof(buf));
  if (result < 0) {
    return fit::error(errno);
  }
  // Use `RemoveTrailingNul` to strip off the trailing NUL if present.
  return fit::ok(RemoveTrailingNul(std::string(buf, result)));
}

ScopedEnforcement ScopedEnforcement::SetEnforcing() {
  return ScopedEnforcement(/*enforcing=*/true);
}

ScopedEnforcement ScopedEnforcement::SetPermissive() {
  return ScopedEnforcement(/*enforcing=*/false);
}

ScopedEnforcement::ScopedEnforcement(bool enforcing) {
  EXPECT_TRUE(files::ReadFileToString("/sys/fs/selinux/enforce", &previous_state_));
  std::string new_state = enforcing ? "1" : "0";
  EXPECT_TRUE(files::WriteFile("/sys/fs/selinux/enforce", new_state));
}

ScopedEnforcement::~ScopedEnforcement() {
  EXPECT_TRUE(files::WriteFile("/sys/fs/selinux/enforce", previous_state_));
}
