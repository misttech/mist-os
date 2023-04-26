// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_INTERNAL_CPU_H_
#define LIB_ZBI_FORMAT_INTERNAL_CPU_H_

#include <stdint.h>

typedef struct {
  // Number of CPU cores in the cluster.
  uint32_t cpu_count;

  // Reserved for future use.  Set to 0.
  uint32_t type;
  uint32_t flags;
  uint32_t reserved;
} zbi_cpu_cluster_t;

// ZBI_TYPE_CPU_CONFIG payload.
//
// zbi_header_t.length must equal
// ```
// zbi_cpu_config_t.cluster_count * sizeof(zbi_cpu_cluster_t)
// ```
//
// This item type has been deprecated in favour of ZBI_TYPE_CPU_TOPOLOGY.
typedef struct {
  // Number of zbi_cpu_cluster_t entries following this header.
  uint32_t cluster_count;

  // Reserved for future use.  Set to 0.
  uint32_t reserved[3];

  // cluster_count entries follow.
  zbi_cpu_cluster_t clusters[];
} zbi_cpu_config_t;

#endif  // LIB_ZBI_FORMAT_INTERNAL_CPU_H_
