// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/user_buffer.h>
#include <lib/mistos/util/small_vector.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using namespace starnix_uapi;

bool test_cap_buffers_to_max_rw_count_buffer_begin_past_max_address() {
  BEGIN_TEST;

  util::SmallVector<UserBuffer, 1> buffers;
  buffers.push_back(UserBuffer{.address_ = UserAddress::const_from(50), .length_ = 10});

  ASSERT_EQ(errno(EFAULT).error_code(),
            UserBuffer::cap_buffers_to_max_rw_count(UserAddress::const_from(40), buffers)
                .error_value()
                .error_code());

  END_TEST;
}

bool test_cap_buffers_to_max_rw_count_buffer_end_past_max_address() {
  BEGIN_TEST;

  util::SmallVector<UserBuffer, 1> buffers;
  buffers.push_back(UserBuffer{.address_ = UserAddress::const_from(50), .length_ = 10});

  ASSERT_EQ(errno(EFAULT).error_code(),
            UserBuffer::cap_buffers_to_max_rw_count(UserAddress::const_from(55), buffers)
                .error_value()
                .error_code());

  END_TEST;
}

bool test_cap_buffers_to_max_rw_count_buffer_overflow_u64() {
  BEGIN_TEST;

  util::SmallVector<UserBuffer, 1> buffers;
  buffers.push_back(
      UserBuffer{.address_ = UserAddress::const_from(UINT64_MAX - 10), .length_ = 20});

  ASSERT_EQ(errno(EINVAL).error_code(),
            UserBuffer::cap_buffers_to_max_rw_count(UserAddress::const_from(UINT64_MAX), buffers)
                .error_value()
                .error_code());

  END_TEST;
}

bool test_cap_buffers_to_max_rw_count_shorten_buffer() {
  BEGIN_TEST;

  util::SmallVector<UserBuffer, 1> buffers;
  buffers.push_back(
      UserBuffer{.address_ = UserAddress::const_from(0), .length_ = MAX_RW_COUNT + 10});

  auto total =
      UserBuffer::cap_buffers_to_max_rw_count(UserAddress::const_from(UINT64_MAX), buffers);

  ASSERT_EQ(total.value(), MAX_RW_COUNT);

  ASSERT_EQ(1u, buffers.size());
  ASSERT_TRUE(buffers[0].address_ == UserAddress::const_from(0));
  ASSERT_EQ(buffers[0].length_, MAX_RW_COUNT);

  END_TEST;
}

bool test_cap_buffers_to_max_rw_count_drop_buffer() {
  BEGIN_TEST;

  util::SmallVector<UserBuffer, 1> buffers;
  buffers.push_back(UserBuffer{.address_ = UserAddress::const_from(0), .length_ = MAX_RW_COUNT});
  buffers.push_back(UserBuffer{.address_ = UserAddress::const_from(1ul << 33), .length_ = 20});

  auto total =
      UserBuffer::cap_buffers_to_max_rw_count(UserAddress::const_from(UINT64_MAX), buffers);

  ASSERT_EQ(total.value(), MAX_RW_COUNT);

  ASSERT_EQ(1u, buffers.size());
  ASSERT_TRUE(buffers[0].address_ == UserAddress::const_from(0));
  ASSERT_EQ(buffers[0].length_, MAX_RW_COUNT);

  END_TEST;
}

bool test_cap_buffers_to_max_rw_count_drop_and_shorten_buffer() {
  BEGIN_TEST;

  util::SmallVector<UserBuffer, 1> buffers;
  buffers.push_back(
      UserBuffer{.address_ = UserAddress::const_from(0), .length_ = MAX_RW_COUNT - 10});
  buffers.push_back(UserBuffer{.address_ = UserAddress::const_from(1ul << 33), .length_ = 20});
  buffers.push_back(UserBuffer{.address_ = UserAddress::const_from(2ul << 33), .length_ = 20});

  auto total =
      UserBuffer::cap_buffers_to_max_rw_count(UserAddress::const_from(UINT64_MAX), buffers);

  ASSERT_EQ(total.value(), MAX_RW_COUNT);

  ASSERT_EQ(2u, buffers.size());
  ASSERT_TRUE(buffers[0].address_ == UserAddress::const_from(0));
  ASSERT_EQ(buffers[0].length_, MAX_RW_COUNT - 10);

  ASSERT_TRUE(buffers[1].address_ == UserAddress::const_from(1ul << 33));
  ASSERT_EQ(buffers[1].length_, 10u);

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_uapi_userbuffer)
UNITTEST("test cap buffers to max rw count buffer begin past max address",
         unit_testing::test_cap_buffers_to_max_rw_count_buffer_begin_past_max_address)
UNITTEST("test cap buffers to max rw count buffer end past max address",
         unit_testing::test_cap_buffers_to_max_rw_count_buffer_end_past_max_address)
UNITTEST("test cap buffers to max rw count buffer overflow u64",
         unit_testing::test_cap_buffers_to_max_rw_count_buffer_overflow_u64)
UNITTEST("test cap buffers to max rw count shorten buffer",
         unit_testing::test_cap_buffers_to_max_rw_count_shorten_buffer)
UNITTEST("test cap buffers to max rw count drop buffer",
         unit_testing::test_cap_buffers_to_max_rw_count_drop_buffer)
UNITTEST("test cap buffers to max rw count drop and shorten buffer",
         unit_testing::test_cap_buffers_to_max_rw_count_drop_and_shorten_buffer)
UNITTEST_END_TESTCASE(starnix_uapi_userbuffer, "starnix_uapi_userbuffer", "Tests User Buffer")
