// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_CPU_H_
#define LIB_ZBI_FORMAT_CPU_H_

#include <stdint.h>

#define ZBI_MAX_SMT 4

// These are Used in the flags field of zbi_topology_processor_t.

// This is the processor that boots the system and the last to be shutdown.
#define ZBI_TOPOLOGY_PROCESSOR_PRIMARY 0b1

// This is the processor that handles all interrupts, some architectures will
// not have one.
#define ZBI_TOPOLOGY_PROCESSOR_INTERRUPT 0b10

#define ZBI_TOPOLOGY_NO_PARENT 0xFFFF

typedef enum {
  ZBI_TOPOLOGY_ARCH_UNDEFINED = 0,  // Intended primarily for testing.
  ZBI_TOPOLOGY_ARCH_X86 = 1,
  ZBI_TOPOLOGY_ARCH_ARM = 2,
  ZBI_TOPOLOGY_ARCH_RISCV = 3,
} zbi_topology_architecture_t;

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
} zbi_topology_arm_info_t;

typedef struct {
  // Indexes here correspond to the logical_ids index for the thread.
  uint32_t apic_ids[ZBI_MAX_SMT];
  uint32_t apic_id_count;
} zbi_topology_x86_info_t;

typedef struct {
  // ID that represents this CPU in SBI.
  uint64_t hart_id;
} zbi_topology_riscv_info_t;

typedef struct {
  uint16_t logical_ids[ZBI_MAX_SMT];
  uint8_t logical_id_count;

  uint16_t flags;

  // Should be one of zbi_topology_arm_info_t.
  // If UNDEFINED then nothing will be set in arch_info.
  uint8_t architecture;
  union {
    zbi_topology_arm_info_t arm;
    zbi_topology_x86_info_t x86;
    zbi_topology_riscv_info_t riscv;
  } architecture_info;

} zbi_topology_processor_t;

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

typedef struct {
  // Starting and ending memory addresses of this numa region.
  uint64_t start_address;
  uint64_t end_address;
} zbi_topology_numa_region_t;

typedef enum {
  ZBI_TOPOLOGY_ENTITY_UNDEFINED = 0,  // Unused default.
  ZBI_TOPOLOGY_ENTITY_PROCESSOR = 1,
  ZBI_TOPOLOGY_ENTITY_CLUSTER = 2,
  ZBI_TOPOLOGY_ENTITY_CACHE = 3,
  ZBI_TOPOLOGY_ENTITY_DIE = 4,
  ZBI_TOPOLOGY_ENTITY_SOCKET = 5,
  ZBI_TOPOLOGY_ENTITY_POWER_PLANE = 6,
  ZBI_TOPOLOGY_ENTITY_NUMA_REGION = 7,
} zbi_topology_entity_type_t;

// The ZBI_TYPE_CPU_TOPOLOGY consists of an array of zbi_topology_node_t,
// giving a flattened tree-like description of the CPU configuration
// according to the zbi_topology_entity_type_t hierarchy.
typedef struct {
  // Should be one of zbi_topology_entity_type_t.
  uint8_t entity_type;
  uint16_t parent_index;
  union {
    zbi_topology_processor_t processor;
    zbi_topology_cluster_t cluster;
    zbi_topology_numa_region_t numa_region;
    zbi_topology_cache_t cache;
  } entity;
} zbi_topology_node_t;

#endif  // LIB_ZBI_FORMAT_CPU_H_
