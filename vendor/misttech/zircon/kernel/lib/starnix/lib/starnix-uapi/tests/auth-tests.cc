// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

namespace {

using starnix_uapi::Capabilities;

bool test_empty() {
  BEGIN_TEST;

  ASSERT_EQ(0u, Capabilities::empty().mask_);

  END_TEST;
}

bool test_all() {
  BEGIN_TEST;

  // all() should be every bit set, not just all the CAP_* constants
  ASSERT_EQ(UINT64_MAX, Capabilities::all().mask_);

  END_TEST;
}

bool test_union() {
  BEGIN_TEST;

  auto expected = Capabilities{.mask_ = starnix_uapi::kCapBlockSuspend.mask_ |
                                        starnix_uapi::kCapAuditRead.mask_};
  ASSERT_EQ(expected.mask_,
            starnix_uapi::kCapBlockSuspend.union_with(starnix_uapi::kCapAuditRead).mask_);
  ASSERT_EQ(starnix_uapi::kCapBlockSuspend.mask_,
            starnix_uapi::kCapBlockSuspend.union_with(starnix_uapi::kCapBlockSuspend).mask_);

  END_TEST;
}

bool test_difference() {
  BEGIN_TEST;

  auto base = starnix_uapi::kCapBpf | starnix_uapi::kCapAuditWrite;
  auto expected = starnix_uapi::kCapBpf;
  ASSERT_EQ(expected.mask_, base.difference(starnix_uapi::kCapAuditWrite).mask_);
  ASSERT_EQ(Capabilities::empty().mask_,
            base.difference(starnix_uapi::kCapBpf | starnix_uapi::kCapAuditWrite).mask_);

  END_TEST;
}

bool test_contains() {
  BEGIN_TEST;

  auto base = starnix_uapi::kCapBpf | starnix_uapi::kCapAuditWrite;
  ASSERT_TRUE(base.contains(starnix_uapi::kCapAuditWrite));
  ASSERT_TRUE(base.contains(starnix_uapi::kCapBpf));
  ASSERT_TRUE(base.contains(starnix_uapi::kCapBpf | starnix_uapi::kCapAuditWrite));

  ASSERT_FALSE(base.contains(starnix_uapi::kCapAuditControl));
  ASSERT_FALSE(base.contains(starnix_uapi::kCapAuditWrite | starnix_uapi::kCapBpf |
                             starnix_uapi::kCapAuditControl));

  END_TEST;
}

bool test_insert() {
  BEGIN_TEST;

  auto capabilities = starnix_uapi::kCapBlockSuspend;
  capabilities.insert(starnix_uapi::kCapBlockSuspend);
  ASSERT_EQ(starnix_uapi::kCapBlockSuspend.mask_, capabilities.mask_);

  capabilities.insert(starnix_uapi::kCapAuditRead);
  auto expected = Capabilities{.mask_ = starnix_uapi::kCapBlockSuspend.mask_ |
                                        starnix_uapi::kCapAuditRead.mask_};
  ASSERT_EQ(expected.mask_, capabilities.mask_);

  END_TEST;
}

bool test_remove() {
  BEGIN_TEST;

  auto capabilities = starnix_uapi::kCapBlockSuspend;
  capabilities.remove(starnix_uapi::kCapBlockSuspend);
  ASSERT_EQ(Capabilities::empty().mask_, capabilities.mask_);

  capabilities = starnix_uapi::kCapBlockSuspend | starnix_uapi::kCapAuditRead;
  capabilities.remove(starnix_uapi::kCapAuditRead);
  ASSERT_EQ(starnix_uapi::kCapBlockSuspend.mask_, capabilities.mask_);

  END_TEST;
}

bool test_try_from() {
  BEGIN_TEST;

  auto capabilities = starnix_uapi::kCapBlockSuspend;
  ASSERT_EQ(capabilities.mask_, Capabilities::try_from(CAP_BLOCK_SUSPEND).value().mask_);

  ASSERT_EQ(errno(EINVAL).error_code(), Capabilities::try_from(200000).error_value().error_code());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_uapi_auth)
UNITTEST("test empty", unit_testing::test_empty)
UNITTEST("test all", unit_testing::test_all)
UNITTEST("test union", unit_testing::test_union)
UNITTEST("test difference", unit_testing::test_difference)
UNITTEST("test contains", unit_testing::test_contains)
UNITTEST("test insert", unit_testing::test_insert)
UNITTEST("test remove", unit_testing::test_remove)
UNITTEST("test try from", unit_testing::test_try_from)
UNITTEST_END_TESTCASE(starnix_uapi_auth, "starnix_uapi_auth", "Tests Auth")
