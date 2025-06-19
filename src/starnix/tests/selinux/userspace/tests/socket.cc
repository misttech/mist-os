// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <netinet/ip.h>
#include <sys/socket.h>
#include <sys/un.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/if_ether.h>
#include <linux/netlink.h>

#include "src/lib/files/file.h"
#include "src/starnix/tests/selinux/userspace/util.h"

extern std::string DoPrePolicyLoadWork() { return "socket_policy.pp"; }

namespace {

constexpr int kTestBacklog = 5;

struct SocketTestCase {
  int domain;
  int type;
};

fit::result<int, fbl::unique_fd> SocketWithLabel(int domain, int type, int protocol,
                                                 std::string_view label) {
  auto sockcreate = ScopedTaskAttrResetter::SetTaskAttr("sockcreate", label);
  fbl::unique_fd fd(socket(domain, type, protocol));
  if (!fd) {
    return fit::error(errno);
  }
  return fit::ok(std::move(fd));
}

class SocketTest : public ::testing::TestWithParam<SocketTestCase> {};

TEST_P(SocketTest, SocketTakesProcessLabel) {
  const SocketTestCase& test_case = GetParam();
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_no_trans_t:s0"), fit::ok());

  fbl::unique_fd sockfd = fbl::unique_fd(socket(test_case.domain, test_case.type, 0));
  ASSERT_TRUE(sockfd) << strerror(errno);
  EXPECT_EQ(GetLabel(sockfd.get()), "test_u:test_r:socket_test_no_trans_t:s0");
}

INSTANTIATE_TEST_SUITE_P(
    SocketTests, SocketTest,
    ::testing::Values(SocketTestCase{AF_UNIX, SOCK_STREAM}, SocketTestCase{AF_UNIX, SOCK_DGRAM},
                      SocketTestCase{AF_UNIX, SOCK_RAW}, SocketTestCase{AF_PACKET, SOCK_RAW},
                      SocketTestCase{AF_NETLINK, SOCK_RAW}, SocketTestCase{AF_INET, SOCK_STREAM},
                      SocketTestCase{AF_INET6, SOCK_DGRAM}));

struct SocketTransitionTestCase {
  int domain;
  int type;
  int protocol;
  std::string_view expected_label;
};

// For AF_INET IPPROTO_ICMP sockets, update ping range to include current GID to allow creating
// sockets.
void MaybeUpdatePingRange(int family, int protocol) {
  constexpr char kProcPingGroupRange[] = "/proc/sys/net/ipv4/ping_group_range";
  if (family != AF_INET || protocol != IPPROTO_ICMP) {
    return;
  }
  std::string ping_group_range;
  if (!files::ReadFileToString(kProcPingGroupRange, &ping_group_range)) {
    fprintf(stderr, "Failed to read %s.\n", kProcPingGroupRange);
    return;
  }
  std::stringstream ss(ping_group_range);
  gid_t min_gid = 0, max_gid = 0;
  if (!(ss >> min_gid >> max_gid)) {
    fprintf(stderr, "Failed to parse GIDs from file content: %s\n", ping_group_range.c_str());
    return;
  }
  gid_t current_egid = getegid();
  if (current_egid < min_gid || current_egid > max_gid) {
    char buf[100] = {};
    sprintf(buf, "%d %d", current_egid, current_egid);
    files::WriteFile(kProcPingGroupRange, buf);
  }
}

class SocketTransitionTest : public ::testing::TestWithParam<SocketTransitionTestCase> {
 protected:
  void SetUp() override {
    const SocketTransitionTestCase& test_case = GetParam();
    MaybeUpdatePingRange(test_case.domain, test_case.protocol);
  }
};

TEST_P(SocketTransitionTest, SocketLabelingAccountsForTransitions) {
  const SocketTransitionTestCase& test_case = GetParam();
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  fbl::unique_fd sockfd =
      fbl::unique_fd(socket(test_case.domain, test_case.type, test_case.protocol));
  ASSERT_TRUE(sockfd) << strerror(errno);
  EXPECT_EQ(GetLabel(sockfd.get()), test_case.expected_label);
}

INSTANTIATE_TEST_SUITE_P(
    SocketTransitionTests, SocketTransitionTest,
    ::testing::Values(
        SocketTransitionTestCase{AF_UNIX, SOCK_STREAM, 0,
                                 "test_u:test_r:unix_stream_socket_test_t:s0"},
        SocketTransitionTestCase{AF_UNIX, SOCK_DGRAM, 0,
                                 "test_u:test_r:unix_dgram_socket_test_t:s0"},
        // AF_UNIX SOCK_RAW sockets are treated as SOCK_DGRAM.
        SocketTransitionTestCase{AF_UNIX, SOCK_RAW, 0, "test_u:test_r:unix_dgram_socket_test_t:s0"},
        SocketTransitionTestCase{AF_INET, SOCK_STREAM, 0, "test_u:test_r:tcp_socket_test_t:s0"},
        SocketTransitionTestCase{AF_INET, SOCK_DGRAM, 0, "test_u:test_r:udp_socket_test_t:s0"},
        SocketTransitionTestCase{AF_INET, SOCK_DGRAM, IPPROTO_ICMP,
                                 "test_u:test_r:rawip_socket_test_t:s0"},
        SocketTransitionTestCase{AF_PACKET, SOCK_RAW, htons(ETH_P_ALL),
                                 "test_u:test_r:packet_socket_test_t:s0"},
        SocketTransitionTestCase{AF_NETLINK, SOCK_RAW, NETLINK_ROUTE,
                                 "test_u:test_r:netlink_route_socket_test_t:s0"},
        SocketTransitionTestCase{AF_NETLINK, SOCK_RAW, NETLINK_USERSOCK,
                                 "test_u:test_r:netlink_socket_test_t:s0"}));

TEST(SocketTest, SockFileLabelIsCorrect) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  fbl::unique_fd sockfd = fbl::unique_fd(socket(AF_UNIX, SOCK_STREAM, 0));
  ASSERT_TRUE(sockfd) << strerror(errno);

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

TEST(SocketTest, ListenAllowed) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_listen_test_t:s0"), fit::ok());
  auto enforce = ScopedEnforcement::SetEnforcing();
  auto sockfd = SocketWithLabel(AF_INET, SOCK_STREAM, 0, "test_u:test_r:socket_listen_yes_t:s0");
  ASSERT_TRUE(sockfd.is_ok()) << sockfd.error_value();

