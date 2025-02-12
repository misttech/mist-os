// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/monitor/monitor.h"

#include <fidl/fuchsia.hardware.ram.metrics/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/cpp/time.h>
#include <lib/async/default.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <string.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <filesystem>
#include <iostream>
#include <iterator>
#include <memory>
#include <mutex>
#include <sstream>
#include <utility>

#include <soc/aml-common/aml-ram.h>
#include <trace-vthread/event_vthread.h>

#include "lib/fpromise/result.h"
#include "src/developer/memory/metrics/bucket_match.h"
#include "src/developer/memory/metrics/capture.h"
#include "src/developer/memory/metrics/printer.h"
#include "src/developer/memory/monitor/high_water.h"
#include "src/developer/memory/monitor/memory_metrics_registry.cb.h"
#include "src/developer/memory/pressure_signaler/pressure_observer.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/strings/string_number_conversions.h"

namespace monitor {

using memory::Capture;
using memory::CaptureLevel;
using memory::Digest;
using memory::Digester;
using memory::SORTED;
using memory::Summary;
using memory::TextPrinter;

const char Monitor::kTraceName[] = "memory_monitor";

namespace {
// Path to the configuration file for buckets.
const std::string kBucketConfigPath = "/config/data/buckets.json";
constexpr std::string kLoggerInspectKey = "logger";
const zx::duration kHighWaterPollFrequency = zx::sec(10);
const uint64_t kHighWaterThreshold = 10ul * 1024 * 1024;
const zx::duration kMetricsPollFrequency = zx::min(5);
const char kTraceNameHighPrecisionBandwidth[] = "memory_monitor:high_precision_bandwidth";
const char kTraceNameHighPrecisionBandwidthCamera[] =
    "memory_monitor:high_precision_bandwidth_camera";
constexpr uint64_t kMaxPendingBandwidthMeasurements = 4;
constexpr uint64_t kMemCyclesToMeasure = 792000000 / 20;                 // 50 ms on sherlock
constexpr uint64_t kMemCyclesToMeasureHighPrecision = 792000000 / 1000;  // 1 ms
// TODO(https://fxbug.dev/42125091): Get default channel information through the FIDL API.
struct RamChannel {
  const char* name;
  uint64_t mask;
};
constexpr RamChannel kRamDefaultChannels[] = {
    {.name = "cpu", .mask = aml_ram::kDefaultChannelCpu},
    {.name = "gpu", .mask = aml_ram::kDefaultChannelGpu},
    {.name = "vdec", .mask = aml_ram::kDefaultChannelVDec},
    {.name = "vpu", .mask = aml_ram::kDefaultChannelVpu},
};
constexpr RamChannel kRamCameraChannels[] = {
    {.name = "cpu", .mask = aml_ram::kDefaultChannelCpu},
    {.name = "isp", .mask = aml_ram::kPortIdMipiIsp},
    {.name = "gdc", .mask = aml_ram::kPortIdGDC},
    {.name = "ge2d", .mask = aml_ram::kPortIdGe2D},
};
uint64_t CounterToBandwidth(uint64_t counter, uint64_t frequency, uint64_t cycles) {
  return counter * frequency / cycles;
}
zx_ticks_t TimestampToTicks(zx_time_t timestamp) {
  __uint128_t temp = static_cast<__uint128_t>(timestamp) * zx_ticks_per_second() / ZX_SEC(1);
  return static_cast<zx_ticks_t>(temp);
}
fuchsia_hardware_ram_metrics::BandwidthMeasurementConfig BuildConfig(
    uint64_t cycles_to_measure, bool use_camera_channels = false) {
  fuchsia_hardware_ram_metrics::BandwidthMeasurementConfig config = {
      {.cycles_to_measure = cycles_to_measure}};
  size_t num_channels = std::size(kRamDefaultChannels);
  const auto* channels = kRamDefaultChannels;
  if (use_camera_channels) {
    num_channels = std::size(kRamCameraChannels);
    channels = kRamCameraChannels;
  }
  for (size_t i = 0; i < num_channels; i++) {
    config.channels()[i] = channels[i].mask;
  }
  return config;
}
uint64_t TotalReadWriteCycles(const fuchsia_hardware_ram_metrics::BandwidthInfo& info) {
  uint64_t total_readwrite_cycles = 0;
  for (auto& channel : info.channels()) {
    total_readwrite_cycles += channel.readwrite_cycles();
  }
  return total_readwrite_cycles;
}

std::vector<memory::BucketMatch> CreateBucketMatchesFromConfigData() {
  std::error_code _ignore;
  if (!std::filesystem::exists(kBucketConfigPath, _ignore)) {
    FX_LOGS(WARNING) << "Bucket configuration file not found; no buckets will be available.";
    return {};
  }

  std::string configuration_str;
  FX_CHECK(files::ReadFileToString(kBucketConfigPath, &configuration_str));
  auto bucket_matches = memory::BucketMatch::ReadBucketMatchesFromConfig(configuration_str);
  FX_CHECK(bucket_matches) << "Unable to read configuration: " << configuration_str;
  return std::move(*bucket_matches);
}

}  // namespace

Monitor::Monitor(const fxl::CommandLine& command_line, async_dispatcher_t* dispatcher,
                 memory_monitor_config::Config config, memory::CaptureMaker capture_maker,
                 std::optional<fidl::Client<fuchsia_memorypressure::Provider>> pressure_provider,
                 std::optional<zx_handle_t> root_job,
                 std::optional<fidl::Client<fuchsia_metrics::MetricEventLoggerFactory>> factory,
                 std::optional<fidl::Client<fuchsia_hardware_ram_metrics::Device>> ram_device)
    : capture_maker_(std::move(capture_maker)),
      high_water_(
          "/cache", kHighWaterPollFrequency, kHighWaterThreshold, dispatcher,
          [this](Capture* c, CaptureLevel l) { return capture_maker_.GetCapture(c, l); },
          [this](const Capture& c, Digest* d) { digester_.Digest(c, d); }),
      prealloc_size_(0),
      logging_(command_line.HasOption("log")),
      tracing_(false),
      delay_(zx::sec(1)),
      dispatcher_(dispatcher),
      config_(config),
      inspector_(dispatcher_, {}),
      logger_(
          dispatcher_, &high_water_,
          [this](Capture* c) { return capture_maker_.GetCapture(c, CaptureLevel::VMO); },
          [this](const Capture& c, Digest* d) { GetDigest(c, d); }, &config_,
          inspector_.root().CreateChild(kLoggerInspectKey)),
      metric_event_logger_factory_(std::move(factory)),
      bucket_matches_(CreateBucketMatchesFromConfigData()),
      digester_(bucket_matches_),
      ram_device_(std::move(ram_device)),
      level_(pressure_signaler::Level::kNumLevels) {
  if (metric_event_logger_factory_)
    CreateMetrics();

  // Expose lazy values under the root, populated from the Inspect method.
  inspector_.root().RecordLazyValues("memory_measurements", [this] {
    return fpromise::make_result_promise(fpromise::ok(Inspect()));
  });
  inspect::Node config_node = inspector_.root().CreateChild("config");
  config_.RecordInspect(&config_node);

  if (command_line.HasOption("help")) {
    PrintHelp();
    exit(EXIT_SUCCESS);
  }
  std::string delay_as_string;
  if (command_line.GetOptionValue("delay", &delay_as_string)) {
    unsigned delay_as_int;
    if (!fxl::StringToNumberWithError<unsigned>(delay_as_string, &delay_as_int)) {
      FX_LOGS(ERROR) << "Invalid value for delay: " << delay_as_string;
      exit(-1);
    }
    delay_ = zx::msec(delay_as_int);
  }
  std::string prealloc_as_string;
  if (command_line.GetOptionValue("prealloc", &prealloc_as_string)) {
    FX_LOGS(INFO) << "prealloc_string: " << prealloc_as_string;
    if (!fxl::StringToNumberWithError<uint64_t>(prealloc_as_string, &prealloc_size_)) {
      FX_LOGS(ERROR) << "Invalid value for prealloc: " << prealloc_as_string;
      exit(-1);
    }
    prealloc_size_ *= 1024ul * 1024;
    zx_status_t status = zx::vmo::create(prealloc_size_, 0, &prealloc_vmo_);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "zx::vmo::create() returns " << zx_status_get_string(status);
      exit(-1);
    }
    prealloc_vmo_.get_size(&prealloc_size_);
    uintptr_t prealloc_addr = 0;
    status = zx::vmar::root_self()->map(ZX_VM_PERM_READ, 0, prealloc_vmo_, 0, prealloc_size_,
                                        &prealloc_addr);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "zx::vmar::map() returns " << zx_status_get_string(status);
      exit(-1);
    }

