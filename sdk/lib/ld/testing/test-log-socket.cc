// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/ld/testing/test-log-socket.h"

#include <zircon/status.h>

#include <gtest/gtest.h>

namespace ld::testing {

void TestLogSocket::Init() {
  ASSERT_EQ(zx::socket::create(0, &read_socket_, &write_socket_), ZX_OK);
}

std::string TestLogSocket::Read() && {
  constexpr size_t kBufferSize = 256;

  EXPECT_FALSE(write_socket_);
  zx::socket read = std::move(read_socket_);

  std::string result;
  while (true) {
    size_t pos = result.size();
    result.resize(pos + kBufferSize);
    size_t nread = 0;
    zx_status_t status = read.read(0, &result[pos], result.size() - pos, &nread);
    result.resize(pos + nread);
    if (status != ZX_OK) {
      EXPECT_TRUE(status == ZX_ERR_PEER_CLOSED || status == ZX_ERR_SHOULD_WAIT)
          << zx_status_get_string(status);
      break;
    }
  }
  return result;
}

}  // namespace ld::testing
