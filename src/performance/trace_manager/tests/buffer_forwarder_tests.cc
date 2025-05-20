// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
#include <lib/zx/socket.h>

#include <gtest/gtest.h>

#include "src/performance/trace_manager/deferred_buffer_forwarder.h"

TEST(BufferForwarderTest, DeferredForwarder) {
  zx::socket ep0, ep1;
  zx::socket::create(0, &ep0, &ep1);
  tracing::DeferredBufferForwarder forwarder(std::move(ep1));
  forwarder.WriteMagicNumberRecord();

  zx_signals_t pending;
  // We shouldn't see any data on the socket yet
  zx_status_t res = ep0.wait_one(ZX_SOCKET_READABLE, zx::time::infinite_past(), &pending);
  ASSERT_EQ(res, ZX_ERR_TIMED_OUT);

  forwarder.Flush();

  // Now we should see data
  res = ep0.wait_one(ZX_SOCKET_READABLE, zx::time::infinite_past(), &pending);
  ASSERT_EQ(res, ZX_OK);
  ASSERT_TRUE(pending & ZX_SOCKET_READABLE);

  uint64_t buffer[8];
  size_t actual;
  ep0.read(0, buffer, 64, &actual);

  // We should only read the 8 bytes of the record we wrote.
  ASSERT_EQ(actual, size_t{8});

  // And the bytes should be the FXT magic bytes.
  ASSERT_EQ(buffer[0], uint64_t{0x0016547846040010});

  // The socket should now be empty.
  res = ep0.wait_one(ZX_SOCKET_READABLE, zx::time::infinite_past(), &pending);
  ASSERT_EQ(res, ZX_ERR_TIMED_OUT);
}
