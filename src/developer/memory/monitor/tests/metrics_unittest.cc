// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/monitor/metrics.h"

#include <lib/async/cpp/executor.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <zircon/time.h>

#include <gtest/gtest.h>
#include <src/cobalt/bin/testing/stub_metric_event_logger.h>

#include "src/developer/memory/metrics/capture.h"
#include "src/developer/memory/metrics/tests/test_utils.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

using namespace memory;

using cobalt_registry::MemoryMigratedMetricDimensionBucket;

namespace monitor {
namespace {
const std::vector<BucketMatch> kBucketMatches = {
    {"ZBI Buffer", ".*", "uncompressed-bootfs", MemoryMigratedMetricDimensionBucket::ZbiBuffer},
    // Memory used with the GPU or display hardware.
    {"Graphics", ".*",
     "magma_create_buffer|Mali "
     ".*|Magma.*|ImagePipe2Surface.*|GFXBufferCollection.*|ScenicImageMemory|Display.*|"
     "CompactImage.*|GFX Device Memory.*",
     MemoryMigratedMetricDimensionBucket::Graphics},
    // Unused protected pool memory.
    {"ProtectedPool", "driver_host", "SysmemAmlogicProtectedPool",
     MemoryMigratedMetricDimensionBucket::ProtectedPool},
    // Unused contiguous pool memory.
    {"ContiguousPool", "driver_host", "SysmemContiguousPool",
     MemoryMigratedMetricDimensionBucket::ContiguousPool},
    {"Fshost", "fshost.cm", ".*", MemoryMigratedMetricDimensionBucket::Fshost},
    {"Minfs", ".*minfs", ".*", MemoryMigratedMetricDimensionBucket::Minfs},
    {"BlobfsInactive", ".*blobfs", "inactive-blob-.*",
     MemoryMigratedMetricDimensionBucket::BlobfsInactive},
    {"Blobfs", ".*blobfs", ".*", MemoryMigratedMetricDimensionBucket::Blobfs},
    {"FlutterApps", "io\\.flutter\\..*", "dart.*",
     MemoryMigratedMetricDimensionBucket::FlutterApps},
    {"Flutter", "io\\.flutter\\..*", ".*", MemoryMigratedMetricDimensionBucket::Flutter},
    {"Web", "web_engine_exe:.*", ".*", MemoryMigratedMetricDimensionBucket::Web},
    {"Kronk", "kronk.cm", ".*", MemoryMigratedMetricDimensionBucket::Kronk},
    {"Scenic", "scenic.cm", ".*", MemoryMigratedMetricDimensionBucket::Scenic},
    {"Amlogic", "driver_host", ".*", MemoryMigratedMetricDimensionBucket::Amlogic},
    {"Netstack", "netstack.cm", ".*", MemoryMigratedMetricDimensionBucket::Netstack},
    {"Pkgfs", "pkgfs", ".*", MemoryMigratedMetricDimensionBucket::Pkgfs},
    {"Cast", "cast_agent.cm", ".*", MemoryMigratedMetricDimensionBucket::Cast},
    {"Archivist", "archivist.cm", ".*", MemoryMigratedMetricDimensionBucket::Archivist},
    {"Cobalt", "cobalt.cm", ".*", MemoryMigratedMetricDimensionBucket::Cobalt},
    {"Audio", "audio_core.cm", ".*", MemoryMigratedMetricDimensionBucket::Audio},
    {"Context", "context_provider.cm", ".*", MemoryMigratedMetricDimensionBucket::Context},
};

class MetricsUnitTest : public gtest::RealLoopFixture {
 public:
  MetricsUnitTest() : executor_(dispatcher()) {}

