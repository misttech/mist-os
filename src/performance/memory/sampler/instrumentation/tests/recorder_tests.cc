// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.memory.sampler/cpp/fidl.h>
#include <fidl/fuchsia.memory.sampler/cpp/natural_types.h>
#include <fidl/fuchsia.memory.sampler/cpp/wire_test_base.h>
#include <fidl/fuchsia.memory.sampler/cpp/wire_types.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/testing/component_context_provider.h>

#include <cstdint>
#include <limits>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/test_loop_fixture.h>
#include <src/performance/memory/sampler/instrumentation/recorder.h>

namespace memory_sampler {
namespace {
void* const kTestAddress = reinterpret_cast<void*>(0x1000);
constexpr size_t kTestSize = 100;

class SamplerImpl : public fidl::testing::WireTestBase<fuchsia_memory_sampler::Sampler> {
 public:
  SamplerImpl(async_dispatcher_t* dispatcher,
              fidl::ServerEnd<fuchsia_memory_sampler::Sampler> server_end)
      : binding_(fidl::BindServer(dispatcher, std::move(server_end), this)) {}

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    std::cerr << "Not implemented: " << name << '\n';
  }

 private:
  fidl::ServerBindingRef<fuchsia_memory_sampler::Sampler> binding_;
};

PoissonSampler& GetSamplerThatAlwaysSamples() {
  class SampleIntervalGenerator : public PoissonSampler::SampleIntervalGenerator {
   public:
    size_t GetNextSampleInterval(size_t) override { return 1; }
  };

  static PoissonSampler sampler{1, std::make_unique<SampleIntervalGenerator>()};
  return sampler;
}

PoissonSampler& GetSamplerThatNeverSamples() {
  class SamplerIntervalGenerator : public PoissonSampler::SampleIntervalGenerator {
   public:
    size_t GetNextSampleInterval(size_t) override { return std::numeric_limits<size_t>::max(); }
  };
  static PoissonSampler sampler{1, std::make_unique<SamplerIntervalGenerator>()};
  return sampler;
}

TEST(RecorderTest, MaybeRecordAllocation) {
  static constexpr size_t kMeaningfulStackTraceLength = 1U;

  // Sampler server that verifies the expected allocation was recorded.
  class Sampler : public SamplerImpl {
   public:
    using SamplerImpl::SamplerImpl;
    void RecordAllocation(fuchsia_memory_sampler::wire::SamplerRecordAllocationRequest* request,
                          RecordAllocationCompleter::Sync& completer) override {
      called_ = true;
      EXPECT_EQ(reinterpret_cast<uint64_t>(kTestAddress), request->address);
      EXPECT_EQ(kTestSize, request->size);
      EXPECT_LE(kMeaningfulStackTraceLength, request->stack_trace.stack_frames().count());
    }
    ~Sampler() override { EXPECT_TRUE(called_); }

   private:
    bool called_ = false;
  };

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)}, GetSamplerThatAlwaysSamples);
  recorder.MaybeRecordAllocation(kTestAddress, kTestSize);

  loop.RunUntilIdle();
}

TEST(RecorderTest, ForgetAllocation) {
  static constexpr size_t kMeaningfulStackTraceLength = 1U;

  // Sampler server that verifies that the expected deallocation was recorded.
  class Sampler : public SamplerImpl {
   public:
    using SamplerImpl::SamplerImpl;
    void RecordDeallocation(fuchsia_memory_sampler::wire::SamplerRecordDeallocationRequest* request,
                            RecordAllocationCompleter::Sync& completer) override {
      called_ = true;
      EXPECT_EQ(reinterpret_cast<uint64_t>(kTestAddress), request->address);
      EXPECT_LE(kMeaningfulStackTraceLength, request->stack_trace.stack_frames().count());
    }
    ~Sampler() override { EXPECT_TRUE(called_); }

   private:
    bool called_ = false;
  };

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)}, GetSamplerThatAlwaysSamples);
  recorder.MaybeRecordAllocation(kTestAddress, kTestSize);
  recorder.MaybeForgetAllocation(kTestAddress);

  loop.RunUntilIdle();
}

