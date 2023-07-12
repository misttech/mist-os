// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <net/if.h>
#include <net/if_arp.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/test_helper.h"

namespace {

constexpr char kLoopbackIfName[] = "lo";
constexpr char kUnknownIfName[] = "unknown";

constexpr short kLoopbackIfFlagsEnabled = IFF_UP | IFF_LOOPBACK | IFF_RUNNING;
constexpr short kLoopbackIfFlagsDisabled = IFF_LOOPBACK;

class IoctlTest : public ::testing::Test {
 public:
  void SetUp() override {
    ASSERT_TRUE(fd = fbl::unique_fd(socket(AF_INET, SOCK_DGRAM, 0))) << strerror(errno);
  }

 protected:
  fbl::unique_fd fd;
};

struct IoctlInvalidTestCase {
  uint16_t req;
  uint16_t family;
  const char* name;
  int expected_errno;
};

class IoctlInvalidTest : public IoctlTest,
                         public ::testing::WithParamInterface<IoctlInvalidTestCase> {};

TEST_P(IoctlInvalidTest, InvalidRequest) {
  const auto [req, family, name, expected_errno] = GetParam();

  // TODO(fxbug.dev/129749): This test does not work with SIOC{G,S}IFADDR as
  // any family value returns 0. Need to find out why.
  if ((req == SIOCGIFADDR || req == SIOCSIFADDR) && !test_helper::IsStarnix()) {
    GTEST_SKIP() << "IoctlInvalidTests with SIOCGIFADDR/SIOCSIFADDR do not work on Linux yet";
  }
  if (req == SIOCSIFADDR && !test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFADDR requires root, skipping...";
  }
  if (req == SIOCSIFFLAGS && !test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFFLAGS requires root, skipping...";
  }

  ifreq ifr;
  ifr.ifr_addr = {.sa_family = family};
  strncpy(ifr.ifr_name, name, IFNAMSIZ);

  ASSERT_EQ(ioctl(fd.get(), req, &ifr), -1);
  EXPECT_EQ(errno, expected_errno);
}

INSTANTIATE_TEST_SUITE_P(IoctlInvalidTest, IoctlInvalidTest,
                         ::testing::Values(
                             IoctlInvalidTestCase{
                                 .req = SIOCGIFINDEX,
                                 .family = AF_INET,
                                 .name = kUnknownIfName,
                                 .expected_errno = ENODEV,
                             },
                             IoctlInvalidTestCase{
                                 .req = SIOCGIFHWADDR,
                                 .family = AF_INET,
                                 .name = kUnknownIfName,
                                 .expected_errno = ENODEV,
                             },
                             IoctlInvalidTestCase{
                                 .req = SIOCGIFADDR,
                                 .family = AF_INET,
                                 .name = kUnknownIfName,
                                 .expected_errno = ENODEV,
                             },
                             IoctlInvalidTestCase{
                                 .req = SIOCGIFADDR,
                                 .family = AF_INET6,
                                 .name = kLoopbackIfName,
                                 .expected_errno = EINVAL,
                             },
                             IoctlInvalidTestCase{
                                 .req = SIOCSIFADDR,
                                 .family = AF_INET,
                                 .name = kUnknownIfName,
                                 .expected_errno = ENODEV,
                             },
                             IoctlInvalidTestCase{
                                 .req = SIOCSIFADDR,
                                 .family = AF_INET6,
                                 .name = kLoopbackIfName,
                                 .expected_errno = EINVAL,
                             },
                             IoctlInvalidTestCase{
                                 .req = SIOCGIFFLAGS,
                                 .name = kUnknownIfName,
                                 .expected_errno = ENODEV,
                             },
                             IoctlInvalidTestCase{
                                 .req = SIOCSIFFLAGS,
                                 .name = kUnknownIfName,
                                 .expected_errno = ENODEV,
                             }));

void GetIfAddr(fbl::unique_fd& fd, in_addr_t expected_addr) {
  ifreq ifr;
  ifr.ifr_addr = {.sa_family = AF_INET};
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ASSERT_EQ(ioctl(fd.get(), SIOCGIFADDR, &ifr), 0) << strerror(errno);

  EXPECT_EQ(strncmp(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ), 0);
  sockaddr_in* s = reinterpret_cast<sockaddr_in*>(&ifr.ifr_addr);
  EXPECT_EQ(s->sin_family, AF_INET);
  EXPECT_EQ(s->sin_port, 0);
  EXPECT_EQ(ntohl(s->sin_addr.s_addr), expected_addr);
}

TEST_F(IoctlTest, SIOCGIFADDR_Success) { ASSERT_NO_FATAL_FAILURE(GetIfAddr(fd, INADDR_LOOPBACK)); }

void SetIfAddr(fbl::unique_fd& fd, in_addr_t addr) {
  ifreq ifr;
  *(reinterpret_cast<sockaddr_in*>(&ifr.ifr_addr)) = sockaddr_in{
      .sin_family = AF_INET,
      .sin_addr = {.s_addr = addr},
  };
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ASSERT_EQ(ioctl(fd.get(), SIOCSIFADDR, &ifr), 0) << strerror(errno);

  ASSERT_NO_FATAL_FAILURE(GetIfAddr(fd, addr));
}

TEST_F(IoctlTest, SIOCSIFADDR_Success) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFADDR requires root, skipping...";
  }
  ASSERT_NO_FATAL_FAILURE(SetIfAddr(fd, INADDR_ANY));
  ASSERT_NO_FATAL_FAILURE(SetIfAddr(fd, INADDR_LOOPBACK));
}

