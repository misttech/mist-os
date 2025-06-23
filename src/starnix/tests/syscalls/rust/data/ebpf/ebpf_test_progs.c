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

SECTION("maps")
struct bpf_map_def target_cookie = {
    .type = BPF_MAP_TYPE_ARRAY,
    .key_size = 4,
    .value_size = 8,
    .max_entries = 1,
    .map_flags = 0,
};

SECTION("maps")
struct bpf_map_def count = {
    .type = BPF_MAP_TYPE_ARRAY,
    .key_size = 4,
    .value_size = sizeof(int),
    .max_entries = 1,
    .map_flags = 0,
};

int skb_test_prog(struct __sk_buff* skb) {
  // Check that the packet corresponds to the target socket. It is identified
  // by the cookie stored in the `target_cookie` map.
  int index = 0;
  __u64* cookie = bpf_map_lookup_elem(&target_cookie, &index);
  if (!cookie || *cookie != bpf_get_socket_cookie(skb)) {
    return 1;
  }

  // Push a message to the ringbuf to indicate that the packet was received.
  int* entry = bpf_ringbuf_reserve(&ringbuf, 4, 0);
  if (!entry) {
    return 1;
  }

  *entry = skb->len;

  // Try calling `bpf_sk_fullsock`.
  struct bpf_sock* fullsock = skb->sk ? bpf_sk_fullsock(skb->sk) : 0;
  if (fullsock) {
    *entry += fullsock->protocol;
  }

  bpf_ringbuf_submit(entry, 0);

  return 1;
}

int sock_test_prog(struct bpf_sock* sock) {
  int index = 0;
  int* counter = bpf_map_lookup_elem(&count, &index);
  if (!counter) {
    return 1;
  }

  __sync_fetch_and_add(counter, 1);

  return 1;
}