TEST(RecorderTest, SetModulesInfo) {
  static constexpr size_t kMeaningfulModuleMapLength = 1U;
  static constexpr size_t kMeaningfulExecutableSegmentsLength = 1U;
  static char process_name[ZX_MAX_NAME_LEN];
  {
    const zx_handle_t process = zx_process_self();
    zx_object_get_property(process, ZX_PROP_NAME, process_name, ZX_MAX_NAME_LEN);
  }

  // Sampler server that verifies the expected process info was
  // communicated.
  class Sampler : public SamplerImpl {
   public:
    using SamplerImpl::SamplerImpl;
    void SetProcessInfo(fuchsia_memory_sampler::wire::SamplerSetProcessInfoRequest* request,
                        RecordAllocationCompleter::Sync& completer) override {
      called_ = true;
      EXPECT_EQ(std::string_view{process_name}, request->process_name().get());

      EXPECT_LE(kMeaningfulModuleMapLength, request->module_map().count());

      auto& module_map = request->module_map()[0];
      EXPECT_GE(static_cast<size_t>(fuchsia_memory_sampler::kBuildIdBytes),
                module_map.build_id().count());
      EXPECT_LE(kMeaningfulExecutableSegmentsLength, module_map.executable_segments().count());

      auto& segment = module_map.executable_segments()[0];
      EXPECT_NE(0U, segment.start_address());
      EXPECT_NE(0U, segment.relative_address());
      EXPECT_NE(0U, segment.size());
    }
    ~Sampler() override { EXPECT_TRUE(called_); }

   private:
    bool called_ = false;
  };

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)}, GetSamplerThatAlwaysSamples);
  recorder.SetModulesInfo();

  loop.RunUntilIdle();
}

TEST(RecorderTest, SampledAllocationCausesSampledDeallocation) {
  // Sampler server that verifies that both the expected allocation
  // and corresponding deallocation occurred.
  class Sampler : public SamplerImpl {
   public:
    using SamplerImpl::SamplerImpl;
    void RecordAllocation(fuchsia_memory_sampler::wire::SamplerRecordAllocationRequest* request,
                          RecordAllocationCompleter::Sync& completer) override {
      if (request->address == reinterpret_cast<uint64_t>(kTestAddress))
        allocation_registered_ = true;
    }
    void RecordDeallocation(fuchsia_memory_sampler::wire::SamplerRecordDeallocationRequest* request,
                            RecordDeallocationCompleter::Sync& completer) override {
      if (request->address == reinterpret_cast<uint64_t>(kTestAddress))
        deallocation_registered_ = true;
    }

    ~Sampler() override {
      EXPECT_TRUE(allocation_registered_);
      EXPECT_TRUE(deallocation_registered_);
    }

   private:
    bool allocation_registered_ = false;
    bool deallocation_registered_ = false;
  };

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)}, GetSamplerThatAlwaysSamples);

  recorder.MaybeRecordAllocation(kTestAddress, kTestSize);
  recorder.MaybeForgetAllocation(kTestAddress);
  loop.RunUntilIdle();
}

TEST(RecorderTest, MaybeForgetAllocationIsNoOpIfAllocationWasNotSampled) {
  // Sampler server that verifies no deallocation was ever recorded.
  class Sampler : public SamplerImpl {
   public:
    using SamplerImpl::SamplerImpl;
    void RecordDeallocation(fuchsia_memory_sampler::wire::SamplerRecordDeallocationRequest* request,
                            RecordDeallocationCompleter::Sync& completer) override {
      FAIL();
    }
  };
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)}, GetSamplerThatNeverSamples);

  // Initial forget while having never recorded.
  recorder.MaybeForgetAllocation(kTestAddress);
  // Does not record.
  recorder.MaybeRecordAllocation(kTestAddress, kTestSize);
  // Forget after maybe recording, but actually not recording.
  recorder.MaybeForgetAllocation(kTestAddress);
  loop.RunUntilIdle();
}
}  // namespace
}  // namespace memory_sampler
