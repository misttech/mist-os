// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string>

#include "third_party/gvisor_syscall_tests/expects/common.h"
#include "third_party/gvisor_syscall_tests/expects/expectations.h"

namespace netstack_syscall_test {

void AddNonPassingTests(TestMap& tests) {
  FilterTestsForLoopbackTcpBacklogTarget(tests);

  // Dual-stack TCP sockets are not supported.
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4Any_ConnectV4MappedAny");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4Any_ConnectV4MappedAny");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4Any_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4Any_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4Loopback_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4Loopback_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4MappedAny_ConnectV4Any");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4MappedAny_ConnectV4Loopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4MappedAny_ConnectV4MappedAny");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4MappedAny_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4MappedLoopback_ConnectV4Any");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4MappedLoopback_ConnectV4Loopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV4MappedLoopback_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV6Any_ConnectV4Any");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV6Any_ConnectV4MappedAny");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV6Any_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklogAcceptAll/"
                "ListenV6Any_ConnectV4Loopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4Any_ConnectV4MappedAny");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4Any_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4Loopback_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog"
                "/ListenV4MappedAny_ConnectV4Any");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4MappedAny_ConnectV4Loopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4MappedAny_ConnectV4MappedAny");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4MappedAny_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4MappedLoopback_ConnectV4Any");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4MappedLoopback_ConnectV4Loopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV4MappedLoopback_ConnectV4MappedLoopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV6Any_ConnectV4Any");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV6Any_ConnectV4Loopback");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV6Any_ConnectV4MappedAny");
  ExpectFailure(tests,
                "All/SocketInetLoopbackTest.TCPBacklog/"
                "ListenV6Any_ConnectV4MappedLoopback");
}  // NOLINT(readability/fn_size)

}  // namespace netstack_syscall_test
