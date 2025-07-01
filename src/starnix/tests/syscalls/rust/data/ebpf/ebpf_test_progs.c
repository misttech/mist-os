// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bpf_helpers.h"

// Global variable that will be stored in a .data section (which is a BPF map).
static volatile __u64 global_counter1 = 0;
static volatile __u64 global_counter2 = 0;

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

// Struct stored in the `test_result` map in order to pass results to the test.
// It must match `TestResult` struct in
// `src/starnix/tests/syscalls/rust/src/ebpf.rs` .
// LINT.IfChange
struct test_result {
  __u64 uid_gid;
  __u64 pid_tgid;
};
// LINT.ThenChange(//src/starnix/tests/syscalls/rust/src/ebpf.rs)

SECTION("maps")
struct bpf_map_def test_result = {
    .type = BPF_MAP_TYPE_ARRAY,
    .key_size = 4,
    .value_size = sizeof(struct test_result),
    .max_entries = 1,
    .map_flags = 0,
};

int skb_test_prog(struct __sk_buff* skb) {
  // Check that the packet corresponds to the target socket. It is identified
  // by the cookie stored in the `target_cookie` map.
  int zero = 0;
  __u64* cookie = bpf_map_lookup_elem(&target_cookie, &zero);
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

int sock_create_prog(struct bpf_sock* sock) {
  // Increment the global counter.
  __sync_fetch_and_add(&global_counter1, 1);
  __sync_fetch_and_add(&global_counter2, 2);

  int zero = 0;
  int* counter = bpf_map_lookup_elem(&count, &zero);
  if (!counter) {
    return 1;
  }

  __sync_fetch_and_add(counter, 1);

  return 1;
}

int sock_release_prog(struct bpf_sock* sock) {
  // Check that the packet corresponds to the target socket. It is identified
  // by the cookie stored in the `target_cookie` map.
  int zero = 0;
  __u64* cookie = bpf_map_lookup_elem(&target_cookie, &zero);
  if (!cookie || *cookie != bpf_get_sk_cookie(sock)) {
    return 1;
  }

  int* counter = bpf_map_lookup_elem(&count, &zero);
  if (!counter) {
    return 1;
  }

  __sync_fetch_and_add(counter, 1);

  struct test_result result = {
      .uid_gid = bpf_get_current_uid_gid(),
      .pid_tgid = bpf_get_current_pid_tgid(),
  };
  bpf_map_update_elem(&test_result, &zero, &result, 0);

  return 1;
}
