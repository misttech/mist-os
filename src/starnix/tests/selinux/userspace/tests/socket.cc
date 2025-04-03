// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/socket.h>
#include <sys/un.h>
#include <sys/xattr.h>
#include <unistd.h>

#include <cstring>
#include <iostream>
#include <stdexcept>
#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {
struct SocketTestCase {
  int family;
  int type;
  std::string_view expected_label;
};

class SocketTest : public ::testing::TestWithParam<SocketTestCase> {};

TEST_P(SocketTest, SocketTakesProcessLabel) {
  const SocketTestCase& test_case = GetParam();
  LoadPolicy("socket_policy.pp");
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  fbl::unique_fd sockfd;
  ASSERT_THAT((sockfd = fbl::unique_fd(socket(test_case.family, test_case.type, 0))),
              SyscallSucceeds());
  EXPECT_EQ(GetLabel(sockfd.get()), "test_u:test_r:socket_test_t:s0");
}

INSTANTIATE_TEST_SUITE_P(
    SocketTests, SocketTest,
    ::testing::Values(SocketTestCase{AF_UNIX, SOCK_STREAM}, SocketTestCase{AF_UNIX, SOCK_DGRAM},
                      SocketTestCase{AF_UNIX, SOCK_RAW}, SocketTestCase{AF_PACKET, SOCK_RAW},
                      SocketTestCase{AF_NETLINK, SOCK_RAW}, SocketTestCase{AF_INET, SOCK_STREAM},
                      SocketTestCase{AF_INET6, SOCK_DGRAM}));

struct SocketTransitionTestCase {
  int family;
  int type;
  std::string_view expected_label;
};

class SocketTransitionTest : public ::testing::TestWithParam<SocketTransitionTestCase> {};

TEST_P(SocketTransitionTest, SocketLabelingAccountsForTransitions) {
  const SocketTransitionTestCase& test_case = GetParam();
  LoadPolicy("socket_transition_policy.pp");
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  fbl::unique_fd sockfd;
  ASSERT_THAT((sockfd = fbl::unique_fd(socket(test_case.family, test_case.type, 0))),
              SyscallSucceeds());
  EXPECT_EQ(GetLabel(sockfd.get()), test_case.expected_label);
}

INSTANTIATE_TEST_SUITE_P(
    SocketTransitionTests, SocketTransitionTest,
    ::testing::Values(
        SocketTransitionTestCase{AF_UNIX, SOCK_STREAM,
                                 "test_u:test_r:unix_stream_socket_test_t:s0"},
        SocketTransitionTestCase{AF_INET, SOCK_STREAM, "test_u:test_r:tcp_socket_test_t:s0"},
        SocketTransitionTestCase{AF_INET, SOCK_DGRAM, "test_u:test_r:udp_socket_test_t:s0"}));

TEST(SocketTest, SockFileLabelIsCorrect) {
  LoadPolicy("socket_transition_policy.pp");
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  fbl::unique_fd sockfd;
  ASSERT_THAT((sockfd = fbl::unique_fd(socket(AF_UNIX, SOCK_STREAM, 0))), SyscallSucceeds());

  struct sockaddr_un sock_addr;
  const char* kSockPath = "/tmp/test_sock_file";
  memset(&sock_addr, 0, sizeof(struct sockaddr_un));
  sock_addr.sun_family = AF_UNIX;
  strncpy(sock_addr.sun_path, kSockPath, sizeof(sock_addr.sun_path) - 1);
  unlink(kSockPath);
  ASSERT_THAT(bind(sockfd.get(), (struct sockaddr*)&sock_addr, sizeof(struct sockaddr_un)),
              SyscallSucceeds());

  EXPECT_EQ(GetLabel(sockfd.get()), "test_u:test_r:unix_stream_socket_test_t:s0");
  EXPECT_EQ(GetLabel(kSockPath), "test_u:object_r:sock_file_test_t:s0");
}
}  // namespace
