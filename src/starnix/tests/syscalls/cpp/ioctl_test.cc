// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <arpa/inet.h>
#include <dirent.h>
#include <fcntl.h>
#include <net/if.h>
#include <net/if_arp.h>
#include <net/route.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/input.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

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

  // TODO(https://fxbug.dev/42080141): This test does not work with SIOC{G,S}IFADDR as
  // any family value returns 0. Need to find out why.
  if ((req == SIOCGIFADDR || req == SIOCSIFADDR) && !test_helper::IsStarnix()) {
    GTEST_SKIP() << "IoctlInvalidTests with SIOCGIFADDR/SIOCSIFADDR do not work on Linux yet";
  }
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (req == SIOCSIFADDR && !test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFADDR requires root, skipping...";
  }
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
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
  EXPECT_EQ(s->sin_addr.s_addr, expected_addr);
}

TEST_F(IoctlTest, SIOCGIFADDR_Success) {
  ASSERT_NO_FATAL_FAILURE(GetIfAddr(fd, htonl(INADDR_LOOPBACK)));
}

void SetIfAddr(fbl::unique_fd& fd, in_addr_t addr) {
  ifreq ifr;
  *(reinterpret_cast<sockaddr_in*>(&ifr.ifr_addr)) = sockaddr_in{
      .sin_family = AF_INET,
      .sin_addr = {.s_addr = addr},
  };
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ASSERT_EQ(ioctl(fd.get(), SIOCSIFADDR, &ifr), 0) << strerror(errno);
}

TEST_F(IoctlTest, SIOCSIFADDR_Success) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFADDR requires root, skipping...";
  }

  ASSERT_NO_FATAL_FAILURE(SetIfAddr(fd, htonl(INADDR_ANY)));
  ASSERT_NO_FATAL_FAILURE(GetIfAddr(fd, htonl(INADDR_ANY)));
  ASSERT_NO_FATAL_FAILURE(SetIfAddr(fd, htonl(INADDR_LOOPBACK)));
  ASSERT_NO_FATAL_FAILURE(GetIfAddr(fd, htonl(INADDR_LOOPBACK)));
}

// Uses netlink to dump all interface addresses and prefix lens into the given vector of in_addr_t.
void DumpIpv4AddressesOnInterface(uint32_t if_index,
                                  std::vector<std::pair<in_addr_t, uint8_t>>& addresses) {
  fbl::unique_fd nlsock(socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE));
  ASSERT_TRUE(nlsock) << strerror(errno);

  struct {
    nlmsghdr hdr;
    ifaddrmsg ifa;
  } req = {};

  req.hdr.nlmsg_len = NLMSG_LENGTH(sizeof(ifaddrmsg));
  req.hdr.nlmsg_type = RTM_GETADDR;
  req.hdr.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
  req.ifa.ifa_family = AF_INET;

  ASSERT_EQ(send(nlsock.get(), &req, req.hdr.nlmsg_len, 0), static_cast<int>(req.hdr.nlmsg_len))
      << strerror(errno);

  constexpr size_t kBufSize = 4096;
  char buf[kBufSize];

  while (true) {
    ssize_t len = recv(nlsock.get(), &buf, kBufSize, 0);
    ASSERT_GT(len, 0) << strerror(errno);
    for (nlmsghdr* hdr = reinterpret_cast<nlmsghdr*>(buf); NLMSG_OK(hdr, len);
         hdr = NLMSG_NEXT(hdr, len)) {
      if (hdr->nlmsg_type == NLMSG_DONE) {
        return;
      }
      if (hdr->nlmsg_type == NLMSG_ERROR) {
        FAIL() << "netlink error";
      }

      ifaddrmsg* ifa = static_cast<ifaddrmsg*>(NLMSG_DATA(hdr));
      if (ifa->ifa_family != AF_INET || ifa->ifa_index != if_index) {
        continue;
      }

      rtattr* rta = IFA_RTA(ifa);
      in_addr addr;
      memcpy(&addr, RTA_DATA(rta), sizeof(addr));
      addresses.emplace_back(addr.s_addr, ifa->ifa_prefixlen);
    }
  }
}

