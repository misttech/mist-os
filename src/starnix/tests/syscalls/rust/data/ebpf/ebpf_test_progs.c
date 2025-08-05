// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bpf_helpers.h"

const int SO_SNDBUF = 7;

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

// The following definitions should match the values use by the tests.

// LINT.IfChange
// Struct stored in the `test_result` map in order to pass results to the test.
struct test_result {
  __u64 uid_gid;
  __u64 pid_tgid;

  __u64 optlen;
  __u64 optval_size;

  __u32 sockaddr_family;
  __u32 sockaddr_port;
  __u32 sockaddr_ip[4];
};

// Global variable that will be stored in a .data section (which is a BPF map).
static volatile __u64 global_counter1 = 0;
static volatile __u64 global_counter2 = 0;

const int TEST_SOCK_OPT = 12345;
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
  if (!cookie || *cookie != bpf_get_socket_cookie(sock)) {
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

int setsockopt_prog(struct bpf_sockopt* sockopt) {
  if (sockopt->optname == TEST_SOCK_OPT) {
    __u64 buffer_size = sockopt->optval_end - sockopt->optval;
    struct test_result result = {
        .optlen = sockopt->optlen,
        .optval_size = buffer_size,
    };
    int zero = 0;
    bpf_map_update_elem(&test_result, &zero, &result, 0);

    char v = 0;
    if (sockopt->optval_end > sockopt->optval + sizeof(char)) {
      v = *(char*)sockopt->optval;
    }

    switch (v) {
      case 0:
        // set `optlen=-1` to bypass the kernels implementation of the syscall.
        sockopt->optlen = -1;
        break;

      case 1:
        // Increase optlen beyond the buffer size. This is expected to result in EFAULT.
        sockopt->optlen = buffer_size + 1;
        break;

      case 2:
        // Returning 0 should result in EPERM.
        return 0;
    }

    return 1;
  }

  if (sockopt->optname == SO_SNDBUF) {
    if (sockopt->optval_end < sockopt->optval + sizeof(int)) {
      return 1;
    }
    int* v = (int*)sockopt->optval;

    // Override the value.
    if (*v == 55555) {
      *v = 66666;
      sockopt->optlen = 4;
    } else {
      sockopt->optlen = 0;
    }
    return 1;
  }

  sockopt->optlen = 0;
  return 1;
}

int getsockopt_prog(struct bpf_sockopt* sockopt) {
  __u64 buffer_size = sockopt->optval_end - sockopt->optval;

  if (sockopt->optname == TEST_SOCK_OPT) {
    switch (buffer_size) {
      case 2:
        // Fail with the original error.
        break;
      case 3:
        // If the program returns 0 then the original error code should
        // still be returned to the user (instead of EPERM).
        return 0;
      case 4:
        // Override an error set by the syscall implementation.
        sockopt->retval = 0;
        if (sockopt->optval + 4 <= sockopt->optval_end) {
          *(int*)sockopt->optval = 42;
        }
        sockopt->optlen = 4;
        break;
      case 5:
        // `getsockopt` is not allowed to set `optlen = -1`. The original
        // ENOPROTOOPT should be returned.
        sockopt->optlen = -1;
        break;
    }

    return 1;
  }

  if (sockopt->optname == SO_SNDBUF) {
    switch (buffer_size) {
      case 55:
        if (sockopt->optval + 8 < sockopt->optval_end) {
          // Try overriding result with a larger value.
          *(__u64*)sockopt->optval = 0x1234567890abcdef;
        }
        sockopt->optlen = 8;
        break;

      case 56:
        // Reject the call should result in EPERM.
        return 0;

      case 57:
        // `getsockopt` is not allowed to set `optlen = -1`. Should result in
        // EFAULT.
        sockopt->optlen = -1;
        break;

      case 58:
        // Try changing retval to an error.
        sockopt->retval = -5;
        break;
    }
  }

  return 1;
}

int udprecv6_prog(struct bpf_sock_addr* sockaddr) {
  // Check that the packet corresponds to the target socket. It is identified
  // by the cookie stored in the `target_cookie` map.
  int zero = 0;
  __u64* cookie = bpf_map_lookup_elem(&target_cookie, &zero);
  if (!cookie || *cookie != bpf_get_socket_cookie(sockaddr)) {
    return 1;
  }

  int* counter = bpf_map_lookup_elem(&count, &zero);
  if (!counter) {
    return 1;
  }

  __sync_fetch_and_add(counter, 1);

  struct test_result result = {
      .sockaddr_port = sockaddr->user_port,
      .sockaddr_family = sockaddr->user_family,
  };
  result.sockaddr_ip[0] = sockaddr->user_ip6[0];
  result.sockaddr_ip[1] = sockaddr->user_ip6[1];
  result.sockaddr_ip[2] = sockaddr->user_ip6[2];
  result.sockaddr_ip[3] = sockaddr->user_ip6[3];
  bpf_map_update_elem(&test_result, &zero, &result, 0);

  return 1;
}

int udpsend4_prog(struct bpf_sock_addr* sockaddr) {
  // Check that the packet corresponds to the target socket. It is identified
  // by the cookie stored in the `target_cookie` map.
  int zero = 0;
  __u64* cookie = bpf_map_lookup_elem(&target_cookie, &zero);
  if (!cookie || *cookie != bpf_get_socket_cookie(sockaddr)) {
    return 1;
  }

  int* counter = bpf_map_lookup_elem(&count, &zero);
  if (!counter) {
    return 1;
  }

  __sync_fetch_and_add(counter, 1);

  struct test_result result = {
      .sockaddr_port = sockaddr->user_port,
      .sockaddr_family = sockaddr->user_family,
  };
  result.sockaddr_ip[0] = sockaddr->user_ip4;
  bpf_map_update_elem(&test_result, &zero, &result, 0);

  // Fail sendmsg() with EFAIL.
  return 0;
}
