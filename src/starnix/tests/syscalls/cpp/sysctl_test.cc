// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <net/if.h>
#include <string.h>
#include <sys/ioctl.h>

#include <chrono>
#include <format>
#include <thread>

#include <gtest/gtest.h>
#include <linux/capability.h>
#include <linux/if_tun.h>

#include "src/lib/files/file.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

class SysctlTest : public ::testing::Test {};

TEST_F(SysctlTest, AcceptRaRtTable) {
  if (!test_helper::HasCapability(CAP_NET_ADMIN)) {
    GTEST_SKIP() << "Need CAP_NET_ADMIN to run SysctlTest";
  }
  std::string accept_ra_rt_table_str;

  constexpr const char *kAcceptRaRtTable = "/proc/sys/net/ipv6/conf/{}/accept_ra_rt_table";
  const std::string kDefault = std::format(kAcceptRaRtTable, "default");
  const std::string kLo = std::format(kAcceptRaRtTable, "lo");

  if (!test_helper::IsStarnix() && access(kDefault.c_str(), F_OK) == -1) {
    GTEST_SKIP() << "The kernel is not compiled with this sysctl";
  }

  const char *kVal1 = "-100\n";
  const char *kVal2 = "100\n";

  for (auto const &path : {kDefault, kLo}) {
    EXPECT_TRUE(files::ReadFileToString(path, &accept_ra_rt_table_str));
    EXPECT_STREQ(accept_ra_rt_table_str.c_str(), "0\n");
  }

  // Write then read back value for interface lo.
  EXPECT_TRUE(files::WriteFile(kLo, kVal1));
  EXPECT_TRUE(files::ReadFileToString(kLo, &accept_ra_rt_table_str));
  EXPECT_STREQ(accept_ra_rt_table_str.c_str(), kVal1);

  // Write then read back value for special file `default`.
  EXPECT_TRUE(files::WriteFile(kDefault, kVal2));
  EXPECT_TRUE(files::ReadFileToString(kDefault, &accept_ra_rt_table_str));
  EXPECT_STREQ(accept_ra_rt_table_str.c_str(), kVal2);

  const char *kTunDev = "/dev/tun";
  int tun = open(kTunDev, O_RDWR);
  ASSERT_GT(tun, 0);

  ifreq ifr{};
  ifr.ifr_flags = IFF_NO_PI | IFF_TUN;

  const char *kTunName = "tun0";
  strncpy(ifr.ifr_name, kTunName, IFNAMSIZ);
  ASSERT_EQ(ioctl(tun, TUNSETIFF, &ifr), 0) << strerror(errno);

  const std::string kTunPath = std::format(kAcceptRaRtTable, kTunName);
  int trial = 0;
  while (!files::ReadFileToString(kTunPath, &accept_ra_rt_table_str)) {
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
    ASSERT_LE(++trial, 100);
  }

  // The new device will have the `default` value.
  EXPECT_STREQ(accept_ra_rt_table_str.c_str(), kVal2);
}