  sockaddr_in addr;
  std::memset(&addr, 0, sizeof(addr));
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = INADDR_ANY;
  ASSERT_THAT(bind(sockfd.value().get(), (struct sockaddr*)&addr, sizeof(addr)), SyscallSucceeds());
  EXPECT_THAT(listen(sockfd.value().get(), kTestBacklog), SyscallSucceeds());
}

TEST(SocketTest, ListenDenied) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_listen_test_t:s0"), fit::ok());
  auto enforce = ScopedEnforcement::SetEnforcing();
  auto sockfd = SocketWithLabel(AF_INET, SOCK_STREAM, 0, "test_u:test_r:socket_listen_no_t:s0");
  ASSERT_TRUE(sockfd.is_ok()) << sockfd.error_value();

  sockaddr_in addr;
  std::memset(&addr, 0, sizeof(addr));
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = INADDR_ANY;
  ASSERT_THAT(bind(sockfd.value().get(), (struct sockaddr*)&addr, sizeof(addr)), SyscallSucceeds());
  EXPECT_THAT(listen(sockfd.value().get(), kTestBacklog), SyscallFailsWithErrno(EACCES));
}

TEST(SocketTest, SendmsgAllowed) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_sendmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_sendmsg_yes_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  char data[] = "y";
  struct iovec iov[] = {{
      .iov_base = data,
      .iov_len = 1,
  }};
  struct msghdr msg = {0};
  msg.msg_iov = iov;
  msg.msg_iovlen = 1;

  EXPECT_THAT(sendmsg(fds[0], &msg, 0), SyscallSucceeds());
}

TEST(SocketTest, SendmsgDenied) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_sendmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_sendmsg_no_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));

  char data[] = "n";
  struct iovec iov[] = {{
      .iov_base = data,
      .iov_len = 1,
  }};
  struct msghdr msg = {0};
  msg.msg_iov = iov;
  msg.msg_iovlen = 1;

  EXPECT_THAT(sendmsg(fds[0], &msg, 0), SyscallFailsWithErrno(EACCES));
}

