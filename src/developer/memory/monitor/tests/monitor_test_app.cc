// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>

#include "src/developer/memory/metrics/capture.h"
#include "src/developer/memory/metrics/capture_strategy.h"
#include "src/developer/memory/metrics/tests/test_utils.h"
#include "src/developer/memory/monitor/memory_monitor_config.h"
#include "src/developer/memory/monitor/monitor.h"
#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"
#include "zircon/syscalls/object.h"
namespace {
std::unique_ptr<memory::MockOS> CreateMockOS() {
  const zx_handle_t handle_process_0 = 1000;
  const zx_koid_t koid_vmo_0 = 200;
  const zx_handle_t handle_process_1 = 1001;
  static const zx_info_vmo_t _vmo_0 = {
      .koid = koid_vmo_0,
      .name = "vmo_0",
      .size_bytes = 0,
  };
  static const memory::GetInfoResponse vmo_0_info = {
      handle_process_0, ZX_INFO_PROCESS_VMOS, &_vmo_0, sizeof(_vmo_0), 1, ZX_OK};

  const zx_koid_t koid_vmo_1 = 201;

  static const zx_info_vmo_t _vmo_1 = {
      .koid = koid_vmo_1,
      .name = "vmo_0",
      .size_bytes = 0,
  };

  static const zx_info_kmem_stats_extended_t kmem_stats_ext = {
      .total_bytes = 40,
      .free_bytes = 41,
      .wired_bytes = 42,
      .total_heap_bytes = 43,
      .free_heap_bytes = 44,
      .vmo_bytes = 45,
      .vmo_pager_total_bytes = 46,
      .vmo_pager_newest_bytes = 47,
      .vmo_pager_oldest_bytes = 48,
      .vmo_discardable_locked_bytes = 49,
      .vmo_discardable_unlocked_bytes = 50,
      .mmu_overhead_bytes = 51,
      .ipc_bytes = 52,
      .other_bytes = 53,
      .vmo_reclaim_disabled_bytes = 54,
  };
  static const memory::GetInfoResponse stat_ext_resp = {.handle = 1,
                                                        .topic = ZX_INFO_KMEM_STATS_EXTENDED,
                                                        .values = &kmem_stats_ext,
                                                        .value_size = sizeof(kmem_stats_ext),
                                                        .value_count = 1,
                                                        .ret = ZX_OK};

  static const zx_info_kmem_stats_compression_t kmem_stats_cmp = {
      .uncompressed_storage_bytes = 60,
      .compressed_storage_bytes = 61,
      .compressed_fragmentation_bytes = 62,
      .compression_time = 63,
      .decompression_time = 64,
      .total_page_compression_attempts = 65,
      .failed_page_compression_attempts = 66,
      .total_page_decompressions = 67,
      .compressed_page_evictions = 68,
      .eager_page_compressions = 69,
      .memory_pressure_page_compressions = 70,
      .critical_memory_page_compressions = 71,
      .pages_decompressed_unit_ns = 72,
      .pages_decompressed_within_log_time = {73, 74, 75, 76, 77, 78, 79, 80},

  };
  const memory::GetInfoResponse zram_stat = {.handle = 1,
                                             .topic = ZX_INFO_KMEM_STATS_COMPRESSION,
                                             .values = &kmem_stats_cmp,
                                             .value_size = sizeof(kmem_stats_cmp),
                                             .value_count = 1,
                                             .ret = ZX_OK};

  const memory::GetInfoResponse vmo_1_info = {
      handle_process_1, ZX_INFO_PROCESS_VMOS, &_vmo_1, sizeof(_vmo_1), 1, ZX_OK};
  return std::make_unique<memory::MockOS>(
      memory::OsResponses{.get_info = {vmo_0_info, vmo_1_info, zram_stat, stat_ext_resp}});
}
}  // namespace

int main(int argc, const char** argv) {
  auto os = CreateMockOS();
  auto capture_maker = memory::CaptureMaker::Create(CreateMockOS()).value();
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  monitor::Monitor app(sys::ComponentContext::CreateAndServeOutgoingDirectory(), fxl::CommandLine{},
                       loop.dispatcher(), false, false, false, memory_monitor_config::Config{},
                       std::move(capture_maker));
  loop.Run();
  return 0;
}