    status = prealloc_vmo_.op_range(ZX_VMO_OP_COMMIT, 0, prealloc_size_, nullptr, 0);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "zx::vmo::op_range() returns " << zx_status_get_string(status);
      exit(-1);
    }
  }

  trace_observer_.Start(dispatcher_, [this] { UpdateState(); });
  if (logging_) {
    Capture capture;
    auto s = capture_maker_.GetCapture(&capture, CaptureLevel::KMEM);
    if (s != ZX_OK) {
      FX_LOGS(ERROR) << "Error getting capture: " << zx_status_get_string(s);
      exit(EXIT_FAILURE);
    }
    const auto& kmem = capture.kmem();
    FX_LOGS(INFO) << "Total: " << kmem.total_bytes << " Wired: " << kmem.wired_bytes
                  << " Total Heap: " << kmem.total_heap_bytes;
  }

  // Pressure monitoring
  if (pressure_provider) {
    auto watcher_endpoints = fidl::CreateEndpoints<fuchsia_memorypressure::Watcher>();
    auto watcher = fidl::BindServer(dispatcher_, std::move(watcher_endpoints->server), this);

    auto result = (*pressure_provider)->RegisterWatcher(std::move(watcher_endpoints->client));
    if (!result.is_ok()) {
      FX_LOGS(ERROR) << "Error registering to memory pressure changes: " << result.error_value();
      exit(-1);
    }
  }

  // Imminent OOM monitoring
  if (root_job) {
    zx_status_t status = zx_system_get_event(*root_job, ZX_SYSTEM_EVENT_IMMINENT_OUT_OF_MEMORY,
                                             &imminent_oom_event_handle_);
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "zx_system_get_event [IMMINENT-OOM] returned "
                     << zx_status_get_string(status);
      exit(-1);
    }

    // Start imminent oom monitoring on a new named thread.
    imminent_oom_loop_.StartThread("imminent-oom-loop");
    watch_task_.Post(imminent_oom_loop_.dispatcher());
  }

  // Bandwidth monitoring
  if (ram_device_) {
    PeriodicMeasureBandwidth();
  }
  SampleAndPost();
}

