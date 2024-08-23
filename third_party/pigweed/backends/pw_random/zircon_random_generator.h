// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef THIRD_PARTY_PIGWEED_BACKENDS_PW_RANDOM_ZIRCON_RANDOM_GENERATOR_H_
#define THIRD_PARTY_PIGWEED_BACKENDS_PW_RANDOM_ZIRCON_RANDOM_GENERATOR_H_

#include <zircon/assert.h>
#include <zircon/syscalls.h>

#include <climits>

#include "pw_random/random.h"
#include "pw_span/span.h"

namespace pw::random_fuchsia {

class ZirconRandomGenerator final : public pw::random::RandomGenerator {
 public:
  void Get(pw::ByteSpan dest) override { zx_cprng_draw(dest.data(), dest.size()); }

  void InjectEntropyBits(uint32_t data, uint_fast8_t num_bits) override {
    static_assert(sizeof(data) <= ZX_CPRNG_ADD_ENTROPY_MAX_LEN);

    constexpr uint8_t max_bits = sizeof(data) * CHAR_BIT;
    if (num_bits == 0) {
      return;
    } else if (num_bits > max_bits) {
      num_bits = max_bits;
    }

    // zx_cprng_add_entropy operates on bytes instead of bits, so round up to the nearest byte so
    // that all entropy bits are included.
    const size_t buffer_size = ((num_bits + CHAR_BIT - 1) / CHAR_BIT);
    zx_cprng_add_entropy(&data, buffer_size);
  }
};

}  // namespace pw::random_fuchsia

#endif  // THIRD_PARTY_PIGWEED_BACKENDS_PW_RANDOM_ZIRCON_RANDOM_GENERATOR_H_
