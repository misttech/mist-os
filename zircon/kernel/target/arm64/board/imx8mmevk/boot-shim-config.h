// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_TARGET_ARM64_BOARD_IMX8MMEVK_BOOT_SHIM_CONFIG_H_
#define ZIRCON_KERNEL_TARGET_ARM64_BOARD_IMX8MMEVK_BOOT_SHIM_CONFIG_H_

#include <lib/zbi-format/board.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/internal/deprecated-cpu.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>

#define PRINT_ZBI 1
#define HAS_DEVICE_TREE 0

static const zbi_mem_range_t mem_config[] = {
    {
        .type = ZBI_MEM_TYPE_RAM,
        .paddr = 0x40000000,
        .length = 0x7E000000,  // 2GB - 32MB for optee
    },
    {
        .type = ZBI_MEM_TYPE_RESERVED,
        .paddr = 0xBE000000,
        .length = 0x2000000,  // 32MB for optee
    },
    {
        .type = ZBI_MEM_TYPE_PERIPHERAL,
        .paddr = 0,
        .length = 0x40000000,
    },
};

static const zbi_dcfg_simple_t uart_driver = {
    .mmio_phys = 0x30890000,
    .irq = 59,
};

static const zbi_dcfg_arm_gic_v3_driver_t gicv3_driver = {
    .mmio_phys = 0x38800000,
    .gicd_offset = 0x00000,
    .gicr_offset = 0x80000,
    .gicr_stride = 0x20000,
    .ipi_base = 9,
};

static const zbi_dcfg_arm_psci_driver_t psci_driver = {
    .use_hvc = false,
};

static const zbi_dcfg_arm_generic_timer_driver_t timer_driver = {
    .irq_phys = 30,
    .irq_virt = 27,
    .freq_override = 8000000,
};

static const zbi_platform_id_t platform_id = {
    .vid = PDEV_VID_NXP,
    .pid = PDEV_PID_IMX8MMEVK,
    .board_name = "imx8mmevk",
};

static void add_cpu_topology(zbi_header_t* zbi) {
#define TOPOLOGY_CPU_COUNT 4
  zbi_topology_node_v2_t nodes[TOPOLOGY_CPU_COUNT];

  for (uint8_t index = 0; index < TOPOLOGY_CPU_COUNT; index++) {
    nodes[index] = (zbi_topology_node_v2_t){
        .entity_type = ZBI_TOPOLOGY_ENTITY_V2_PROCESSOR,
        .parent_index = ZBI_TOPOLOGY_NO_PARENT,
        .entity =
            {
                .processor =
                    {
                        .logical_ids = {index},
                        .logical_id_count = 1,
                        .flags = index == 0 ? ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY
                                            : (zbi_topology_processor_flags_t)0,
                        .architecture = ZBI_TOPOLOGY_ARCHITECTURE_V2_ARM64,
                        .architecture_info =
                            {
                                .arm64 =
                                    {
                                        .cpu_id = index,
                                        .gic_id = index,
                                    },
                            },
                    },
            },
    };
  }

  append_boot_item(zbi, ZBI_TYPE_DEPRECATED_CPU_TOPOLOGY_V2, sizeof(zbi_topology_node_v2_t), &nodes,
                   sizeof(zbi_topology_node_v2_t) * TOPOLOGY_CPU_COUNT);
}

static void append_board_boot_item(zbi_header_t* zbi) {
  // add cpu topology
  add_cpu_topology(zbi);

  // add kernel drivers
  append_boot_item(zbi, ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_IMX_UART, &uart_driver,
                   sizeof(uart_driver));

  append_boot_item(zbi, ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_ARM_GIC_V3, &gicv3_driver,
                   sizeof(gicv3_driver));

  append_boot_item(zbi, ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_ARM_PSCI, &psci_driver,
                   sizeof(psci_driver));

  append_boot_item(zbi, ZBI_TYPE_KERNEL_DRIVER, ZBI_KERNEL_DRIVER_ARM_GENERIC_TIMER, &timer_driver,
                   sizeof(timer_driver));

  // add platform ID
  append_boot_item(zbi, ZBI_TYPE_PLATFORM_ID, 0, &platform_id, sizeof(platform_id));
}

#endif  // ZIRCON_KERNEL_TARGET_ARM64_BOARD_IMX8MMEVK_BOOT_SHIM_CONFIG_H_
