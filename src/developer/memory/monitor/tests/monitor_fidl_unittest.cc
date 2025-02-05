// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/ram/metrics/cpp/fidl_test_base.h>
#include <fuchsia/metrics/cpp/fidl_test_base.h>
#include <lib/async/cpp/executor.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/sys/cpp/testing/component_context_provider.h>

#include <cstddef>
#include <future>
#include <memory>

#include <gtest/gtest.h>
#include <src/cobalt/bin/testing/stub_metric_event_logger.h>

#include "lib/fpromise/result.h"
#include "src/developer/memory/monitor/monitor.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace monitor::test {

class FakeRamDevice : public fuchsia::hardware::ram::metrics::testing::Device_TestBase {
 public:
  FakeRamDevice() = default;

  fidl::InterfaceRequestHandler<fuchsia::hardware::ram::metrics::Device> GetHandler(
      async_dispatcher_t* dispatcher = nullptr) {
    return [this,
            dispatcher](fidl::InterfaceRequest<fuchsia::hardware::ram::metrics::Device> request) {
      binding_.Bind(std::move(request), dispatcher);
    };
  }

  void MeasureBandwidth(fuchsia::hardware::ram::metrics::BandwidthMeasurementConfig config,
                        MeasureBandwidthCallback completer) override {
    auto mul = config.cycles_to_measure / 1024;

    fuchsia::hardware::ram::metrics::BandwidthInfo info = {};
    info.timestamp = zx::msec(1234).to_nsecs();
    info.frequency = static_cast<uint64_t>(256 * 1024 * 1024);
    info.bytes_per_cycle = 1;
    info.channels[0].readwrite_cycles = 10 * mul;
    info.channels[1].readwrite_cycles = 20 * mul;
    info.channels[2].readwrite_cycles = 30 * mul;
    info.channels[3].readwrite_cycles = 40 * mul;
    info.channels[4].readwrite_cycles = 50 * mul;
    info.channels[5].readwrite_cycles = 60 * mul;
    info.channels[6].readwrite_cycles = 70 * mul;
    info.channels[7].readwrite_cycles = 80 * mul;

    fuchsia::hardware::ram::metrics::Device_MeasureBandwidth_Response response(info);
    auto result = fuchsia::hardware::ram::metrics::Device_MeasureBandwidth_Result::WithResponse(
        std::move(response));
    completer(std::move(result));
  }

  void NotImplemented_(const std::string& name) override { FAIL() << name; }

 private:
  fidl::Binding<fuchsia::hardware::ram::metrics::Device> binding_{this};
};

class MockLogger : public fidl::Server<fuchsia_metrics::MetricEventLogger> {
 public:
  void LogMetricEvents(LogMetricEventsRequest& request,
                       LogMetricEventsCompleter::Sync& completer) override {
    num_calls_++;
    num_events_ += request.events().size();
    completer.Reply(fit::success());
  }
  size_t num_calls() const { return num_calls_; }
  size_t num_events() const { return num_events_; }

  void LogOccurrence(LogOccurrenceRequest& request,
                     LogOccurrenceCompleter::Sync& completer) override {
    FAIL() << "Not implemented";
  }
  void LogString(LogStringRequest& request, LogStringCompleter::Sync& completer) override {
    FAIL() << "Not implemented";
  }
  void LogInteger(LogIntegerRequest& request, LogIntegerCompleter::Sync& completer) override {
    FAIL() << "Not implemented";
  }
  void LogIntegerHistogram(LogIntegerHistogramRequest& request,
                           LogIntegerHistogramCompleter::Sync& completer) override {
    FAIL() << "Not implemented";
  }

 private:
  size_t num_calls_ = 0;
  size_t num_events_ = 0;
};

class MockLoggerFactory : public fidl::Server<fuchsia_metrics::MetricEventLoggerFactory> {
 public:
  explicit MockLoggerFactory(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}
  uint32_t received_project_id() const { return received_project_id_; }

  void CreateMetricEventLogger(CreateMetricEventLoggerRequest& request,
                               CreateMetricEventLoggerCompleter::Sync& completer) override {
    received_project_id_ = request.project_spec().project_id().value();
    auto _ = fidl::BindServer(dispatcher_, std::move(request.logger()), &logger_);
    completer.Reply(fit::success());
  }

  void CreateMetricEventLoggerWithExperiments(
      CreateMetricEventLoggerWithExperimentsRequest& request,
      CreateMetricEventLoggerWithExperimentsCompleter::Sync& completer) override {
    FAIL() << "Not implemented";
  }

