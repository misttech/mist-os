// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <linux/bpf.h>

static void *(*const bpf_map_lookup_elem)(void *map,
                                          const void *key) = (void *)BPF_FUNC_map_lookup_elem;
static long (*const bpf_map_update_elem)(void *map, const void *key, const void *value,
                                         __u64 flags) = (void *)BPF_FUNC_map_update_elem;
static long (*const bpf_map_delete_elem)(void *map,
                                         const void *key) = (void *)BPF_FUNC_map_delete_elem;
static __u64 (*bpf_get_socket_cookie)(struct __sk_buff *skb) = (void *)BPF_FUNC_get_socket_cookie;
static __u64 (*bpf_get_sk_cookie)(struct bpf_sock *sk) = (void *)BPF_FUNC_get_socket_cookie;
static __u32 (*bpf_get_socket_uid)(struct __sk_buff *skb) = (void *)BPF_FUNC_get_socket_uid;
static int (*bpf_skb_load_bytes_relative)(const struct __sk_buff *skb, int off, void *to, int len,
                                          int start_hdr) = (void *)BPF_FUNC_skb_load_bytes_relative;
static void *(*const bpf_ringbuf_reserve)(void *ringbuf, __u64 size,
                                          __u64 flags) = (void *)BPF_FUNC_ringbuf_reserve;
static void (*const bpf_ringbuf_submit)(void *data, __u64 flags) = (void *)BPF_FUNC_ringbuf_submit;
static void (*const bpf_ringbuf_discard)(void *data,
                                         __u64 flags) = (void *)BPF_FUNC_ringbuf_discard;

struct bpf_map_def {
  enum bpf_map_type type;
  unsigned int key_size;
  unsigned int value_size;
  unsigned int max_entries;
  unsigned int map_flags;
};

#define SECTION(name) __attribute__((section(name), used))
