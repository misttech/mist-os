// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon_random_generator.h"

#include <gtest/gtest.h>

namespace pw::random_fuchsia {

TEST(ZirconRandomGeneratorTest, Get) {
  // Getting a random number should not crash.
  std::array<std::byte, 4> value = {std::byte{0}};
  ZirconRandomGenerator().Get(value);
}

TEST(ZirconRandomGeneratorTest, InjectEntropyBits) {
  ZirconRandomGenerator rng;
  // Injecting 0 bits of entropy should safely do nothing.
  rng.InjectEntropyBits(/*data=*/1, /*num_bits=*/0);
  // Injecting too many bits should round down to 32 and not crash.
  rng.InjectEntropyBits(/*data=*/1, /*num_bits=*/33);
  // Inject the maximum number of bits.
  rng.InjectEntropyBits(/*data=*/1, /*num_bits=*/32);
  rng.InjectEntropyBits(/*data=*/1, /*num_bits=*/8);
  rng.InjectEntropyBits(/*data=*/1, /*num_bits=*/31);
}

}  // namespace pw::random_fuchsia
