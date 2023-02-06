// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string>

#include "third_party/gvisor_syscall_tests/expects/common.h"
#include "third_party/gvisor_syscall_tests/expects/expectations.h"

namespace netstack_syscall_test {

const auto kDualStackTestCaseNames = {
    "ListenV4Any_ConnectV4MappedAny",
    "ListenV4Any_ConnectV4MappedLoopback",
    "ListenV4Loopback_ConnectV4MappedLoopback",
    "ListenV4MappedAny_ConnectV4Any",
    "ListenV4MappedAny_ConnectV4Loopback",
    "ListenV4MappedAny_ConnectV4MappedAny",
    "ListenV4MappedAny_ConnectV4MappedLoopback",
    "ListenV4MappedLoopback_ConnectV4Any",
    "ListenV4MappedLoopback_ConnectV4Loopback",
    "ListenV4MappedLoopback_ConnectV4MappedLoopback",
    "ListenV6Any_ConnectV4Any",
    "ListenV6Any_ConnectV4Loopback",
    "ListenV6Any_ConnectV4MappedAny",
    "ListenV6Any_ConnectV4MappedLoopback",
};

void AddNonPassingTests(TestMap& tests) {
  FilterTestsForLoopbackIsolatedTarget(tests);

  // Skip tests that will otherwise hang forever.
  // TODO(b/245940107): un-skip some of these when the data path is ready.
  SkipTest(tests, "All/SocketInetLoopbackIsolatedTest.TCPActiveCloseTimeWaitTest/*");
  SkipTest(tests, "All/SocketInetLoopbackIsolatedTest.TCPPassiveCloseNoTimeWaitTest/*");
  ExpectFailure(tests,
                "All/SocketInetLoopbackIsolatedTest.TCPPassiveCloseNoTimeWaitReuseTest/"
                "*");
  ExpectFailure(tests,
                "AllFamilies/"
                "SocketMultiProtocolInetLoopbackIsolatedTest.BindToDeviceReusePort/TCP");
  // Netstack3 does not support dual-stack TCP sockets yet.
  for (const auto& test_case : kDualStackTestCaseNames) {
    ExpectFailure(tests,
                  TestSelector::ParameterizedTest("All", "SocketInetLoopbackIsolatedTest",
                                                  "TCPActiveCloseTimeWaitReuseTest", test_case));
  }

  // Netstack3 does not have complete support for multicast sockets.
  ExpectFailure(tests,
                "AllFamilies/"
                "SocketMultiProtocolInetLoopbackIsolatedTest."
                "V4EphemeralPortReservedReuseAddr/UDP");

  ExpectFailure(tests,
                "AllFamilies/"
                "SocketMultiProtocolInetLoopbackIsolatedTest."
                "V4MappedEphemeralPortReservedReuseAddr/*");
  ExpectFailure(tests,
                "AllFamilies/"
                "SocketMultiProtocolInetLoopbackIsolatedTest."
                "V6EphemeralPortReservedReuseAddr/UDP");
}  // NOLINT(readability/fn_size)

}  // namespace netstack_syscall_test
