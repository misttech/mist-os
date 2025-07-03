// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// This file defines eBPF programs used for tests in Netstack3. Currently
// it has to be compiled manually by running `compile.sh` in the same
// directory as this file.

#include <sys/endian.h>

#include <linux/if_ether.h>

#include "bpf_helpers.h"

// Struct stored in the `result_array` in order to pass results to the test.
// It must match `TestResult` struct  used by the `skb_test_prog` test in
// `src/connectivity/network/netstack3/src/bindings/bpf.rs` .
// LINT.IfChange
struct test_result {
  __u64 cookie;
  __u32 uid;
  __u32 ifindex;
  __u32 proto;
  __u8 ip_proto;
};
// LINT.ThenChange(//src/connectivity/network/netstack3/src/bindings/bpf.rs)

SECTION("maps")
struct bpf_map_def result_array = {
    .type = BPF_MAP_TYPE_ARRAY,
    .key_size = sizeof(int),
    .value_size = sizeof(struct test_result),
    .max_entries = 1,
    .map_flags = BPF_F_NO_PREALLOC,
};

#define BPF_HDR_START_NET 1

int skb_test_prog(struct __sk_buff *skb) {
  struct test_result result = {};

  result.uid = bpf_get_socket_uid(skb);
  result.cookie = bpf_get_socket_cookie(skb);
  result.proto = ntohs(skb->protocol);
  result.ifindex = skb->ifindex;

  if (skb->protocol == htons(ETH_P_IP)) {
    bpf_skb_load_bytes_relative(skb, 9, &result.ip_proto, 2, BPF_HDR_START_NET);
  } else if (skb->protocol == htons(ETH_P_IPV6)) {
    bpf_skb_load_bytes_relative(skb, 6, &result.ip_proto, 1, BPF_HDR_START_NET);
  }

  int index = 0;
  bpf_map_update_elem(&result_array, &index, &result, 0);

  return 1;
}