void Monitor::CreateMetrics() {
  fuchsia_metrics::ProjectSpec project_spec{
      {.customer_id = cobalt_registry::kCustomerId, .project_id = cobalt_registry::kProjectId}};
  auto endpoints = fidl::CreateEndpoints<fuchsia_metrics::MetricEventLogger>();
  if (endpoints.is_error()) {
    FX_LOGS(ERROR) << "Unable to create fuchsia_metrics::MetricEventLogger channels.";
    return;
  }
  (*metric_event_logger_factory_)
      ->CreateMetricEventLogger(
          {{.project_spec = project_spec, .logger = std::move(endpoints->server)}})
      .Then([this, client = std::move(endpoints->client)](const auto& result) mutable {
        if (result.is_error()) {
          FX_LOGS(ERROR) << "Unable to get metrics.Logger from factory.";
          return;
        }
        metrics_.emplace(
            bucket_matches_, kMetricsPollFrequency, dispatcher_, &inspector_,
            fidl::Client<fuchsia_metrics::MetricEventLogger>(std::move(client), dispatcher_),
            [this](Capture* c) { return capture_maker_.GetCapture(c, CaptureLevel::VMO); },
            [this](const Capture& c, Digest* d) { GetDigest(c, d); });
      });
}

void Monitor::CollectJsonStats(CollectJsonStatsRequest& request,
                               CollectJsonStatsCompleter::Sync& completer) {
  // We set |include_starnix_processes| to true to avoid any change of behavior to the current
  // clients.
  CollectJsonStatsWithOptions(std::move(request.socket()));
}

void Monitor::CollectJsonStatsWithOptions(CollectJsonStatsWithOptionsRequest& request,
                                          CollectJsonStatsWithOptionsCompleter::Sync& completer) {
  CollectJsonStatsWithOptions(std::move(*request.socket()));
}

