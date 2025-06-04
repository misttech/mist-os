// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bpf_helpers.h"

SECTION("maps")
struct bpf_map_def ringbuf = {
    .type = BPF_MAP_TYPE_RINGBUF,
    .key_size = 0,
    .value_size = 0,
    .max_entries = 4096,
    .map_flags = 0,
};

int egress_test_prog(struct __sk_buff* skb) {
  int* entry = bpf_ringbuf_reserve(&ringbuf, 4, 0);
  if (!entry) {
    return 1;
  }
  *entry = skb->len;
  bpf_ringbuf_submit(entry, 0);
  return 1;
}
