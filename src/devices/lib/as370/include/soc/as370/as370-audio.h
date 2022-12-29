// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <ddktl/metadata/audio.h>

namespace metadata {

static constexpr uint32_t kMaxAs370ConfigString = 32;

struct As370Config {
  char manufacturer[kMaxAs370ConfigString];
  char product_name[kMaxAs370ConfigString];
  bool is_input;
  RingBuffer ring_buffer;
};
}  // namespace metadata