TEST(SocketTest, WriteAllowed) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_sendmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_sendmsg_yes_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  char data[] = "y";
  EXPECT_THAT(write(fds[0], &data, 1), SyscallSucceeds());
}

TEST(SocketTest, WriteDenied) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_sendmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_sendmsg_no_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  char data[] = "n";
  EXPECT_THAT(write(fds[0], &data, 1), SyscallFailsWithErrno(EACCES));
}

TEST(SocketTest, RecvmsgAllowed) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_recvmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_recvmsg_yes_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  char data[] = "y";
  struct iovec iov[] = {{
      .iov_base = data,
      .iov_len = 1,
  }};
  struct msghdr msg = {0};
  msg.msg_iov = iov;
  msg.msg_iovlen = 1;

  ASSERT_THAT(sendmsg(fds[0], &msg, 0), SyscallSucceeds());
  EXPECT_THAT(recvmsg(fds[1], &msg, 0), SyscallSucceeds());
}

TEST(SocketTest, RecvmsgDenied) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_recvmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_recvmsg_no_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));

  char data[] = "n";
  struct iovec iov[] = {{
      .iov_base = data,
      .iov_len = 1,
  }};
  struct msghdr msg = {0};
  msg.msg_iov = iov;
  msg.msg_iovlen = 1;

  ASSERT_THAT(sendmsg(fds[0], &msg, 0), SyscallSucceeds());
  EXPECT_THAT(recvmsg(fds[1], &msg, 0), SyscallFailsWithErrno(EACCES));
}

TEST(SocketTest, ReadAllowed) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_recvmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_recvmsg_yes_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  char data[] = "y";
  ASSERT_THAT(write(fds[0], &data, 1), SyscallSucceeds());
  EXPECT_THAT(read(fds[1], &data, 1), SyscallSucceeds());
}

TEST(SocketTest, ReadDenied) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_recvmsg_test_t:s0"), fit::ok());
  auto sockcreate =
      ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_recvmsg_no_t:s0");
  auto enforce = ScopedEnforcement::SetEnforcing();

  int fds[2];
  ASSERT_EQ(0, socketpair(AF_UNIX, SOCK_STREAM, 0, fds));
  char data[] = "y";
  ASSERT_THAT(write(fds[0], &data, 1), SyscallSucceeds());
  EXPECT_THAT(read(fds[1], &data, 1), SyscallFailsWithErrno(EACCES));
}

