// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/epoll.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>

#include <gtest/gtest.h>
#include <linux/input.h>

#include "src/starnix/tests/syscalls/test_helper.h"

namespace {

constexpr size_t min_bytes(size_t n_bits) { return (n_bits + 7) / 8; }

template <size_t SIZE>
bool get_bit(const std::array<uint8_t, SIZE>& buf, size_t bit_num) {
  size_t byte_index = bit_num / 8;
  size_t bit_index = bit_num % 8;
  EXPECT_LT(byte_index, SIZE) << "get_bit(" << bit_num << ") with array of only " << SIZE
                              << " elements";
  return buf[byte_index] & (1 << bit_index);
}

// TODO(quiche): Maybe move this to a test fixture, and guarantee removal of the input
// node between test cases.
test_helper::ScopedFD GetInputFile() {
  // Typically, this would be `/dev/input/event0`, but there's not much to be gained by
  // exercising `mkdir()` in this test.
  const char kInputFile[] = "/dev/input0";

  // Create device node. Allow `EEXIST`, to avoid requiring each test case to remove the
  // input device node.
  const uint32_t kInputMajor = 13;
  if (mknod(kInputFile, 0600 | S_IFCHR, makedev(kInputMajor, 0)) != 0 && errno != EEXIST) {
    ADD_FAILURE() << " creating " << kInputFile << " failed: " << strerror(errno);
  };

  // Open device node.
  test_helper::ScopedFD fd(open(kInputFile, O_RDONLY));
  EXPECT_TRUE(fd.is_valid()) << " failed to open " << kInputFile << ": " << strerror(errno);

  return fd;
}

TEST(InputTest, DevicePropertiesMatchTouchProperties) {
  if (getuid() != 0) {
    GTEST_SKIP() << "Can only be run as root.";
  }

  auto fd = GetInputFile();
  ASSERT_TRUE(fd.is_valid());

  // Getting the driver version must succeed, but the actual value doesn't matter.
  {
    uint32_t buf;
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGVERSION, &buf)) << "get version failed: " << strerror(errno);
  }

  // Getting the device identifier must succeed, but the actual value doesn't matter.
  {
    input_id buf;
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGID, &buf)) << "get identifier failed: " << strerror(errno);
  }

  // Getting the supported keys must succeed, with `BTN_TOUCH` and `BTN_TOOL_FINGER` supported.
  {
    constexpr auto kBufSize = min_bytes(KEY_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGBIT(EV_KEY, kBufSize), &buf))
        << "get supported keys failed: " << strerror(errno);
    ASSERT_TRUE(get_bit(buf, BTN_TOUCH)) << " BTN_TOUCH not supported (but should be)";
    ASSERT_TRUE(get_bit(buf, BTN_TOOL_FINGER)) << " BTN_TOOL_FINGER not supported (but should be)";
  }

  // Getting the supported absolute position attributes must succeed, with `ABS_X` and
  // `ABS_Y` supported.
  {
    constexpr auto kBufSize = min_bytes(ABS_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGBIT(EV_ABS, kBufSize), &buf))
        << "get supported absolute position failed: " << strerror(errno);
    ASSERT_TRUE(get_bit(buf, ABS_X)) << " ABS_X not supported (but should be)";
    ASSERT_TRUE(get_bit(buf, ABS_Y)) << " ABS_Y not supported (but should be)";
  }

  // Getting the supported relative motive attributes must succeed, but the actual values
  // don't matter.
  {
    constexpr auto kBufSize = min_bytes(REL_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGBIT(EV_REL, kBufSize), &buf))
        << "get supported relative motion failed: " << strerror(errno);
  }

  // Getting the supported switches must succeed, but the actual values don't matter.
  {
    constexpr auto kBufSize = min_bytes(SW_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGBIT(EV_SW, kBufSize), &buf))
        << "get supported switches failed: " << strerror(errno);
  }

  // Getting the supported LEDs must succeed, but the actual values don't matter.
  {
    constexpr auto kBufSize = min_bytes(LED_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGBIT(EV_LED, kBufSize), &buf))
        << "get supported LEDs failed: " << strerror(errno);
  }

  // Getting the supported force feedbacks must succeed, but the actual values don't matter.
  {
    constexpr auto kBufSize = min_bytes(FF_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGBIT(EV_FF, kBufSize), &buf))
        << "get supported force feedbacks failed: " << strerror(errno);
  }

  // Getting the supported miscellaneous features must succeed, but the actual values don't matter.
  {
    constexpr auto kBufSize = min_bytes(MSC_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGBIT(EV_MSC, kBufSize), &buf))
        << "get supported miscellaneous features failed: " << strerror(errno);
  }

  // Getting the input properties must succeed, with `INPUT_PROP_DIRECT` set.
  {
    constexpr auto kBufSize = min_bytes(INPUT_PROP_MAX);
    std::array<uint8_t, kBufSize> buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGPROP(kBufSize), &buf))
        << "get supported input properties features failed: " << strerror(errno);
    ASSERT_TRUE(get_bit(buf, INPUT_PROP_DIRECT))
        << " INPUT_PROP_DIRECT not supported (but should be)";
  }

  // Getting the x-axis range must succeed. The exact axis parameters are device dependent,
  // but some basic validation is possible.
  {
    input_absinfo buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGABS(ABS_X), &buf))
        << "get x-axis info failed: " << strerror(errno);
    ASSERT_EQ(0.0, buf.minimum);
    ASSERT_GT(buf.maximum, 0.0);
  }

  // Getting the x-axis range must succeed. The exact axis parameters are device dependent,
  // but some basic validation is possible.
  {
    input_absinfo buf{};
    ASSERT_EQ(0, ioctl(fd.get(), EVIOCGABS(ABS_Y), &buf))
        << "get y-axis info failed: " << strerror(errno);
    ASSERT_EQ(0.0, buf.minimum);
    ASSERT_GT(buf.maximum, 0.0);
  }
}

TEST(InputTest, DeviceCanBeRegisteredWithEpoll) {
  if (getuid() != 0) {
    GTEST_SKIP() << "Can only be run as root.";
  }

  auto input_fd = GetInputFile();
  ASSERT_TRUE(input_fd.is_valid());

  test_helper::ScopedFD epoll_fd(epoll_create(1));  // Per `man` page, must be >0.
  ASSERT_TRUE(epoll_fd.is_valid()) << "failed to create epoll fd: " << strerror(errno);

  epoll_event epoll_params = {.events = EPOLLIN | EPOLLWAKEUP, .data = {.fd = input_fd.get()}};
  ASSERT_EQ(0, epoll_ctl(epoll_fd.get(), EPOLL_CTL_ADD, input_fd.get(), &epoll_params))
      << " epoll_ctl() failed: " << strerror(errno);

  epoll_event event_buf[1];
  ASSERT_EQ(0, epoll_wait(epoll_fd.get(), event_buf, 1, 0));
}

}  // namespace
