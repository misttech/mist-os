// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/starnix/tests/selinux/userspace/util.h"

extern std::string DoPrePolicyLoadWork() { return "minimal_policy.pp"; }

namespace {

// Attempting to read the process' current context should return a value.
TEST(ProcAttrTest, Current) {
  EXPECT_THAT(ReadTaskAttr("current"), IsOk("system_u:unconfined_r:unconfined_t:s0"));
}

TEST(ProcAttrTest, AttrsAreSeekable) {
  auto current_attr = fbl::unique_fd(open("/proc/self/attr/current", O_RDONLY));
  ASSERT_TRUE(current_attr.is_valid()) << strerror(errno);

  // Seek position is retained, so it is possible to read the value in chunks.
  std::string context;
  for (;;) {
    char buf[2];
    auto result = read(current_attr.get(), buf, sizeof(buf));
    ASSERT_GE(result, 0) << strerror(errno);
    if (result == 0) {
      break;
    }
    context.append(buf, result);
  }

  EXPECT_EQ(RemoveTrailingNul(context), "system_u:unconfined_r:unconfined_t:s0");

  // It is possible to seek back to the start of the file and read it all again.
  auto result = lseek(current_attr.get(), 0, SEEK_SET);
  ASSERT_EQ(result, 0) << strerror(errno);
  ASSERT_TRUE(files::ReadFileDescriptorToString(current_attr.get(), &context));
  EXPECT_EQ(RemoveTrailingNul(context), "system_u:unconfined_r:unconfined_t:s0");

  // It is possible to seek into the middle and read from there.
  result = lseek(current_attr.get(), 9, SEEK_SET);
  ASSERT_EQ(result, 9) << strerror(errno);
  ASSERT_TRUE(files::ReadFileDescriptorToString(current_attr.get(), &context));
  EXPECT_EQ(RemoveTrailingNul(context), "unconfined_r:unconfined_t:s0");
}

// Writable attributes validate the contexts written to them.
TEST(ProcAttrTest, WritableAttrsValidateContexts) {
  // Write a valid context and verify that it was set.
  EXPECT_TRUE(WriteTaskAttr("exec", "system_u:unconfined_r:unconfined_t:s0").is_ok());
  EXPECT_EQ(ReadTaskAttr("exec"), fit::ok("system_u:unconfined_r:unconfined_t:s0"));

  // Write an invalid context and verify that nothing is changed.
  EXPECT_EQ(WriteTaskAttr("exec", "system_u:invalid_role_r:unconfined_t:s0"), fit::error(EINVAL));
  EXPECT_EQ(ReadTaskAttr("exec"), fit::ok("system_u:unconfined_r:unconfined_t:s0"));
}

// Writing a single NUL clears the attribute.
TEST(ProcAttrTest, WritableAttrsClearedByNul) {
  // Set a valid context, then clear it with NUL.
  ASSERT_TRUE(WriteTaskAttr("exec", "system_u:unconfined_r:unconfined_t:s0").is_ok());
  ASSERT_EQ(ReadTaskAttr("exec"), fit::ok("system_u:unconfined_r:unconfined_t:s0"));
  EXPECT_EQ(WriteTaskAttr("exec", std::string_view("\0", 1)), fit::success<>());
  EXPECT_EQ(ReadTaskAttr("exec"), fit::ok(""));
}

// Writing a single newline clears the attribute.
TEST(ProcAttrTest, WritableAttrsClearedByNewline) {
  // Set a valid context, then clear it with newline.
  ASSERT_TRUE(WriteTaskAttr("exec", "system_u:unconfined_r:unconfined_t:s0").is_ok());
  ASSERT_EQ(ReadTaskAttr("exec"), fit::ok("system_u:unconfined_r:unconfined_t:s0"));
  EXPECT_EQ(WriteTaskAttr("exec", "\n"), fit::success<>());
  EXPECT_EQ(ReadTaskAttr("exec"), fit::ok(""));
}

// Writing a valid context across multiple writes is not valid.
TEST(ProcAttrTest, ContextsMustBeWrittenInASingleWrite) {
  auto exec_attr = fbl::unique_fd(open("/proc/self/attr/exec", O_RDWR));
  ASSERT_TRUE(exec_attr.is_valid()) << strerror(errno);

  constexpr char kFirstPart[] = "system_u:unconfined_r";
  constexpr char kSecondPart[] = ":unconfined_t:s0";

  auto result = write(exec_attr.get(), kFirstPart, sizeof(kFirstPart));
  EXPECT_EQ(result, -1);
  EXPECT_EQ(errno, EINVAL);

  result = write(exec_attr.get(), kSecondPart, sizeof(kSecondPart));
  EXPECT_EQ(result, -1);
  EXPECT_EQ(errno, EINVAL);
}

// Writes are accepted, but only if the seek position is zero.
TEST(ProcAttrTest, WritableAttrsCanOnlyBeWrittenAtSeekPositionZero) {
  auto exec_attr = fbl::unique_fd(open("/proc/self/attr/exec", O_RDWR));
  ASSERT_TRUE(exec_attr.is_valid()) << strerror(errno);

  // Seek to a non-zero position.
  ASSERT_EQ(lseek(exec_attr.get(), 1, SEEK_SET), 1) << strerror(errno);

  // Write a valid context into the attribute.
  constexpr char kValidContext[] = "system_u:unconfined_r:unconfined_t:s0";
  auto result = write(exec_attr.get(), kValidContext, sizeof(kValidContext));
  EXPECT_EQ(result, -1);
  EXPECT_EQ(errno, EINVAL) << strerror(errno);
}

// Writes do not affect the seek position.
TEST(ProcAttrTest, WritesDoNotAffectSeekPosition) {
  auto exec_attr = fbl::unique_fd(open("/proc/self/attr/exec", O_RDWR));
  ASSERT_TRUE(exec_attr.is_valid()) << strerror(errno);

  // Write a valid context into the attribute.
  constexpr char kValidContext[] = "system_u:unconfined_r:unconfined_t:s0";
  auto result = write(exec_attr.get(), kValidContext, sizeof(kValidContext));
  ASSERT_EQ(result, (ssize_t)sizeof(kValidContext)) << strerror(errno);

  // Seek to a non-zero position.
  EXPECT_EQ(lseek(exec_attr.get(), 0, SEEK_CUR), 0) << strerror(errno);
}

// Multiple writes are accepted, so long as the contents are valid.
TEST(ProcAttrTest, WritableAttrsCanBeWrittenMoreThanOnce) {
  auto exec_attr = fbl::unique_fd(open("/proc/self/attr/exec", O_RDWR));
  ASSERT_TRUE(exec_attr.is_valid()) << strerror(errno);

  // Write a valid context into the attribute.
  constexpr char kValidContext[] = "system_u:unconfined_r:unconfined_t:s0";
  auto result = write(exec_attr.get(), kValidContext, sizeof(kValidContext));
  ASSERT_EQ(result, (ssize_t)sizeof(kValidContext)) << strerror(errno);

  // Read back the context.
  ASSERT_EQ(lseek(exec_attr.get(), 0, SEEK_SET), 0) << strerror(errno);
  std::string context;
  ASSERT_TRUE(files::ReadFileDescriptorToString(exec_attr.get(), &context));
  EXPECT_EQ(RemoveTrailingNul(context), kValidContext);

  // Seek back to position zero, so writes will be accepted, and write a NUL to
  // clear the attribute.
  ASSERT_EQ(lseek(exec_attr.get(), 0, SEEK_SET), 0) << strerror(errno);
  constexpr char kSingleNul[] = "\0";
  result = write(exec_attr.get(), kSingleNul, sizeof(kSingleNul));
  ASSERT_EQ(result, (ssize_t)sizeof(kSingleNul)) << strerror(errno);

  // Read back the context, which will now be empty.
  ASSERT_TRUE(files::ReadFileDescriptorToString(exec_attr.get(), &context));
  EXPECT_EQ(context, std::string());
}

}  // namespace