TEST(SocketTest, GetSocknameAndPeername) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_getname_test_t:s0"), fit::ok());
  auto enforce = ScopedEnforcement::SetEnforcing();
  auto listen_fd =
      SocketWithLabel(AF_UNIX, SOCK_STREAM, 0, "test_u:test_r:socket_getname_yes_t:s0");
  ASSERT_TRUE(listen_fd.is_ok()) << listen_fd.error_value();
  auto client_fd = SocketWithLabel(AF_UNIX, SOCK_STREAM, 0, "test_u:test_r:socket_getname_no_t:s0");
  ASSERT_TRUE(client_fd.is_ok()) << client_fd.error_value();

  constexpr char kListenPath[] = "/tmp/getpeername_test";
  struct sockaddr_un sock_addr{.sun_family = AF_UNIX};
  strncpy(sock_addr.sun_path, kListenPath, sizeof(sock_addr.sun_path) - 1);
  ASSERT_THAT(bind(listen_fd.value().get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());
  ASSERT_THAT(listen(listen_fd.value().get(), kTestBacklog), SyscallSucceeds());
  ASSERT_THAT(connect(client_fd.value().get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());

  fbl::unique_fd accepted_fd;
  ASSERT_TRUE((accepted_fd = fbl::unique_fd(accept(listen_fd.value().get(), nullptr, nullptr))))
      << strerror(errno);
  sockaddr_in addr;
  socklen_t addr_len = sizeof(addr);
  std::memset(&addr, 0, sizeof(addr));

  EXPECT_THAT(getsockname(accepted_fd.get(), (struct sockaddr*)&addr, &addr_len),
              SyscallSucceeds());
  EXPECT_THAT(getpeername(accepted_fd.get(), (struct sockaddr*)&addr, &addr_len),
              SyscallSucceeds());
  EXPECT_THAT(getsockname(client_fd.value().get(), (struct sockaddr*)&addr, &addr_len),
              SyscallFailsWithErrno(EACCES));
  EXPECT_THAT(getpeername(client_fd.value().get(), (struct sockaddr*)&addr, &addr_len),
              SyscallFailsWithErrno(EACCES));
}

TEST(SocketTest, AcceptAllowed) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_accept_test_t:s0"), fit::ok());
  auto enforce = ScopedEnforcement::SetEnforcing();
  fbl::unique_fd listen_fd, client_fd;
  {
    auto sockcreate =
        ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_accept_yes_t:s0");
    ASSERT_TRUE((listen_fd = fbl::unique_fd(socket(AF_UNIX, SOCK_STREAM, 0)))) << strerror(errno);
  }

  ASSERT_TRUE((client_fd = fbl::unique_fd(socket(AF_UNIX, SOCK_STREAM, 0)))) << strerror(errno);
  constexpr char kListenPath[] = "/tmp/accept_test_yes";
  struct sockaddr_un sock_addr{.sun_family = AF_UNIX};
  strncpy(sock_addr.sun_path, kListenPath, sizeof(sock_addr.sun_path) - 1);
  ASSERT_THAT(bind(listen_fd.get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());
  ASSERT_THAT(listen(listen_fd.get(), kTestBacklog), SyscallSucceeds());
  ASSERT_THAT(connect(client_fd.get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());

  // Accept the connection in a domain that is only allowed the "accept" permission, to verify that
  // only the "accept" permission is required and that the "create" permission is not needed to
  // create `accepted_fd` on `accept()`.
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_accept_only_test_t:s0"), fit::ok());
  fbl::unique_fd accepted_fd;
  EXPECT_TRUE((accepted_fd = fbl::unique_fd(accept(listen_fd.get(), nullptr, nullptr))))
      << strerror(errno);
}

TEST(SocketTest, AcceptDenied) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_accept_test_t:s0"), fit::ok());
  auto enforce = ScopedEnforcement::SetEnforcing();
  fbl::unique_fd listen_fd, client_fd;
  {
    auto sockcreate =
        ScopedTaskAttrResetter::SetTaskAttr("sockcreate", "test_u:test_r:socket_accept_no_t:s0");
    ASSERT_TRUE((listen_fd = fbl::unique_fd(socket(AF_UNIX, SOCK_STREAM, 0)))) << strerror(errno);
  }

  ASSERT_TRUE((client_fd = fbl::unique_fd(socket(AF_UNIX, SOCK_STREAM, 0)))) << strerror(errno);
  constexpr char kListenPath[] = "/tmp/accept_test_no";
  struct sockaddr_un sock_addr{.sun_family = AF_UNIX};
  strncpy(sock_addr.sun_path, kListenPath, sizeof(sock_addr.sun_path) - 1);
  ASSERT_THAT(bind(listen_fd.get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());
  ASSERT_THAT(listen(listen_fd.get(), kTestBacklog), SyscallSucceeds());
  ASSERT_THAT(connect(client_fd.get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());

  fbl::unique_fd accepted_fd;
  EXPECT_THAT(accept(listen_fd.get(), nullptr, nullptr), SyscallFailsWithErrno(EACCES));
}

fit::result<int, std::string> GetPeerSec(int fd) {
  char label_buf[256]{};
  socklen_t label_len = sizeof(label_buf);
  if (getsockopt(fd, SOL_SOCKET, SO_PEERSEC, label_buf, &label_len) == -1) {
    return fit::error(errno);
  }
  return fit::ok(RemoveTrailingNul(std::string(label_buf, label_len)));
}

TEST(SocketPeerSecTest, UnixDomainStream) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  auto listen_fd = SocketWithLabel(AF_UNIX, SOCK_STREAM, 0, "test_u:test_r:socket_test_peer_t:s0");
  ASSERT_TRUE(listen_fd.is_ok()) << listen_fd.error_value();
  EXPECT_THAT(GetLabel(listen_fd.value().get()), IsOk("test_u:test_r:socket_test_peer_t:s0"));

  // Before connecting, Unix stream sockets report the peer as the "unlabeled" context.
  EXPECT_THAT(GetPeerSec(listen_fd.value().get()), IsOk("unlabeled_u:unlabeled_r:unlabeled_t:s0"));

  fbl::unique_fd client_fd;
  ASSERT_TRUE((client_fd = fbl::unique_fd(socket(AF_UNIX, SOCK_STREAM, 0)))) << strerror(errno);
  EXPECT_THAT(GetLabel(client_fd.get()), IsOk("test_u:test_r:unix_stream_socket_test_t:s0"));
  EXPECT_THAT(GetPeerSec(client_fd.get()), IsOk("unlabeled_u:unlabeled_r:unlabeled_t:s0"));

  // Bind the `listen_fd` to an address and start listening on it.
  constexpr char kListenPath[] = "/tmp/unix_domain_stream_test";
  struct sockaddr_un sock_addr{.sun_family = AF_UNIX};
  strncpy(sock_addr.sun_path, kListenPath, sizeof(sock_addr.sun_path) - 1);
  ASSERT_THAT(bind(listen_fd.value().get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());
  ASSERT_THAT(listen(listen_fd.value().get(), kTestBacklog), SyscallSucceeds());

  // Connect the `client_fd` to the listener, which should immediately cause the peer label to
  // reflect that of the listening socket.
  ASSERT_THAT(connect(client_fd.get(), (struct sockaddr*)&sock_addr, sizeof(sock_addr)),
              SyscallSucceeds());
  EXPECT_THAT(GetPeerSec(client_fd.get()), IsOk("test_u:test_r:socket_test_peer_t:s0"));

  // Accept the client connection on `listen_fd` and validate the peer label reported by the
  // accepted socket.
  fbl::unique_fd accepted_fd;
  ASSERT_TRUE((accepted_fd = fbl::unique_fd(accept(listen_fd.value().get(), nullptr, nullptr))))
      << strerror(errno);
  EXPECT_THAT(GetPeerSec(accepted_fd.get()), IsOk("test_u:test_r:unix_stream_socket_test_t:s0"));
}

TEST(SocketPeerSecTest, UnixDomainDatagram) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  fbl::unique_fd fd;
  ASSERT_TRUE((fd = fbl::unique_fd(socket(AF_UNIX, SOCK_DGRAM, 0)))) << strerror(errno);
  EXPECT_THAT(GetLabel(fd.get()), IsOk("test_u:test_r:unix_dgram_socket_test_t:s0"));

  // Unix datagram sockets do not support `SO_PEERSEC`.
  EXPECT_EQ(GetPeerSec(fd.get()), fit::error(ENOPROTOOPT));
}

TEST(SocketPeerSecTest, SocketPairUnixStream) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  int fds[2]{};
  ASSERT_THAT(socketpair(AF_UNIX, SOCK_STREAM, 0, fds), SyscallSucceeds());

  fbl::unique_fd fd1(fds[0]);
  fbl::unique_fd fd2(fds[1]);

  EXPECT_THAT(GetLabel(fd1.get()), IsOk("test_u:test_r:unix_stream_socket_test_t:s0"));
  EXPECT_THAT(GetLabel(fd2.get()), IsOk("test_u:test_r:unix_stream_socket_test_t:s0"));

  // Unix-domain sockets created with `socketpair()` should report each other's labels immediately.
  EXPECT_THAT(GetPeerSec(fd1.get()), IsOk("test_u:test_r:unix_stream_socket_test_t:s0"));
  EXPECT_THAT(GetPeerSec(fd2.get()), IsOk("test_u:test_r:unix_stream_socket_test_t:s0"));
}

TEST(SocketPeerSecTest, SocketPairUnixDatagram) {
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_test_t:s0"), fit::ok());

  int fds[2];
  ASSERT_THAT(socketpair(AF_UNIX, SOCK_DGRAM, 0, fds), SyscallSucceeds());

  fbl::unique_fd fd1(fds[0]);
  fbl::unique_fd fd2(fds[1]);

  EXPECT_THAT(GetLabel(fd1.get()), IsOk("test_u:test_r:unix_dgram_socket_test_t:s0"));
  EXPECT_THAT(GetLabel(fd2.get()), IsOk("test_u:test_r:unix_dgram_socket_test_t:s0"));

  // Unix-domain datagram sockets created with `socketpair()` are described as supporting
  // `SO_PEERSEC` but actually seem to report not-supported.
  EXPECT_EQ(GetPeerSec(fd1.get()), fit::error(ENOPROTOOPT));
  EXPECT_EQ(GetPeerSec(fd2.get()), fit::error(ENOPROTOOPT));
}

}  // namespace
