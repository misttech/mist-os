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

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/test_loop_fixture.h>
#include <src/performance/memory/sampler/instrumentation/recorder.h>

class SamplerImpl : public fidl::testing::WireTestBase<fuchsia_memory_sampler::Sampler> {
 public:
  SamplerImpl(async_dispatcher_t* dispatcher,
              fidl::ServerEnd<fuchsia_memory_sampler::Sampler> server_end)
      : binding_(fidl::BindServer(dispatcher, std::move(server_end), this)) {}

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    std::cerr << "Not implemented: " << name << std::endl;
  }

 private:
  fidl::ServerBindingRef<fuchsia_memory_sampler::Sampler> binding_;
};

TEST(RecorderTest, RecordAllocation) {
  static constexpr uint64_t kTestAddress = 0x1000;
  static constexpr uint64_t kTestSize = 100;
  static constexpr size_t kMeaningfulStackTraceLength = 1U;

  class Sampler : public SamplerImpl {
    using SamplerImpl::SamplerImpl;
    void RecordAllocation(fuchsia_memory_sampler::wire::SamplerRecordAllocationRequest* request,
                          RecordAllocationCompleter::Sync& completer) override {
      EXPECT_EQ(kTestAddress, request->address);
      EXPECT_EQ(kTestSize, request->size);
      EXPECT_LE(kMeaningfulStackTraceLength, request->stack_trace.stack_frames().count());
    }
  };

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)});
  recorder.RecordAllocation(0x1000, 100);

  loop.RunUntilIdle();
}

TEST(RecorderTest, ForgetAllocation) {
  static constexpr uint64_t kTestAddress = 0x1000;
  static constexpr size_t kMeaningfulStackTraceLength = 1U;

  class Sampler : public SamplerImpl {
    using SamplerImpl::SamplerImpl;
    void RecordDeallocation(fuchsia_memory_sampler::wire::SamplerRecordDeallocationRequest* request,
                            RecordAllocationCompleter::Sync& completer) override {
      EXPECT_EQ(kTestAddress, request->address);
      EXPECT_LE(kMeaningfulStackTraceLength, request->stack_trace.stack_frames().count());
    }
  };

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)});
  recorder.ForgetAllocation(0x1000);

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

  class Sampler : public SamplerImpl {
    using SamplerImpl::SamplerImpl;
    void SetProcessInfo(fuchsia_memory_sampler::wire::SamplerSetProcessInfoRequest* request,
                        RecordAllocationCompleter::Sync& completer) override {
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
  };

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  auto endpoints = fidl::CreateEndpoints<fuchsia_memory_sampler::Sampler>();
  Sampler sampler{dispatcher, std::move(endpoints->server)};

  auto recorder = memory_sampler::Recorder::CreateRecorderForTesting(
      fidl::SyncClient{std::move(endpoints->client)});
  recorder.SetModulesInfo();

  loop.RunUntilIdle();
}