void Monitor::CollectJsonStatsWithOptions(zx::socket socket) {
  // Capture state.
  Capture capture;

  zx_status_t capture_status;
  capture_status = capture_maker_.GetCapture(&capture, CaptureLevel::VMO);

  if (capture_status != ZX_OK) {
    FX_LOGS(ERROR) << "Error getting capture: " << zx_status_get_string(capture_status);
    return;
  }

  // Copy the bucket definition into a string
  std::string configuration_str;
  std::error_code _ignore;
  if (!std::filesystem::exists(kBucketConfigPath, _ignore)) {
    FX_LOGS(ERROR) << "Bucket configuration file not found; no buckets will be available.";
    configuration_str = "[]";
  } else {
    FX_CHECK(files::ReadFileToString(kBucketConfigPath, &configuration_str));
  }
  memory::JsonPrinter printer(socket);
  printer.PrintCaptureAndBucketConfig(capture, configuration_str);
}

void Monitor::PrintHelp() {
  std::cout << "memory_monitor [options]\n";
  std::cout << "Options:\n";
  std::cout << "  --log\n";
  std::cout << "  --prealloc=kbytes\n";
  std::cout << "  --delay=msecs\n";
}

inspect::Inspector Monitor::Inspect() {
  inspect::Inspector inspector(inspect::InspectSettings{.maximum_size = 1024ul * 1024});
  auto& root = inspector.GetRoot();

  auto high_water_string = high_water_.GetHighWater();
  if (!high_water_string.empty()) {
    root.RecordString("high_water", high_water_string);
  }

  auto high_water_digest_string = high_water_.GetHighWaterDigest();
  if (!high_water_digest_string.empty()) {
    root.RecordString("high_water_digest", high_water_digest_string);
  }

  auto previous_high_water_string = high_water_.GetPreviousHighWater();
  if (!previous_high_water_string.empty()) {
    root.RecordString("high_water_previous_boot", previous_high_water_string);
  }

  auto previous_high_water_digest_string = high_water_.GetPreviousHighWaterDigest();
  if (!previous_high_water_digest_string.empty()) {
    root.RecordString("high_water_digest_previous_boot", previous_high_water_digest_string);
  }

  root.RecordLazyValues("lazynode", [this] {
    inspect::Inspector inspector(inspect::InspectSettings{.maximum_size = 1024ul * 1024});
    auto& root = inspector.GetRoot();
    Capture capture;
    zx_status_t rc = capture_maker_.GetCapture(&capture, CaptureLevel::VMO);
    if (rc != ZX_OK) {
      FX_LOGS(ERROR) << "Failed to create capture with error: " << zx_status_get_string(rc);
      return fpromise::make_result_promise(fpromise::ok(inspector));
    }

    Summary summary(capture, Summary::kNameMatches);
    std::ostringstream summary_stream;
    TextPrinter summary_printer(summary_stream);
    summary_printer.PrintSummary(summary, CaptureLevel::VMO, SORTED);
    auto current_string = summary_stream.str();
    if (!current_string.empty()) {
      root.RecordString("current", current_string);
    }
    // Expose raw values for downstream computation.
    {
      auto values = root.CreateChild("values");
      const auto& stats = *capture.kmem_extended();
      values.CreateUint("total_bytes", stats.total_bytes, &inspector);
      values.CreateUint("free_bytes", stats.free_bytes, &inspector);
      values.CreateUint("wired_bytes", stats.wired_bytes, &inspector);
      values.CreateUint("total_heap_bytes", stats.total_heap_bytes, &inspector);
      values.CreateUint("free_heap_bytes", stats.free_heap_bytes, &inspector);
      values.CreateUint("vmo_bytes", stats.vmo_bytes, &inspector);
      values.CreateUint("vmo_pager_total_bytes", stats.vmo_pager_total_bytes, &inspector);
      values.CreateUint("vmo_pager_newest_bytes", stats.vmo_pager_newest_bytes, &inspector);
      values.CreateUint("vmo_pager_oldest_bytes", stats.vmo_pager_oldest_bytes, &inspector);
      values.CreateUint("vmo_discardable_locked_bytes", stats.vmo_discardable_locked_bytes,
                        &inspector);
      values.CreateUint("vmo_discardable_unlocked_bytes", stats.vmo_discardable_unlocked_bytes,
                        &inspector);
      values.CreateUint("mmu_overhead_bytes", stats.mmu_overhead_bytes, &inspector);
      values.CreateUint("ipc_bytes", stats.ipc_bytes, &inspector);
      values.CreateUint("other_bytes", stats.other_bytes, &inspector);
      values.CreateUint("vmo_reclaim_disabled_bytes", stats.vmo_reclaim_disabled_bytes, &inspector);
      inspector.emplace(std::move(values));
    }
    if (capture.kmem_compression()) {
      const auto& stats = *capture.kmem_compression();
      auto values = root.CreateChild("kmem_stats_compression");
      values.CreateUint("uncompressed_storage_bytes", stats.uncompressed_storage_bytes, &inspector);
      values.CreateUint("compressed_storage_bytes", stats.compressed_storage_bytes, &inspector);
      values.CreateUint("compressed_fragmentation_bytes", stats.compressed_fragmentation_bytes,
                        &inspector);
      values.CreateUint("compression_time", stats.compression_time, &inspector);
      values.CreateUint("decompression_time", stats.decompression_time, &inspector);
      values.CreateUint("total_page_compression_attempts", stats.total_page_compression_attempts,
                        &inspector);
      values.CreateUint("failed_page_compression_attempts", stats.failed_page_compression_attempts,
                        &inspector);
      values.CreateUint("total_page_decompressions", stats.total_page_decompressions, &inspector);
      values.CreateUint("compressed_page_evictions", stats.compressed_page_evictions, &inspector);
      values.CreateUint("eager_page_compressions", stats.eager_page_compressions, &inspector);
      values.CreateUint("memory_pressure_page_compressions",
                        stats.memory_pressure_page_compressions, &inspector);
      values.CreateUint("critical_memory_page_compressions",
                        stats.critical_memory_page_compressions, &inspector);
      values.CreateUint("pages_decompressed_unit_ns", stats.pages_decompressed_unit_ns, &inspector);
      constexpr size_t log_time_size =
          sizeof(zx_info_kmem_stats_compression_t::pages_decompressed_within_log_time) /
          sizeof(zx_info_kmem_stats_compression_t::pages_decompressed_within_log_time[0]);
      inspect::UintArray array =
          values.CreateUintArray("pages_decompressed_within_log_time", log_time_size);

      for (size_t i = 0; i < log_time_size; i++) {
        array.Set(i, stats.pages_decompressed_within_log_time[i]);
      }
      inspector.emplace(std::move(array));
      inspector.emplace(std::move(values));
    }

    Digest digest;
    digester_.Digest(capture, &digest);
    std::ostringstream digest_stream;
    TextPrinter digest_printer(digest_stream);
    digest_printer.PrintDigest(digest);
    auto current_digest_string = digest_stream.str();
    if (!current_digest_string.empty()) {
      root.RecordString("current_digest", current_digest_string);
    }
    return fpromise::make_result_promise(fpromise::ok(inspector));
  });

  return inspector;
}

