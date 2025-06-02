// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/selinux/userspace/util.h"

#include <fcntl.h>
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

std::string RemoveTrailingNul(std::string in) {
  if (in.size() > 0 && in[in.size() - 1] == 0) {
    in.pop_back();
  }
  return in;
}

fit::result<int, std::string> ReadFile(const std::string& path) {
  std::string result;
  if (files::ReadFileToString(path, &result)) {
    return fit::ok(std::move(result));
  }
  return fit::error(errno);
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
                                                           std::string_view new_value) {
  auto old_value = ReadTaskAttr(attr_name);
  if (old_value.is_error()) {
    ADD_FAILURE() << "Saving task attr " << attr_name
                  << " error:" << strerror(old_value.error_value());
    return ScopedTaskAttrResetter("", "");
  }
  auto write_result = WriteTaskAttr(attr_name, new_value);
  if (write_result.is_error()) {
    ADD_FAILURE() << "Setting attr " << attr_name << " to \"" << new_value
                  << "\" error:" << strerror(old_value.error_value());
    return ScopedTaskAttrResetter("", "");
  }
  return ScopedTaskAttrResetter(attr_name, old_value.value());
}

ScopedTaskAttrResetter::ScopedTaskAttrResetter(std::string_view attr_name,
                                               std::string_view old_value) {
  attr_name_ = std::string(attr_name);
  old_value_ = std::string(old_value);
}

ScopedTaskAttrResetter::~ScopedTaskAttrResetter() {
  if (attr_name_ == "") {
    return;
  }
  auto to_write = old_value_.empty() ? std::string(1, 0) : old_value_;
  auto result = WriteTaskAttr(attr_name_, to_write);
  if (result.is_error()) {
    ADD_FAILURE() << "Restoring task attr " << attr_name_ << " to \"" << old_value_ << "\"";
  }
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

fit::result<int, bool> IsSelinuxNullInode(int fd) {
  struct stat null_file_info;
  if (stat("/sys/fs/selinux/null", &null_file_info) < 0) {
    return fit::error(errno);
  }
  struct stat fd_info;
  if (fstat(fd, &fd_info) < 0) {
    return fit::error(errno);
  }
  return fit::ok(null_file_info.st_dev == fd_info.st_dev &&
                 null_file_info.st_ino == fd_info.st_ino);
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
