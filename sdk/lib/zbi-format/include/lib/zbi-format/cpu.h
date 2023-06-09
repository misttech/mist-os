// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_CPU_H_
#define LIB_ZBI_FORMAT_CPU_H_

#include <stdint.h>

#define ZBI_MAX_SMT 4

typedef uint16_t zbi_topology_processor_flags_t;

// This is the processor that boots the system and the last to be shutdown.
#define ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY ((zbi_topology_processor_flags_t)(1u << 0))

// This is the processor that handles all interrupts, some architectures will
// not have one.
#define ZBI_TOPOLOGY_PROCESSOR_FLAGS_INTERRUPT ((zbi_topology_processor_flags_t)(1u << 1))

typedef struct {
  // Cluster ids for each level, one being closest to the cpu.
  // These map to aff1, aff2, and aff3 values in the ARM registers.
  uint8_t cluster_1_id;
  uint8_t cluster_2_id;
  uint8_t cluster_3_id;

  // Id of the cpu inside of the bottom-most cluster, aff0 value.
  uint8_t cpu_id;

  // The GIC interface number for this processor.
  // In GIC v3+ this is not necessary as the processors are addressed by their
  // affinity routing (all cluster ids followed by cpu_id).
  uint8_t gic_id;
} zbi_topology_arm64_info_t;

typedef struct {
  // Indexes here correspond to the logical_ids index for the thread.
  uint32_t apic_ids[ZBI_MAX_SMT];
  uint32_t apic_id_count;
} zbi_topology_x64_info_t;

typedef struct {
  // ID that represents this CPU in SBI.
  uint64_t hart_id;
} zbi_topology_riscv64_info_t;

typedef struct {
  // Relative performance level of this processor in the system. The value is
  // interpreted as the performance of this processor relative to the maximum
  // performance processor in the system. No specific values are required for
  // the performance level, only that the following relationship holds:
  //
  //   Pmax is the value of performance_class for the maximum performance
  //   processor in the system, operating at its maximum operating point.
  //
  //   P is the value of performance_class for this processor, operating at
  //   its maximum operating point.
  //
  //   R is the performance ratio of this processor to the maximum performance
  //   processor in the system in the range (0.0, 1.0].
  //
  //   R = (P + 1) / (Pmax + 1)
  //
  // If accuracy is limited, choose a conservative value that slightly under-
  // estimates the performance of lower-performance processors.
  uint8_t performance_class;
} zbi_topology_cluster_t;

typedef struct {
  // Unique id of this cache node. No other semantics are assumed.
  uint32_t cache_id;
} zbi_topology_cache_t;

#define ZBI_TOPOLOGY_NO_PARENT ((uint16_t)(0xffffu))

#endif  // LIB_ZBI_FORMAT_CPU_H_