short GetLoopbackIfFlags(fbl::unique_fd& fd) {
  ifreq ifr;
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  EXPECT_EQ(ioctl(fd.get(), SIOCGIFFLAGS, &ifr), 0) << strerror(errno);

  EXPECT_EQ(strncmp(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ), 0);
  return ifr.ifr_ifru.ifru_flags;
}

TEST_F(IoctlTest, SIOCGIFFLAGS_Success) {
  EXPECT_EQ(GetLoopbackIfFlags(fd), kLoopbackIfFlagsEnabled);
}

void SetLoopbackIfFlags(fbl::unique_fd& fd, short flags) {
  ifreq ifr;
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ifr.ifr_ifru.ifru_flags = flags;
  ASSERT_EQ(ioctl(fd.get(), SIOCSIFFLAGS, &ifr), 0) << strerror(errno);

  if ((flags & IFF_UP) == IFF_UP) {
    // TODO(https://issuetracker.google.com/290372180): Once Netlink properly
    // synchronizes enable requests, replace this "wait for expected flags" with
    //  a single check.
    while (true) {
      if (GetLoopbackIfFlags(fd) == flags) {
        break;
      }
      sleep(1);
    }
  } else {
    EXPECT_EQ(GetLoopbackIfFlags(fd), flags);
  }
}

TEST_F(IoctlTest, SIOCSIFFLAGS_Success) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFFLAGS requires root, skipping...";
  }
  ASSERT_EQ(GetLoopbackIfFlags(fd), kLoopbackIfFlagsEnabled);
  ASSERT_NO_FATAL_FAILURE(SetLoopbackIfFlags(fd, kLoopbackIfFlagsDisabled));
  ASSERT_NO_FATAL_FAILURE(SetLoopbackIfFlags(fd, kLoopbackIfFlagsEnabled));
}

TEST_F(IoctlTest, SIOCGIFHWADDR_Success) {
  ifreq ifr = {};
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ASSERT_EQ(ioctl(fd.get(), SIOCGIFHWADDR, &ifr), 0) << strerror(errno);

  EXPECT_EQ(strncmp(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ), 0);
  sockaddr* s = &ifr.ifr_hwaddr;
  EXPECT_EQ(s->sa_family, ARPHRD_LOOPBACK);
  constexpr char kAllZeroes[sizeof(sockaddr{}.sa_data)] = {0};
  EXPECT_EQ(memcmp(s->sa_data, kAllZeroes, sizeof(kAllZeroes)), 0);
}

TEST_F(IoctlTest, SIOCGIFINDEX_Success) {
  ifreq ifr = {};
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ASSERT_EQ(ioctl(fd.get(), SIOCGIFINDEX, &ifr), 0) << strerror(errno);

  EXPECT_EQ(strncmp(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ), 0);
  EXPECT_GT(ifr.ifr_ifindex, 0);
}

}  // namespace