// Uses netlink to install the given IPv4 address on the given interface.
void InstallIpv4AddressOnInterface(const char* if_name, in_addr_t addr, uint8_t prefix_len) {
  fbl::unique_fd nlsock(socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE));
  ASSERT_TRUE(nlsock) << strerror(errno);

  // Prepare the netlink request.
  struct {
    nlmsghdr hdr;
    ifaddrmsg ifa;
    rtattr rta;
    in_addr in_addr;
  } req;

  req.hdr.nlmsg_len = NLMSG_LENGTH(sizeof(ifaddrmsg)) + RTA_LENGTH(sizeof(in_addr));
  req.hdr.nlmsg_type = RTM_NEWADDR;
  req.hdr.nlmsg_flags = NLM_F_REQUEST | NLM_F_CREATE | NLM_F_EXCL | NLM_F_ACK;
  req.hdr.nlmsg_seq = 1;  // Arbitrary non-zero value.

  req.ifa.ifa_family = AF_INET;
  req.ifa.ifa_prefixlen = prefix_len;
  req.ifa.ifa_flags = 0;
  req.ifa.ifa_scope = RT_SCOPE_UNIVERSE;
  req.ifa.ifa_index = if_nametoindex(if_name);
  assert(req.ifa.ifa_index != 0);  // Ensure the interface exists.

  req.rta.rta_type = IFA_LOCAL;
  req.rta.rta_len = RTA_LENGTH(sizeof(in_addr));
  req.in_addr.s_addr = addr;

  ASSERT_EQ(send(nlsock.get(), &req, req.hdr.nlmsg_len, 0), static_cast<int>(req.hdr.nlmsg_len))
      << strerror(errno);

  constexpr size_t kBufSize = 4096;
  char buf[kBufSize];
  ssize_t len = recv(nlsock.get(), &buf, kBufSize, 0);
  ASSERT_GT(len, 0) << strerror(errno);

  nlmsghdr* response_hdr = reinterpret_cast<nlmsghdr*>(buf);
  ASSERT_TRUE(NLMSG_OK(response_hdr, len)) << "Invalid netlink response";
  ASSERT_EQ(response_hdr->nlmsg_type, NLMSG_ERROR) << "Unexpected netlink response type";

  nlmsgerr* err = reinterpret_cast<nlmsgerr*>(NLMSG_DATA(response_hdr));
  ASSERT_EQ(err->error, 0) << "Netlink error: " << strerror(-err->error);
}

TEST_F(IoctlTest, SIOCSIFADDR_WithMultipleAddressesOnInterface) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFADDR requires root, skipping...";
  }
  ASSERT_NO_FATAL_FAILURE(SetIfAddr(fd, htonl(INADDR_ANY)));
  ASSERT_NO_FATAL_FAILURE(GetIfAddr(fd, htonl(INADDR_ANY)));
  ASSERT_NO_FATAL_FAILURE(SetIfAddr(fd, htonl(INADDR_LOOPBACK)));
  ASSERT_NO_FATAL_FAILURE(GetIfAddr(fd, htonl(INADDR_LOOPBACK)));

  // Retrieve the address via netlink and check that the retrieved address is the one we set.
  // This helps guard against a regression due to mixing up endianness.
  fbl::unique_fd nlsock(socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE));
  ASSERT_TRUE(nlsock) << strerror(errno);

  struct {
    nlmsghdr hdr;
    ifaddrmsg ifa;
  } req = {};

  req.hdr.nlmsg_len = NLMSG_LENGTH(sizeof(ifaddrmsg));
  req.hdr.nlmsg_type = RTM_GETADDR;
  req.hdr.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
  req.ifa.ifa_family = AF_INET;

  ASSERT_EQ(send(nlsock.get(), &req, req.hdr.nlmsg_len, 0), static_cast<int>(req.hdr.nlmsg_len))
      << strerror(errno);

  constexpr size_t kBufSize = 4096;
  char buf[kBufSize];

  ssize_t len = recv(nlsock.get(), &buf, kBufSize, 0);
  ASSERT_GT(len, 0) << strerror(errno);

  std::vector<std::pair<in_addr_t, uint8_t>> addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));

  std::vector<std::pair<in_addr_t, uint8_t>> expected_addresses = {{htonl(INADDR_LOOPBACK), 8}};

  EXPECT_EQ(addresses, expected_addresses);

  // Install another IP address so we can exercise what happens if there are multiple.
  InstallIpv4AddressOnInterface("lo", inet_addr("1.2.3.4"), 24);
  addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));

  expected_addresses = {{inet_addr("1.2.3.4"), 24}, {htonl(INADDR_LOOPBACK), 8}};
  EXPECT_EQ(addresses, expected_addresses);

  SetIfAddr(fd, inet_addr("5.6.7.8"));
  addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));

  expected_addresses = {{inet_addr("5.6.7.8"), 8}, {htonl(INADDR_LOOPBACK), 8}};
  EXPECT_EQ(addresses, expected_addresses);

  // SIOCSIFADDR with the all-zeros address should remove the first address it finds.
  SetIfAddr(fd, inet_addr("0.0.0.0"));
  addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));

  expected_addresses = {{htonl(INADDR_LOOPBACK), 8}};
  EXPECT_EQ(addresses, expected_addresses);
}

void GetIfNetmask(fbl::unique_fd& fd, in_addr_t expected_addr) {
  ifreq ifr;
  ifr.ifr_netmask = {.sa_family = AF_INET};
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ASSERT_EQ(ioctl(fd.get(), SIOCGIFNETMASK, &ifr), 0) << strerror(errno);

  EXPECT_EQ(strncmp(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ), 0);
  sockaddr_in* s = reinterpret_cast<sockaddr_in*>(&ifr.ifr_netmask);
  EXPECT_EQ(s->sin_family, AF_INET);
  EXPECT_EQ(s->sin_port, 0);
  EXPECT_EQ(s->sin_addr.s_addr, expected_addr);
}