void Monitor::SampleAndPost() {
  if (logging_ || tracing_) {
    Capture capture;
    auto s = capture_maker_.GetCapture(&capture, CaptureLevel::KMEM);
    if (s != ZX_OK) {
      FX_LOGS(ERROR) << "Error getting capture: " << zx_status_get_string(s);
      return;
    }
    const auto& kmem = capture.kmem();
    if (logging_) {
      FX_LOGS(INFO) << "Free: " << kmem.free_bytes << " Free Heap: " << kmem.free_heap_bytes
                    << " VMO: " << kmem.vmo_bytes << " MMU: " << kmem.mmu_overhead_bytes
                    << " IPC: " << kmem.ipc_bytes;
    }
    if (tracing_) {
      TRACE_COUNTER(kTraceName, "allocated", 0, "vmo", kmem.vmo_bytes, "mmu_overhead",
                    kmem.mmu_overhead_bytes, "ipc", kmem.ipc_bytes);
      TRACE_COUNTER(kTraceName, "free", 0, "free", kmem.free_bytes, "free_heap",
                    kmem.free_heap_bytes);
    }
    async::PostDelayedTask(dispatcher_, [this] { SampleAndPost(); }, delay_);
  }
}

void Monitor::MeasureBandwidthAndPost() {
  if (!ram_device_)
    return;
  // Bandwidth measurements are cheap but they take some time to
  // perform as they run over a number of memory cycles. In order to
  // support a relatively small cycle count for measurements, we keep
  // multiple requests in-flight. This gives us results with high
  // granularity and relatively good coverage.
  while (tracing_ && pending_bandwidth_measurements_ < kMaxPendingBandwidthMeasurements) {
    uint64_t cycles_to_measure = kMemCyclesToMeasure;
    bool trace_high_precision = trace_is_category_enabled(kTraceNameHighPrecisionBandwidth);
    bool trace_high_precision_camera =
        trace_is_category_enabled(kTraceNameHighPrecisionBandwidthCamera);
    if (trace_high_precision && trace_high_precision_camera) {
      FX_LOGS(ERROR) << kTraceNameHighPrecisionBandwidth << " and "
                     << kTraceNameHighPrecisionBandwidthCamera
                     << " are mutually exclusive categories.";
    }
    if (trace_high_precision || trace_high_precision_camera) {
      cycles_to_measure = kMemCyclesToMeasureHighPrecision;
    }
    ++pending_bandwidth_measurements_;
    (*ram_device_)
        ->MeasureBandwidth({{BuildConfig(cycles_to_measure, trace_high_precision)}})
        .Then([this, cycles_to_measure, trace_high_precision_camera](
                  fidl::Result<fuchsia_hardware_ram_metrics::Device::MeasureBandwidth>& result) {
          --pending_bandwidth_measurements_;
          if (result.is_error()) {
            FX_LOGS(ERROR) << "Bad bandwidth measurement result: " << result.error_value();
          } else {
            const auto& info = result->info();
            uint64_t total_readwrite_cycles = TotalReadWriteCycles(info);
            uint64_t other_readwrite_cycles =
                (info.total().readwrite_cycles() > total_readwrite_cycles)
                    ? info.total().readwrite_cycles() - total_readwrite_cycles
                    : 0;
            static_assert(std::size(kRamDefaultChannels) == std::size(kRamCameraChannels));
            const auto* channels =
                trace_high_precision_camera ? kRamCameraChannels : kRamDefaultChannels;
            TRACE_VTHREAD_COUNTER(
                kTraceName, "bandwidth_usage", "membw" /*vthread_literal*/, 1 /*vthread_id*/,
                0 /*counter_id*/, TimestampToTicks(info.timestamp()), channels[0].name,
                CounterToBandwidth(info.channels()[0].readwrite_cycles(), info.frequency(),
                                   cycles_to_measure) *
                    info.bytes_per_cycle(),
                channels[1].name,
                CounterToBandwidth(info.channels()[1].readwrite_cycles(), info.frequency(),
                                   cycles_to_measure) *
                    info.bytes_per_cycle(),
                channels[2].name,
                CounterToBandwidth(info.channels()[2].readwrite_cycles(), info.frequency(),
                                   cycles_to_measure) *
                    info.bytes_per_cycle(),
                channels[3].name,
                CounterToBandwidth(info.channels()[3].readwrite_cycles(), info.frequency(),
                                   cycles_to_measure) *
                    info.bytes_per_cycle(),
                "other",
                CounterToBandwidth(other_readwrite_cycles, info.frequency(), cycles_to_measure) *
                    info.bytes_per_cycle());
            TRACE_VTHREAD_COUNTER(kTraceName, "bandwidth_free", "membw" /*vthread_literal*/,
                                  1 /*vthread_id*/, 0 /*counter_id*/,
                                  TimestampToTicks(info.timestamp()), "value",
                                  CounterToBandwidth(cycles_to_measure - total_readwrite_cycles -
                                                         other_readwrite_cycles,
                                                     info.frequency(), cycles_to_measure) *
                                      info.bytes_per_cycle());
          }
          async::PostTask(dispatcher_, [this] { MeasureBandwidthAndPost(); });
        });
  }
}

