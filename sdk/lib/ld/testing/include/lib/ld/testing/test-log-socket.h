// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TESTING_TEST_LOG_SOCKET_H_
#define LIB_LD_TESTING_TEST_LOG_SOCKET_H_

#include <lib/zx/socket.h>

#include <string>

namespace ld::testing {

// This manages a Zircon socket for the purpose of collecting logging output
// from a test (in-process or spawned process).
class TestLogSocket {
 public:
  // This is separately from default construction so it can be called inside
  // ASSERT_NO_FATAL_FAILURE(...).
  void Init();

  // This takes the write end to be passed to the test.
  zx::socket TakeSocket() { return std::move(write_socket_); }

  // This reads all the data buffered in the socket and then closes it down.
  // This should be called once the test has finished so reading the socket
  // definitely won't block.  It must be the last thing called on the object.
  std::string Read() &&;

 private:
  zx::socket read_socket_, write_socket_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TESTING_TEST_LOG_SOCKET_H_