 private:
  uint32_t received_project_id_;
  MockLogger logger_{};
  async_dispatcher_t* dispatcher_;
};

class MemoryBandwidthInspectTest : public gtest::TestLoopFixture {
 public:
  MemoryBandwidthInspectTest()
      : monitor_(Monitor(fxl::CommandLine{}, dispatcher(), memory_monitor_config::Config{},
                         memory::CaptureMaker::Create(memory::CreateDefaultOS()).value())),
        executor_(dispatcher()),
        ram_binding_(&fake_device_),
        metric_event_logger_factory_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        logger_factory_(metric_event_logger_factory_loop_.dispatcher()) {
    metric_event_logger_factory_loop_.StartThread();
    // Create metrics
    auto endpoints = fidl::CreateEndpoints<fuchsia_metrics::MetricEventLoggerFactory>().value();
    auto _ = fidl::BindServer<fuchsia_metrics::MetricEventLoggerFactory>(
        metric_event_logger_factory_loop_.dispatcher(), std::move(endpoints.server),
        &logger_factory_);
    CreateMetrics(
        fidl::SyncClient<fuchsia_metrics::MetricEventLoggerFactory>{std::move(endpoints.client)});

    // Set RamDevice
    fuchsia::hardware::ram::metrics::DevicePtr ram_device;
    ram_binding_.Bind(ram_device.NewRequest());
    monitor_.SetRamDevice(std::move(ram_device));
  }

  void RunPromiseToCompletion(fpromise::promise<> promise) {
    bool done = false;
    executor_.schedule_task(std::move(promise).and_then([&]() { done = true; }));
    RunLoopUntilIdle();
    ASSERT_TRUE(done);
  }

  fpromise::result<inspect::Hierarchy> GetHierachyFromInspect() {
    fpromise::result<inspect::Hierarchy> hierarchy;
    RunPromiseToCompletion(inspect::ReadFromInspector(Inspector())
                               .then([&](fpromise::result<inspect::Hierarchy>& result) {
                                 hierarchy = std::move(result);
                               }));
    return hierarchy;
  }

  inspect::Inspector Inspector() { return monitor_.inspector_.inspector(); }

 private:
  void CreateMetrics(fidl::SyncClient<fuchsia_metrics::MetricEventLoggerFactory> factory) {
    // The Monitor will make asynchronous calls to the MockLogger*s that are also running in this
    // class/tests thread. So the call to the Monitor needs to be made on a different thread, such
    // that the MockLogger*s running on the main thread can respond to those calls.
    std::future<void /*fuchsia::metrics::MetricEventLogger_Sync**/> result =
        std::async([this, factory = std::move(factory)]() mutable {
          monitor_.CreateMetrics(std::move(factory), {});
        });
    while (result.wait_for(std::chrono::milliseconds(1)) != std::future_status::ready) {
      // Run the main thread's loop, allowing the MockLogger* objects to respond to requests.
      RunLoopUntilIdle();
    }
    result.get();
  }

  sys::testing::ComponentContextProvider context_provider_;
  Monitor monitor_;
  async::Executor executor_;
  FakeRamDevice fake_device_;
  fidl::Binding<fuchsia::hardware::ram::metrics::Device> ram_binding_;
  async::Loop metric_event_logger_factory_loop_;
  MockLoggerFactory logger_factory_;
  fidl::BindingSet<fuchsia::metrics::MetricEventLoggerFactory> factory_bindings_;
};

TEST_F(MemoryBandwidthInspectTest, MemoryBandwidth) {
  RunLoopUntilIdle();
  fpromise::result<inspect::Hierarchy> result = GetHierachyFromInspect();
  ASSERT_TRUE(result.is_ok());
  auto hierarchy = result.take_value();

  auto* metric_node = hierarchy.GetByPath({Metrics::kInspectPlatformNodeName});
  ASSERT_TRUE(metric_node);

  auto* metric_memory = metric_node->GetByPath({Metrics::kMemoryBandwidthNodeName});
  ASSERT_TRUE(metric_memory);

  auto* readings = metric_memory->node().get_property<inspect::UintArrayValue>(Metrics::kReadings);
  ASSERT_TRUE(readings);
  EXPECT_EQ(Metrics::kMemoryBandwidthArraySize, readings->value().size());
  EXPECT_EQ(94369704u, readings->value()[0]);

  auto* timestamp = metric_memory->node().get_property<inspect::IntPropertyValue>(
      Metrics::kReadingMemoryTimestamp);
  ASSERT_TRUE(timestamp);
}

}  // namespace monitor::test
