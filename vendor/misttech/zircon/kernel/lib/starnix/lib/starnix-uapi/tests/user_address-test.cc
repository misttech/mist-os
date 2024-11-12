// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/mistos/util/small_vector.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using starnix_uapi::UserAddress;
using starnix_uapi::UserRef;

namespace {

bool test_into() {
  BEGIN_TEST;
  ASSERT_EQ(mtl::DefaultConstruct<UserAddress>().ptr(),
            mtl::DefaultConstruct<UserRef<uint32_t>>().addr().ptr());

  auto user_address = UserAddress::from(32);

  ASSERT_EQ(user_address.ptr(), UserRef<uint32_t>::New(user_address).addr().ptr());
  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_uapi_useraddress)
UNITTEST("test into", unit_testing::test_into)
UNITTEST_END_TESTCASE(starnix_uapi_useraddress, "starnix_uapi_useraddress", "Tests User Address")