 protected:
  // Run a promise to completion on the default async executor.
  void RunPromiseToCompletion(fpromise::promise<> promise) {
    bool done = false;
    executor_.schedule_task(std::move(promise).and_then([&]() { done = true; }));
    RunLoopUntilIdle();
    ASSERT_TRUE(done);
  }
  std::vector<CaptureTemplate> Template() {
    return std::vector<CaptureTemplate>{{
        .time = zx_nsec_from_duration(zx_duration_from_hour(7)),
        .kmem =
            {
                .total_bytes = 2000,
                .free_bytes = 800,
                .wired_bytes = 60,
                .total_heap_bytes = 200,
                .free_heap_bytes = 80,
                .vmo_bytes = 900,
                .mmu_overhead_bytes = 60,
                .ipc_bytes = 10,
                .other_bytes = 20,
            },
        .vmos =
            {
                {.koid = 1,
                 .name = "uncompressed-bootfs",
                 .committed_bytes = 1,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 2,
                 .name = "magma_create_buffer",
                 .committed_bytes = 2,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 3,
                 .name = "SysmemAmlogicProtectedPool",
                 .committed_bytes = 3,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 4,
                 .name = "SysmemContiguousPool",
                 .committed_bytes = 4,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 5,
                 .name = "test",
                 .committed_bytes = 5,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 6,
                 .name = "test",
                 .committed_bytes = 6,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 7,
                 .name = "test",
                 .committed_bytes = 7,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 8,
                 .name = "dart",
                 .committed_bytes = 8,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 9,
                 .name = "test",
                 .committed_bytes = 9,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 10,
                 .name = "test",
                 .committed_bytes = 10,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 11,
                 .name = "test",
                 .committed_bytes = 11,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 12,
                 .name = "test",
                 .committed_bytes = 12,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 13,
                 .name = "test",
                 .committed_bytes = 13,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 14,
                 .name = "test",
                 .committed_bytes = 14,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 15,
                 .name = "test",
                 .committed_bytes = 15,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 16,
                 .name = "test",
                 .committed_bytes = 16,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 17,
                 .name = "test",
                 .committed_bytes = 17,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 18,
                 .name = "test",
                 .committed_bytes = 18,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 19,
                 .name = "test",
                 .committed_bytes = 19,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 20,
                 .name = "test",
                 .committed_bytes = 20,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 21,
                 .name = "test",
                 .committed_bytes = 21,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
                {.koid = 22,
                 .name = "test",
                 .committed_bytes = 22,
                 .committed_fractional_scaled_bytes = UINT64_MAX},
            },
        .processes =
            {
                {.koid = 1, .name = "bin/bootsvc", .vmos = {1}},
                {.koid = 2, .name = "test", .vmos = {2}},
                {.koid = 3, .name = "driver_host", .vmos = {3, 4}},
                {.koid = 4, .name = "fshost.cm", .vmos = {5}},
                {.koid = 5, .name = "/boot/bin/minfs", .vmos = {6}},
                {.koid = 6, .name = "/boot/bin/blobfs", .vmos = {7}},
                {.koid = 7, .name = "io.flutter.product_runner.aot", .vmos = {8, 9}},
                {.koid = 8, .name = "web_engine_exe:renderer", .vmos = {10}},
                {.koid = 9, .name = "web_engine_exe:gpu", .vmos = {11}},
                {.koid = 10, .name = "kronk.cm", .vmos = {12}},
                {.koid = 11, .name = "scenic.cm", .vmos = {13}},
                {.koid = 12, .name = "driver_host", .vmos = {14}},
                {.koid = 13, .name = "netstack.cm", .vmos = {15}},
                {.koid = 14, .name = "pkgfs", .vmos = {16}},
                {.koid = 15, .name = "cast_agent.cm", .vmos = {17}},
                {.koid = 16, .name = "archivist.cm", .vmos = {18}},
                {.koid = 17, .name = "cobalt.cm", .vmos = {19}},
                {.koid = 18, .name = "audio_core.cm", .vmos = {20}},
                {.koid = 19, .name = "context_provider.cm", .vmos = {21}},
                {.koid = 20, .name = "new", .vmos = {22}},
            },
    }};
  }
  async::Executor executor_;
  sys::testing::ComponentContextProvider context_provider_;
};

TEST_F(MetricsUnitTest, Inspect) {
  CaptureSupplier cs(Template());
  cobalt::StubMetricEventLogger_Sync logger;
  inspect::ComponentInspector inspector(dispatcher(), inspect::PublishOptions{});
  Metrics m(
      kBucketMatches, zx::min(5), dispatcher(), &inspector, &logger,
      [&cs](Capture* c) {
        return cs.GetCapture(c, CaptureLevel::VMO, true /*use_capture_supplier_time*/);
      },
      [](const Capture& c, Digest* d) { Digester(kBucketMatches).Digest(c, d); });
  RunLoopUntil([&cs] { return cs.empty(); });

  // [START get_hierarchy]
  fpromise::result<inspect::Hierarchy> hierarchy;
  RunPromiseToCompletion(inspect::ReadFromInspector(inspector.inspector())
                             .then([&](fpromise::result<inspect::Hierarchy>& result) {
                               hierarchy = std::move(result);
                             }));
  ASSERT_TRUE(hierarchy.is_ok());
  // [END get_hierarchy]

  // [START assertions]
  auto* metric_node = hierarchy.value().GetByPath({Metrics::kInspectPlatformNodeName});
  ASSERT_TRUE(metric_node);

  auto* metric_memory = metric_node->GetByPath({Metrics::kMemoryNodeName});
  ASSERT_TRUE(metric_memory);

  auto* usage_readings = metric_memory->node().get_property<inspect::UintPropertyValue>("Graphics");
  ASSERT_TRUE(usage_readings);
  EXPECT_EQ(2u, usage_readings->value());
}

TEST_F(MetricsUnitTest, All) {
  CaptureSupplier cs(Template());
  cobalt::StubMetricEventLogger_Sync logger;
  inspect::ComponentInspector inspector(dispatcher(), inspect::PublishOptions{});
  Metrics m(
      kBucketMatches, zx::msec(10), dispatcher(), &inspector, &logger,
      [&cs](Capture* c) {
        return cs.GetCapture(c, CaptureLevel::VMO, true /*use_capture_supplier_time*/);
      },
      [](const Capture& c, Digest* d) { Digester(kBucketMatches).Digest(c, d); });
  RunLoopUntil([&cs] { return cs.empty(); });
  // memory metric: 20 buckets + 4 (Orphaned, Kernel, Undigested and Free buckets)  +
  // memory_general_breakdown metric: 10 +
  // memory_leak metric: 10
  // = 44
  EXPECT_EQ(44U, logger.logged_events().size());
  using Breakdown = cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown;
  using Breakdown2 = cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown;
  for (const auto& metric_event : logger.logged_events()) {
    EXPECT_EQ(fuchsia::metrics::MetricEventPayload::Tag::kIntegerValue,
              metric_event.payload.Which());
    switch (metric_event.metric_id) {
      case cobalt_registry::kMemoryMigratedMetricId:
        ASSERT_EQ(1u, metric_event.event_codes.size());
        switch (metric_event.event_codes[0]) {
          case MemoryMigratedMetricDimensionBucket::ZbiBuffer:
            EXPECT_EQ(1u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Graphics:
            EXPECT_EQ(2u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::ProtectedPool:
            EXPECT_EQ(3u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::ContiguousPool:
            EXPECT_EQ(4u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Fshost:
            EXPECT_EQ(5u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Minfs:
            EXPECT_EQ(6u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Blobfs:
            EXPECT_EQ(7u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::FlutterApps:
            EXPECT_EQ(8u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Flutter:
            EXPECT_EQ(9u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Web:
            EXPECT_EQ(21u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Kronk:
            EXPECT_EQ(12u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Scenic:
            EXPECT_EQ(13u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Amlogic:
            EXPECT_EQ(14u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Netstack:
            EXPECT_EQ(15u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Pkgfs:
            EXPECT_EQ(16u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Cast:
            EXPECT_EQ(17u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Archivist:
            EXPECT_EQ(18u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Cobalt:
            EXPECT_EQ(19u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Audio:
            EXPECT_EQ(20u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Context:
            EXPECT_EQ(21u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Undigested:
            EXPECT_EQ(22, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Orphaned:
            // 900 kmem.vmo - (1 + 2 + 3 + ... + 22) vmo digested in buckets = 647
            EXPECT_EQ(647, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Kernel:
            // 60 wired + 200 total_heap + 60 mmu_overhead + 10 ipc + 20 other = 350
            EXPECT_EQ(350u, metric_event.payload.integer_value());
            break;
          case MemoryMigratedMetricDimensionBucket::Free:
            EXPECT_EQ(800u, metric_event.payload.integer_value());
            break;
          default:
            ADD_FAILURE();
            break;
        }
        break;
      case cobalt_registry::kMemoryGeneralBreakdownMigratedMetricId:
        ASSERT_EQ(1u, metric_event.event_codes.size());
        switch (metric_event.event_codes[0]) {
          case Breakdown::TotalBytes:
            EXPECT_EQ(2000u, metric_event.payload.integer_value());
            break;
          case Breakdown::UsedBytes:
            EXPECT_EQ(1200u, metric_event.payload.integer_value());
            break;
          case Breakdown::VmoBytes:
            EXPECT_EQ(900u, metric_event.payload.integer_value());
            break;
          case Breakdown::FreeBytes:
            EXPECT_EQ(800u, metric_event.payload.integer_value());
            break;
          default:
            EXPECT_TRUE(metric_event.payload.integer_value() <= 200);
            break;
        }
        break;
      case cobalt_registry::kMemoryLeakMigratedMetricId:
        ASSERT_EQ(2u, metric_event.event_codes.size());
        ASSERT_EQ(cobalt_registry::MemoryLeakMigratedMetricDimensionTimeSinceBoot::UpSixHours,
                  metric_event.event_codes[1]);
        switch (metric_event.event_codes[0]) {
          case Breakdown2::TotalBytes:
            EXPECT_EQ(2000u, metric_event.payload.integer_value());
            break;
          case Breakdown2::UsedBytes:
            EXPECT_EQ(1200u, metric_event.payload.integer_value());
            break;
          case Breakdown2::VmoBytes:
            EXPECT_EQ(900u, metric_event.payload.integer_value());
            break;
          case Breakdown2::FreeBytes:
            EXPECT_EQ(800u, metric_event.payload.integer_value());
            break;
          default:
            EXPECT_TRUE(metric_event.payload.integer_value() <= 200);
            break;
        }
        break;
    }
  }
}

TEST_F(MetricsUnitTest, One) {
  CaptureSupplier cs({{
      .kmem =
          {
              .total_bytes = 0,
              .free_bytes = 0,
              .wired_bytes = 0,
              .total_heap_bytes = 0,
              .free_heap_bytes = 0,
              .vmo_bytes = 0,
              .mmu_overhead_bytes = 0,
              .ipc_bytes = 0,
              .other_bytes = 0,
          },
      .vmos =
          {
              {.koid = 1,
               .name = "",
               .committed_bytes = 1,
               .committed_fractional_scaled_bytes = UINT64_MAX},
          },
      .processes =
          {
              {.koid = 1, .name = "bin/bootsvc", .vmos = {1}},
          },
  }});
  cobalt::StubMetricEventLogger_Sync logger;
  inspect::ComponentInspector inspector(dispatcher(), inspect::PublishOptions{});
  Metrics m(
      kBucketMatches, zx::msec(10), dispatcher(), &inspector, &logger,
      [&cs](Capture* c) { return cs.GetCapture(c, CaptureLevel::VMO); },
      [](const Capture& c, Digest* d) { Digester(kBucketMatches).Digest(c, d); });
  RunLoopUntil([&cs] { return cs.empty(); });
  EXPECT_EQ(21U, logger.event_count());  // 1 + 10 + 10
}

TEST_F(MetricsUnitTest, Undigested) {
  CaptureSupplier cs({{
      .kmem =
          {
              .total_bytes = 0,
              .free_bytes = 0,
              .wired_bytes = 0,
              .total_heap_bytes = 0,
              .free_heap_bytes = 0,
              .vmo_bytes = 0,
              .mmu_overhead_bytes = 0,
              .ipc_bytes = 0,
              .other_bytes = 0,
          },
      .vmos =
          {
              {.koid = 1,
               .name = "uncompressed-bootfs",
               .committed_bytes = 1,
               .committed_fractional_scaled_bytes = UINT64_MAX},
              {.koid = 2,
               .name = "test",
               .committed_bytes = 2,
               .committed_fractional_scaled_bytes = UINT64_MAX},
          },
      .processes =
          {
              {.koid = 1, .name = "bin/bootsvc", .vmos = {1}},
              {.koid = 2, .name = "test", .vmos = {2}},
          },
  }});
  cobalt::StubMetricEventLogger_Sync logger;
  inspect::ComponentInspector inspector(dispatcher(), inspect::PublishOptions{});
  Metrics m(
      kBucketMatches, zx::msec(10), dispatcher(), &inspector, &logger,
      [&cs](Capture* c) { return cs.GetCapture(c, CaptureLevel::VMO); },
      [](const Capture& c, Digest* d) { Digester(kBucketMatches).Digest(c, d); });
  RunLoopUntil([&cs] { return cs.empty(); });
  EXPECT_EQ(22U, logger.event_count());  // 2 + 10 + 10
}

}  // namespace
}  // namespace monitor
