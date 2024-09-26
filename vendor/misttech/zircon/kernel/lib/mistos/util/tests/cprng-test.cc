// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/cprng.h>
#include <lib/unittest/unittest.h>

#include <ktl/array.h>
#include <ktl/span.h>

namespace unit_testing {

namespace {
bool check_buffer(uint8_t* buffer, size_t size) {
  BEGIN_TEST;

  auto first_zero = 0;
  auto last_zero = 0;
  for (int i = 0; i < 30; i++) {
    cprng_draw(buffer, size);
    if (buffer[0] == 0) {
      first_zero += 1;
    }
    if (size > 1 && buffer[size - 1] == 0) {
      last_zero += 1;
    }
  }
  ASSERT_NE(30, first_zero);
  ASSERT_NE(30, last_zero);

  END_TEST;
}

bool cprng() {
  BEGIN_TEST;

  ktl::array<uint8_t, 20> buffer{};
  ASSERT_TRUE(check_buffer(buffer.data(), buffer.size()));

  END_TEST;
}

bool cprng_large() {
  BEGIN_TEST;

  constexpr size_t kSIZE = ZX_CPRNG_DRAW_MAX_LEN + 1;
  ktl::array<uint8_t, kSIZE> buffer{};
  ASSERT_TRUE(check_buffer(buffer.data(), buffer.size()));

  ktl::span<uint8_t> buffer_span{buffer.data(), buffer.size()};
  size_t chunkSize = kSIZE / 3;
  for (size_t i = 0; i < buffer.size(); i += chunkSize) {
    size_t remaining = buffer.size() - i;
    size_t chunkEnd = (remaining < chunkSize) ? i + remaining : i + chunkSize;
    auto chunk = buffer_span.subspan(i, chunkEnd - i);
    ASSERT_TRUE(check_buffer(chunk.data(), chunk.size()));
  }

  END_TEST;
}

#if 0
TEST(Cprng, cprng_add) {
  ktl::array<uint8_t, 3> buffer{0, 1, 2};
  ASSERT_TRUE(cprng_add_entropy({buffer.begin(), buffer.end()}).is_ok());
}
#endif

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_util_cprng)
UNITTEST("test cprng", unit_testing::cprng)
UNITTEST("test cprng large", unit_testing::cprng_large)
// UNITTEST("node_info_is_reflected_in_stats", node_info_is_reflected_in_stat)
UNITTEST_END_TESTCASE(mistos_util_cprng, "mistos_util_cprng", "Tests Util CPRNG")
