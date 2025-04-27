// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <netinet/ip.h>
#include <sys/socket.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

TEST(SocketTest, ListenAllowed) {
  const int kBacklog = 5;
  LoadPolicy("socket_policy.pp");
  ASSERT_EQ(WriteTaskAttr("current", "test_u:test_r:socket_listen_test_t:s0"), fit::ok());
  ASSERT_EQ(WriteTaskAttr("sockcreate", "test_u:test_r:socket_listen_yes_t:s0"), fit::ok());
  auto enforce = ScopedEnforcement::SetEnforcing();

  fbl::unique_fd sockfd;
  ASSERT_THAT((sockfd = fbl::unique_fd(socket(AF_INET, SOCK_STREAM, 0))), SyscallSucceeds());
  sockaddr_in addr;
  std::memset(&addr, 0, sizeof(addr));
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = INADDR_ANY;
  ASSERT_THAT(bind(sockfd.get(), (struct sockaddr*)&addr, sizeof(addr)), SyscallSucceeds());
  EXPECT_THAT(listen(sockfd.get(), kBacklog), SyscallSucceeds());
}

}  // namespace