void Monitor::PeriodicMeasureBandwidth() {
  if (!ram_device_)
    return;
  std::chrono::seconds seconds_to_sleep = std::chrono::seconds(1);
  async::PostDelayedTask(
      dispatcher_, [this]() { PeriodicMeasureBandwidth(); }, zx::sec(seconds_to_sleep.count()));

  // Will not do measurement when tracing
  if (tracing_)
    return;

  uint64_t cycles_to_measure = kMemCyclesToMeasure;
  (*ram_device_)
      ->MeasureBandwidth({{BuildConfig(cycles_to_measure)}})
      .Then([this, cycles_to_measure](
                fidl::Result<fuchsia_hardware_ram_metrics::Device::MeasureBandwidth>& result) {
        if (result.is_error()) {
          FX_LOGS(ERROR) << "Bad bandwidth measurement result: " << result.error_value();
        } else {
          const auto& info = result->info();
          uint64_t total_readwrite_cycles = TotalReadWriteCycles(info);
          total_readwrite_cycles =
              std::max(total_readwrite_cycles, info.total().readwrite_cycles());

          uint64_t memory_bandwidth_reading =
              CounterToBandwidth(total_readwrite_cycles, info.frequency(), cycles_to_measure) *
              info.bytes_per_cycle();
          if (metrics_)
            metrics_->NextMemoryBandwidthReading(memory_bandwidth_reading, info.timestamp());
        }
      });
}

