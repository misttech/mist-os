// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/cprng.h>

#include <ktl/array.h>
#include <ktl/span.h>
#include <zxtest/zxtest.h>

namespace {

void check_buffer(uint8_t* buffer, size_t size) {
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
}

TEST(Cprng, cprng) {
  ktl::array<uint8_t, 20> buffer{};
  check_buffer(buffer.data(), buffer.size());
}

TEST(Cprng, cprng_large) {
  constexpr size_t kSIZE = ZX_CPRNG_DRAW_MAX_LEN + 1;
  ktl::array<uint8_t, kSIZE> buffer{};
  check_buffer(buffer.data(), buffer.size());

  ktl::span<uint8_t> buffer_span{buffer.data(), buffer.size()};
  size_t chunkSize = kSIZE / 3;
  for (size_t i = 0; i < buffer.size(); i += chunkSize) {
    size_t remaining = buffer.size() - i;
    size_t chunkEnd = (remaining < chunkSize) ? i + remaining : i + chunkSize;
    auto chunk = buffer_span.subspan(i, chunkEnd - i);
    check_buffer(chunk.data(), chunk.size());
  }
}

TEST(Cprng, cprng_add) {
  ktl::array<uint8_t, 3> buffer{0, 1, 2};
  ASSERT_TRUE(cprng_add_entropy({buffer.begin(), buffer.end()}).is_ok());
}

}  // namespace