TEST_F(IoctlTest, SIOCGIFNETMASK_Success) {
  in_addr_t expected_netmask = inet_addr("255.0.0.0");
  ASSERT_NO_FATAL_FAILURE(GetIfNetmask(fd, expected_netmask));
}

void SetIfNetmask(fbl::unique_fd& fd, in_addr_t addr) {
  ifreq ifr;
  *(reinterpret_cast<sockaddr_in*>(&ifr.ifr_netmask)) = sockaddr_in{
      .sin_family = AF_INET,
      .sin_addr = {.s_addr = addr},
  };
  strncpy(ifr.ifr_name, kLoopbackIfName, IFNAMSIZ);
  ASSERT_EQ(ioctl(fd.get(), SIOCSIFNETMASK, &ifr), 0) << strerror(errno);
}

TEST_F(IoctlTest, SIOCSIFNETMASK_Success) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFNETMASK requires root, skipping...";
  }

  ASSERT_NO_FATAL_FAILURE(SetIfNetmask(fd, inet_addr("255.255.0.0")));
  ASSERT_NO_FATAL_FAILURE(GetIfNetmask(fd, inet_addr("255.255.0.0")));
  ASSERT_NO_FATAL_FAILURE(SetIfNetmask(fd, inet_addr("255.0.0.0")));
  ASSERT_NO_FATAL_FAILURE(GetIfNetmask(fd, inet_addr("255.0.0.0")));
}

TEST_F(IoctlTest, SIOCSIFNETMASK_WithMultipleAddressesOnInterface) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "SIOCSIFNETMASK requires root, skipping...";
  }

  std::vector<std::pair<in_addr_t, uint8_t>> addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));
  std::vector<std::pair<in_addr_t, uint8_t>> expected_addresses = {{htonl(INADDR_LOOPBACK), 8}};
  EXPECT_EQ(addresses, expected_addresses);

  InstallIpv4AddressOnInterface("lo", inet_addr("1.2.3.4"), 24);
  addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));
  expected_addresses = {
      {inet_addr("1.2.3.4"), 24},
      {htonl(INADDR_LOOPBACK), 8},
  };
  EXPECT_EQ(addresses, expected_addresses);

  SetIfNetmask(fd, inet_addr("255.255.0.0"));
  addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));
  expected_addresses = {
      {inet_addr("1.2.3.4"), 16},
      {htonl(INADDR_LOOPBACK), 8},
  };
  EXPECT_EQ(addresses, expected_addresses);

  // Clear the address so that loopback is left in the same state as when we started.
  SetIfAddr(fd, inet_addr("0.0.0.0"));
  addresses = {};
  ASSERT_NO_FATAL_FAILURE(DumpIpv4AddressesOnInterface(if_nametoindex(kLoopbackIfName), addresses));
  expected_addresses = {
      {htonl(INADDR_LOOPBACK), 8},
  };
  EXPECT_EQ(addresses, expected_addresses);
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
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
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

// Check the names of all available input devices as reported by EVIOCGNAME.
// We expect few (two to be exact).
TEST_F(IoctlTest, EVIOCGNAME_Success) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "EVIOCGNAME requires permissions to avoid EACCESS, skipping here...";
  }

  std::vector<std::string> input_device_names;
  const std::string dev_input_path = "/dev/input";
  DIR* dir = opendir(dev_input_path.c_str());
  ASSERT_NE(dir, nullptr);

  for (struct dirent* entry = readdir(dir); entry != nullptr; entry = readdir(dir)) {
    const std::string dev_file = entry->d_name;
    if (dev_file == "." || dev_file == "..") {
      continue;
    }
    const std::string dev_path = dev_input_path + "/" + dev_file;
    const int fd = open(dev_path.c_str(), O_RDONLY);
    ASSERT_GT(fd, 0) << "for: " << dev_path;
    char dev_name[100];
    const int result = ioctl(fd, EVIOCGNAME(sizeof(dev_name)), &dev_name);
    ASSERT_GT(result, 0) << "for: " << dev_path;
    close(fd);
    input_device_names.push_back(dev_name);
  }

  EXPECT_THAT(input_device_names, testing::UnorderedElementsAre("starnix_touch_fc1a_0002_v0",
                                                                "starnix_buttons_fc1a_0001_v1"));
}

// If the buffer for copying the device name is too small, copy only how much
// will fit.
TEST_F(IoctlTest, EVIOCGNAME_TooSmall) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "EVIOCGNAME requires permissions to avoid EACCESS, skipping here...";
  }
  const std::string dev_path = "/dev/input/event0";
  int fd = open(dev_path.c_str(), O_RDONLY);
  ASSERT_GT(fd, 0);
  char dev_name[10];
  int result = ioctl(fd, EVIOCGNAME(sizeof(dev_name)), &dev_name);
  EXPECT_EQ(result, 10);
  close(fd);
}

}  // namespace