void Monitor::UpdateState() {
  if (trace_state() == TRACE_STARTED) {
    if (trace_is_category_enabled(kTraceName)) {
      FX_LOGS(INFO) << "Tracing started";
      if (!tracing_) {
        Capture capture;
        auto s = capture_maker_.GetCapture(&capture, CaptureLevel::KMEM);
        if (s != ZX_OK) {
          FX_LOGS(ERROR) << "Error getting capture: " << zx_status_get_string(s);
          return;
        }
        const auto& kmem = capture.kmem();
        TRACE_COUNTER(kTraceName, "fixed", 0, "total", kmem.total_bytes, "wired", kmem.wired_bytes,
                      "total_heap", kmem.total_heap_bytes);
        tracing_ = true;
        if (!logging_) {
          SampleAndPost();
        }
        if (ram_device_) {
          MeasureBandwidthAndPost();
        }
      }
    }
  } else {
    if (tracing_) {
      FX_LOGS(INFO) << "Tracing stopped";
      tracing_ = false;
    }
  }
}

void Monitor::GetDigest(const memory::Capture& capture, memory::Digest* digest) {
  std::lock_guard<std::mutex> lock(digester_mutex_);
  digester_.Digest(capture, digest);
}

namespace {
pressure_signaler::Level ConvertPressureLevel(fuchsia_memorypressure::Level level) {
  switch (level) {
    case fuchsia_memorypressure::Level::kCritical:
      return pressure_signaler::kCritical;
    case fuchsia_memorypressure::Level::kWarning:
      return pressure_signaler::kWarning;
    case fuchsia_memorypressure::Level::kNormal:
      return pressure_signaler::kNormal;
  }
}
}  // namespace

void Monitor::OnLevelChanged(pressure_signaler::Level level) {
  if (level == level_) {
    return;
  }
  FX_LOGS(INFO) << "Memory pressure level changed from " << pressure_signaler::kLevelNames[level_]
                << " to " << pressure_signaler::kLevelNames[level];
  TRACE_INSTANT("memory_monitor", "MemoryPressureLevelChange", TRACE_SCOPE_THREAD, "from",
                pressure_signaler::kLevelNames[level_], "to",
                pressure_signaler::kLevelNames[level]);
  level_ = level;
  logger_.SetPressureLevel(level_);
}

void Monitor::OnLevelChanged(OnLevelChangedRequest& request,
                             OnLevelChangedCompleter::Sync& completer) {
  completer.Reply();
  OnLevelChanged(ConvertPressureLevel(request.level()));
}

void Monitor::WaitForImminentOom() {
  zx_signals_t observed;
  zx_status_t status = zx_object_wait_one(imminent_oom_event_handle_, ZX_EVENT_SIGNALED,
                                          ZX_TIME_INFINITE, &observed);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "zx_object_wait_one returned " << zx_status_get_string(status);
    return;
  }

  OnLevelChanged(pressure_signaler::Level::kImminentOOM);
}

void Monitor::WatchForImminentOom() {
  WaitForImminentOom();
  watch_task_.Post(imminent_oom_loop_.dispatcher());
}

}  // namespace monitor
