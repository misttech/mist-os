// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/signals/types.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using starnix_uapi::Signal;
using starnix_uapi::UncheckedSignal;

bool TestSignal() {
  BEGIN_TEST;

  EXPECT_TRUE(Signal::try_from(UncheckedSignal::From(0)).is_error());
  EXPECT_TRUE(Signal::try_from(UncheckedSignal::From(1)).is_ok());
  EXPECT_TRUE(Signal::try_from(UncheckedSignal::From(Signal::NUM_SIGNALS)).is_ok());
  EXPECT_TRUE(Signal::try_from(UncheckedSignal::From(Signal::NUM_SIGNALS + 1)).is_error());
  EXPECT_TRUE(!starnix_uapi::kSIGCHLD.is_real_time());
  EXPECT_TRUE(Signal::try_from(UncheckedSignal::From(SIGRTMIN + 12))->is_real_time());

#if 0
  char buffer[32];
  snprintf(buffer, sizeof(buffer), "%s", SIGPWR.to_string().c_str());
  EXPECT_STR_EQ(buffer, "SIGPWR(30)");

  snprintf(
      buffer, sizeof(buffer), "%s",
      Signal::try_from(UncheckedSignal::From(uapi::SIGRTMIN + 10)).unwrap().to_string().c_str());
  EXPECT_STR_EQ(buffer, "SIGRTMIN+10(42)");
#endif

  END_TEST;
}

bool TestSiginfoBytes() {
  BEGIN_TEST;

#if 0
  ktl::Vector<uint8_t> sigchld_bytes = {17, 0, 0, 0,   0, 0, 0, 0,   1, 0, 0, 0, 0,
                                        0,  0, 0, 123, 0, 0, 0, 200, 1, 0, 0, 2};
  sigchld_bytes.resize(sizeof(siginfo_t), 0);

  SignalInfo info =
      SignalInfo::New(SIGCHLD, CLD_EXITED, SignalDetail::SIGCHLD{pid : 123, uid : 456, status : 2});

  EXPECT_EQ(memcmp(info.as_siginfo_bytes().data(), sigchld_bytes.data(), sizeof(siginfo_t)), 0);
#endif
  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_signal_types)
UNITTEST("Test Signal", unit_testing::TestSignal)
// UNITTEST("Test Siginfo Bytes", unit_testing::TestSiginfoBytes)
UNITTEST_END_TESTCASE(starnix_signal_types, "starnix_signal_types", "Tests for Signal Types")
