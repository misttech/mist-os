// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <sys/ioctl.h>

#include <gtest/gtest.h>

#include "src/starnix/lib/linux_uapi/stub/kgsl/msm_kgsl.h"

namespace {

struct ErrStr {
  [[maybe_unused]] friend std::ostream& operator<<(std::ostream& out, const ErrStr&) {
    int err = errno;
    out << err << " (" << strerror(err) << ")";
    return out;
  }
};

}  // namespace

class KgslUnitTest : public ::testing::Test {
 protected:
  KgslUnitTest() = default;

  ~KgslUnitTest() override {}

  void SetUp() override {
    constexpr auto kDevicePath = "/dev/kgsl-3d0";
    fd_ = open(kDevicePath, O_RDWR);
    ASSERT_GE(fd_, 0) << "Failed to open " << kDevicePath << ": " << ErrStr();
  }

  void TearDown() override {
    if (fd_ >= 0) {
      int result = close(fd_);
      EXPECT_EQ(result, 0);
    }
  }

  // NOLINTBEGIN
  int fd_ = -1;
  // NOLINTEND
};

TEST_F(KgslUnitTest, GetDeviceInfo) {
  kgsl_devinfo devinfo{};
  kgsl_device_getproperty args{.type = KGSL_PROP_DEVICE_INFO, .value = &devinfo};
  int result = ioctl(fd_, IOCTL_KGSL_DEVICE_GETPROPERTY, &args);
  EXPECT_EQ(result, 0) << ErrStr();
  constexpr uint32_t kPlaceholderDeviceId = 42;
  EXPECT_EQ(devinfo.device_id, kPlaceholderDeviceId);
}
