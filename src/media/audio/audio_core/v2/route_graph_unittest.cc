// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/route_graph.h"

#include <lib/async-testing/test_loop.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>

#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/audio_core/v2/reference_clock.h"
#include "src/media/audio/audio_core/v2/testing/fake_graph_server.h"
#include "src/media/audio/audio_core/v2/testing/matchers.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/common/logging.h"
#include "src/media/audio/services/common/memory_mapped_buffer.h"

using ::media::audio::CaptureUsage;
using ::media::audio::PipelineConfig;
using ::media::audio::RenderUsage;
using ::media::audio::StreamUsage;
using ::media::audio::StreamUsageSet;
using ::media::audio::StreamUsageSetFromCaptureUsages;
using ::media::audio::StreamUsageSetFromRenderUsages;
using ::media::audio::VolumeCurve;
using InputDeviceProfile = ::media::audio::DeviceConfig::InputDeviceProfile;
using OutputDeviceProfile = ::media::audio::DeviceConfig::OutputDeviceProfile;

using ::fuchsia_audio::GainControlSetGainRequest;
using ::fuchsia_audio::wire::Timestamp;
using ::fuchsia_audio_mixer::GraphBindProducerLeadTimeWatcherRequest;
using ::fuchsia_audio_mixer::GraphCreateConsumerRequest;
using ::fuchsia_audio_mixer::GraphCreateEdgeRequest;
using ::fuchsia_audio_mixer::GraphCreateGainControlRequest;
using ::fuchsia_audio_mixer::GraphCreateMixerRequest;
using ::fuchsia_audio_mixer::GraphCreateProducerRequest;
using ::fuchsia_audio_mixer::GraphCreateSplitterRequest;
using ::fuchsia_audio_mixer::GraphDeleteNodeRequest;
using ::fuchsia_audio_mixer::GraphStartRequest;
using ::fuchsia_audio_mixer::GraphStopRequest;

using ::testing::UnorderedElementsAreArray;

namespace media_audio {
namespace {

constexpr ThreadId kThreadId = 100;
constexpr uint32_t kClockDomain = 42;
const Format kFormatIntMono = Format::CreateOrDie({fuchsia_audio::SampleType::kInt32, 1, 1000});
const Format kFormatIntStereo = Format::CreateOrDie({fuchsia_audio::SampleType::kInt32, 2, 1000});

struct TestHarness {
  TestHarness();
  ~TestHarness();

  async::TestLoop loop;
  std::shared_ptr<FidlThread> thread;
  std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;
  std::shared_ptr<FakeGraphServer> graph_server;
  ReferenceClock reference_clock;

  std::vector<std::shared_ptr<AudioRendererServer>> renderer_servers;
  std::vector<fidl::WireSharedClient<fuchsia_media::AudioRenderer>> renderer_clients;

  std::vector<std::shared_ptr<AudioRendererServer>> capturer_servers;
  std::vector<fidl::WireSharedClient<fuchsia_media::AudioRenderer>> capturer_clients;
};

TestHarness::TestHarness() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_audio_mixer::Graph>();
  if (!endpoints.is_ok()) {
    FX_PLOGS(FATAL, endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  thread = FidlThread::CreateFromCurrentThread("test", loop.dispatcher());
  graph_client = std::make_shared<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>>(
      std::move(endpoints->client), loop.dispatcher());
  graph_server = FakeGraphServer::Create(
      FidlThread::CreateFromCurrentThread("test", loop.dispatcher()), std::move(endpoints->server));

  EXPECT_EQ(zx::clock::create(ZX_CLOCK_OPT_MONOTONIC | ZX_CLOCK_OPT_CONTINUOUS, nullptr,
                              &reference_clock.handle),
            ZX_OK);
  reference_clock.domain = kClockDomain;
}

TestHarness::~TestHarness() {
  graph_client = nullptr;
  renderer_clients.clear();
  capturer_clients.clear();
  loop.RunUntilIdle();
  EXPECT_TRUE(graph_server->WaitForShutdown(zx::nsec(0)));
  for (auto& server : renderer_servers) {
    EXPECT_TRUE(server->WaitForShutdown(zx::nsec(0)));
  }
  for (auto& server : capturer_servers) {
    EXPECT_TRUE(server->WaitForShutdown(zx::nsec(0)));
  }
}

std::shared_ptr<OutputDevicePipeline> CreateOutputPipeline(TestHarness& h, const Format& format,
                                                           std::vector<RenderUsage> usages,
                                                           bool with_loopback) {
  fidl::Arena<> arena;
  std::shared_ptr<OutputDevicePipeline> pipeline;

  PipelineConfig::MixGroup root{
      .name = "root",
      .input_streams = usages,
      .loopback = with_loopback,
      .output_rate = static_cast<int32_t>(format.frames_per_second()),
      .output_channels = static_cast<int16_t>(format.channels()),
  };

  OutputDevicePipeline::Create({
      .graph_client = h.graph_client,
      .dispatcher = h.loop.dispatcher(),
      .consumer =
          {
              .thread = kThreadId,
              .ring_buffer = fuchsia_audio::wire::RingBuffer::Builder(arena)
                                 .format(format.ToWireFidl(arena))
                                 .reference_clock(h.reference_clock.DupHandle())
                                 .reference_clock_domain(h.reference_clock.domain)
                                 .Build(),
              .external_delay_watcher =
                  fuchsia_audio_mixer::wire::ExternalDelayWatcher::Builder(arena)
                      .initial_delay(0)
                      .Build(),
          },
      .config =
          OutputDeviceProfile(with_loopback, StreamUsageSetFromRenderUsages(usages),
                              VolumeCurve::DefaultForMinGain(VolumeCurve::kDefaultGainForMinVolume),
                              /*independent_volume_control=*/true, PipelineConfig(root),
                              /*driver_gain_db=*/0.0f, /*software_gain_db=*/0.0f),
      .callback =
          [&pipeline](auto p) {
            ASSERT_TRUE(p);
            pipeline = std::move(p);
          },
  });

  h.loop.RunUntilIdle();
  FX_CHECK(pipeline);
  return pipeline;
}

std::shared_ptr<InputDevicePipeline> CreateInputPipeline(TestHarness& h, const Format& format,
                                                         std::vector<CaptureUsage> usages) {
  fidl::Arena<> arena;

  std::shared_ptr<InputDevicePipeline> pipeline;
  InputDevicePipeline::CreateForDevice({
      .graph_client = h.graph_client,
      .dispatcher = h.loop.dispatcher(),
      .producer =
          {
              .ring_buffer = fuchsia_audio::wire::RingBuffer::Builder(arena)
                                 .format(format.ToWireFidl(arena))
                                 .reference_clock(h.reference_clock.DupHandle())
                                 .reference_clock_domain(h.reference_clock.domain)
                                 .Build(),
              .external_delay_watcher =
                  fuchsia_audio_mixer::wire::ExternalDelayWatcher::Builder(arena)
                      .initial_delay(0)
                      .Build(),
          },
      .config =
          InputDeviceProfile(static_cast<uint32_t>(format.frames_per_second()),
                             StreamUsageSetFromCaptureUsages(usages),
                             VolumeCurve::DefaultForMinGain(VolumeCurve::kDefaultGainForMinVolume),
                             /*driver_gain_db=*/0.0f, /*software_gain_db=*/0.0f),
      .thread = kThreadId,
      .callback =
          [&pipeline](auto p) {
            ASSERT_TRUE(p);
            pipeline = std::move(p);
          },
  });

  h.loop.RunUntilIdle();
  FX_CHECK(pipeline);
  return pipeline;
}

struct RendererServerAndClient {
  std::shared_ptr<AudioRendererServer> server;
  fidl::WireSharedClient<fuchsia_media::AudioRenderer> client;
};

RendererServerAndClient CreateRenderer(TestHarness& h, const Format& format, RenderUsage usage,
                                       bool ramp_on_play_pause) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_media::AudioRenderer>();
  if (!endpoints.is_ok()) {
    FX_PLOGS(FATAL, endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  auto server =
      AudioRendererServer::Create(h.thread, std::move(endpoints->server),
                                  {
                                      .graph_client = h.graph_client,
                                      .usage = usage,
                                      .format = format,
                                      .default_reference_clock = h.reference_clock.DupHandle(),
                                      .ramp_on_play_pause = ramp_on_play_pause,
                                  });

  auto client = fidl::WireSharedClient<fuchsia_media::AudioRenderer>(std::move(endpoints->client),
                                                                     h.loop.dispatcher());

  zx::vmo vmo;
  EXPECT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  EXPECT_EQ(client->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_TRUE(server->IsFullyCreated());

  return {std::move(server), std::move(client)};
}

struct CapturerServerAndClient {
  std::shared_ptr<AudioCapturerServer> server;
  fidl::WireSharedClient<fuchsia_media::AudioCapturer> client;
};

CapturerServerAndClient CreateCapturer(TestHarness& h, const Format& format, CaptureUsage usage) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_media::AudioCapturer>();
  if (!endpoints.is_ok()) {
    FX_PLOGS(FATAL, endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  auto server =
      AudioCapturerServer::Create(h.thread, std::move(endpoints->server),
                                  {
                                      .graph_client = h.graph_client,
                                      .usage = usage,
                                      .format = format,
                                      .default_reference_clock = h.reference_clock.DupHandle(),
                                  });
  auto client = fidl::WireSharedClient<fuchsia_media::AudioCapturer>(std::move(endpoints->client),
                                                                     h.loop.dispatcher());

  zx::vmo vmo;
  EXPECT_EQ(zx::vmo::create(1024, 0, &vmo), ZX_OK);
  EXPECT_EQ(client->AddPayloadBuffer(0, std::move(vmo)).status(), ZX_OK);

  h.loop.RunUntilIdle();
  EXPECT_TRUE(server->IsFullyCreated());

  return {std::move(server), std::move(client)};
}

TEST(RouteGraphTest, TestOutputDevices) {
  constexpr auto kUsage1 = RenderUsage::MEDIA;
  constexpr auto kUsage2 = RenderUsage::COMMUNICATION;
  const auto& kFormat = kFormatIntMono;

  TestHarness h;
  auto device1 = CreateOutputPipeline(h, kFormat, {kUsage1, kUsage2}, /*with_loopback=*/true);
  auto device2 = CreateOutputPipeline(h, kFormat, {kUsage2}, /*with_loopback=*/false);
  auto renderer1 = CreateRenderer(h, kFormat, kUsage1, /*ramp_on_play_pause=*/true);
  auto renderer2 = CreateRenderer(h, kFormat, kUsage2, /*ramp_on_play_pause=*/false);
  auto renderer3 = CreateRenderer(h, kFormat, kUsage2, /*ramp_on_play_pause=*/false);
  auto capturer1 = CreateCapturer(h, kFormat, CaptureUsage::LOOPBACK);
  auto capturer2 = CreateCapturer(h, kFormat, CaptureUsage::FOREGROUND);

  const auto producer1 = renderer1.server->producer_node();
  const auto producer2 = renderer2.server->producer_node();
  const auto producer3 = renderer3.server->producer_node();
  const auto consumer1 = capturer1.server->consumer_node();

  const auto& calls = h.graph_server->calls();
  const auto call0 = calls.size();

  auto r = std::make_shared<RouteGraph>(h.graph_client);

  // Add device1 and the renderers, each of which should route to device1.
  r->AddOutputDevice(device1, zx::time(1));
  r->AddRenderer(renderer1.server);
  r->AddRenderer(renderer2.server);
  r->AddRenderer(renderer3.server);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 3) << "call0=" << call0;
  EXPECT_THAT(calls[call0 + 0], CreateEdgeEq(producer1, device1->DestNodeForUsage(kUsage1)));
  EXPECT_THAT(calls[call0 + 1], CreateEdgeEq(producer2, device1->DestNodeForUsage(kUsage2)));
  EXPECT_THAT(calls[call0 + 2], CreateEdgeEq(producer3, device1->DestNodeForUsage(kUsage2)));

  // Check that the correct gain controls were attached to those edges.
  EXPECT_THAT(calls[call0 + 0], CreateEdgeWithGainControlsEq(std::vector<GainControlId>{
                                    device1->UsageVolumeForUsage(kUsage1)->gain_controls()[0],
                                    device1->UsageVolumeForUsage(kUsage1)->gain_controls()[1],
                                    renderer1.server->stream_gain_control(),
                                    *renderer1.server->play_pause_ramp_gain_control()}));

  EXPECT_THAT(calls[call0 + 1], CreateEdgeWithGainControlsEq(std::vector<GainControlId>{
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[0],
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[1],
                                    renderer2.server->stream_gain_control()}));

  EXPECT_THAT(calls[call0 + 2], CreateEdgeWithGainControlsEq(std::vector<GainControlId>{
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[0],
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[1],
                                    renderer3.server->stream_gain_control()}));

  // Add capturers. capturer1 routes to device1's loopback. capture2 does not have a route.
  r->AddCapturer(capturer1.server);
  r->AddCapturer(capturer2.server);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 4) << "call0=" << call0;
  ASSERT_TRUE(device1->loopback()->SourceNodeForFormat(kFormat));
  EXPECT_THAT(calls[call0 + 3],
              CreateEdgeEq(*device1->loopback()->SourceNodeForFormat(kFormat), consumer1));

  EXPECT_THAT(
      calls[call0 + 3],
      CreateEdgeWithGainControlsEq(std::vector<GainControlId>{
          device1->loopback()->UsageVolumeForUsage(CaptureUsage::LOOPBACK)->gain_controls()[0],
          device1->loopback()->UsageVolumeForUsage(CaptureUsage::LOOPBACK)->gain_controls()[1],
          capturer1.server->stream_gain_control()}));

  // After adding device1, renderer2 and renderer3 are rerouted to device2.
  // Whether renderer2 or renderer3 is rerouted first depends on how the shared_ptrs are sorted.
  r->AddOutputDevice(device2, zx::time(2));
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 8) << "call0=" << call0;
  if (renderer2.server < renderer3.server) {
    EXPECT_THAT(calls[call0 + 4], DeleteEdgeEq(producer2, device1->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 5], CreateEdgeEq(producer2, device2->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 6], DeleteEdgeEq(producer3, device1->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 7], CreateEdgeEq(producer3, device2->DestNodeForUsage(kUsage2)));
  } else {
    EXPECT_THAT(calls[call0 + 4], DeleteEdgeEq(producer3, device1->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 5], CreateEdgeEq(producer3, device2->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 6], DeleteEdgeEq(producer2, device1->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 7], CreateEdgeEq(producer2, device2->DestNodeForUsage(kUsage2)));
  }

  // After removing device2, renderer2 and renderer3 are rerouted to device1.
  r->RemoveOutputDevice(device2);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 12) << "call0=" << call0;
  if (renderer2.server < renderer3.server) {
    EXPECT_THAT(calls[call0 + 8], DeleteEdgeEq(producer2, device2->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 9], CreateEdgeEq(producer2, device1->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 10], DeleteEdgeEq(producer3, device2->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 11], CreateEdgeEq(producer3, device1->DestNodeForUsage(kUsage2)));
  } else {
    EXPECT_THAT(calls[call0 + 8], DeleteEdgeEq(producer3, device2->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 9], CreateEdgeEq(producer3, device1->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 10], DeleteEdgeEq(producer2, device2->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 11], CreateEdgeEq(producer2, device1->DestNodeForUsage(kUsage2)));
  }

  // Remove renderer3, which should be disconnected from the graph.
  r->RemoveRenderer(renderer3.server);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 13) << "call0=" << call0;
  EXPECT_THAT(calls[call0 + 12], DeleteEdgeEq(producer3, device1->DestNodeForUsage(kUsage2)));

  // Remove device1, which should disconnect renderer1, renderer2, and capturer1.
  r->RemoveOutputDevice(device1);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 16) << "call0=" << call0;
  if (renderer1.server < renderer2.server) {
    EXPECT_THAT(calls[call0 + 13], DeleteEdgeEq(producer1, device1->DestNodeForUsage(kUsage1)));
    EXPECT_THAT(calls[call0 + 14], DeleteEdgeEq(producer2, device1->DestNodeForUsage(kUsage2)));
  } else {
    EXPECT_THAT(calls[call0 + 13], DeleteEdgeEq(producer2, device1->DestNodeForUsage(kUsage2)));
    EXPECT_THAT(calls[call0 + 14], DeleteEdgeEq(producer1, device1->DestNodeForUsage(kUsage1)));
  }
  EXPECT_THAT(calls[call0 + 15],
              DeleteEdgeEq(*device1->loopback()->SourceNodeForFormat(kFormat), consumer1));
}

TEST(RouteGraphTest, TestInputDevices) {
  constexpr auto kUsage1 = CaptureUsage::FOREGROUND;
  constexpr auto kUsage2 = CaptureUsage::BACKGROUND;
  const auto& kFormat1 = kFormatIntMono;
  const auto& kFormat2 = kFormatIntStereo;

  TestHarness h;
  auto device1 = CreateInputPipeline(h, kFormat1, {kUsage1, kUsage2});
  auto device2 = CreateInputPipeline(h, kFormat1, {kUsage2});
  auto capturer1 = CreateCapturer(h, kFormat1, kUsage1);
  auto capturer2 = CreateCapturer(h, kFormat1, kUsage2);
  auto capturer3 = CreateCapturer(h, kFormat2, kUsage2);

  const auto consumer1 = capturer1.server->consumer_node();
  const auto consumer2 = capturer2.server->consumer_node();
  const auto consumer3 = capturer3.server->consumer_node();

  const auto& calls = h.graph_server->calls();
  const auto call0 = calls.size();

  auto r = std::make_shared<RouteGraph>(h.graph_client);

  // Add device1 and the capturers, each of which should route to device1.
  r->AddInputDevice(device1, zx::time(1));
  r->AddCapturer(capturer1.server);
  r->AddCapturer(capturer2.server);
  r->AddCapturer(capturer3.server);
  h.loop.RunUntilIdle();

  // These two formats should have different source nodes.
  ASSERT_TRUE(device1->SourceNodeForFormat(kFormat1));
  ASSERT_TRUE(device1->SourceNodeForFormat(kFormat2));
  EXPECT_NE(device1->SourceNodeForFormat(kFormat1), device1->SourceNodeForFormat(kFormat2));

  const auto device1_format1_source = *device1->SourceNodeForFormat(kFormat1);
  const auto device1_format2_source = *device1->SourceNodeForFormat(kFormat2);

  // After capturer1 and capturer2 are routed, there are four extra calls to create device1's source
  // node for kFormat2 (CreateMixer, CreateSplitter, CreateEdge to connect root source -> mixer, and
  // CreateEdge to connect mixer -> splitter).
  ASSERT_EQ(calls.size(), call0 + 7) << "call0=" << call0;
  EXPECT_THAT(calls[call0 + 0], CreateEdgeEq(device1_format1_source, consumer1));
  EXPECT_THAT(calls[call0 + 1], CreateEdgeEq(device1_format1_source, consumer2));
  EXPECT_THAT(calls[call0 + 6], CreateEdgeEq(device1_format2_source, consumer3));

  // Check that the correct gain controls were attached to those edges.
  EXPECT_THAT(calls[call0 + 0], CreateEdgeWithGainControlsEq(std::vector<GainControlId>{
                                    device1->UsageVolumeForUsage(kUsage1)->gain_controls()[0],
                                    device1->UsageVolumeForUsage(kUsage1)->gain_controls()[1],
                                    capturer1.server->stream_gain_control()}));

  EXPECT_THAT(calls[call0 + 1], CreateEdgeWithGainControlsEq(std::vector<GainControlId>{
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[0],
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[1],
                                    capturer2.server->stream_gain_control()}));

  EXPECT_THAT(calls[call0 + 6], CreateEdgeWithGainControlsEq(std::vector<GainControlId>{
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[0],
                                    device1->UsageVolumeForUsage(kUsage2)->gain_controls()[1],
                                    capturer3.server->stream_gain_control()}));

  // After adding device2, capturer2 and capturer3 are rerouted to device2.
  // Whether capturer2 or capturer3 is rerouted first depends on how the shared_ptrs are sorted.
  r->AddInputDevice(device2, zx::time(2));
  h.loop.RunUntilIdle();

  ASSERT_TRUE(device2->SourceNodeForFormat(kFormat1));
  ASSERT_TRUE(device2->SourceNodeForFormat(kFormat2));
  const auto device2_format1_source = *device2->SourceNodeForFormat(kFormat1);
  const auto device2_format2_source = *device2->SourceNodeForFormat(kFormat2);

  // As above, there are four extra calls to create device2's source node for kFormat2.
  ASSERT_EQ(calls.size(), call0 + 15) << "call0=" << call0;
  if (capturer2.server < capturer3.server) {
    // capturer2 is rerouted, then the source is created for kFormat2, then capturer3 is rerouted.
    EXPECT_THAT(calls[call0 + 7], DeleteEdgeEq(device1_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 8], CreateEdgeEq(device2_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 13], DeleteEdgeEq(device1_format2_source, consumer3));
    EXPECT_THAT(calls[call0 + 14], CreateEdgeEq(device2_format2_source, consumer3));
  } else {
    // start creating the source for kFormat2 (CreateMixer and CreateSplitter), then reroute
    // capturer2, then finish creating the source (two CreateEdge calls), then reroute capturer3.
    EXPECT_THAT(calls[call0 + 9], DeleteEdgeEq(device1_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 10], CreateEdgeEq(device2_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 13], DeleteEdgeEq(device1_format2_source, consumer3));
    EXPECT_THAT(calls[call0 + 14], CreateEdgeEq(device2_format2_source, consumer3));
  }

  // After removing device2, capturer2 and capturer3 are rerouted to device1.
  r->RemoveInputDevice(device2);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 19) << "call0=" << call0;
  if (capturer2.server < capturer3.server) {
    EXPECT_THAT(calls[call0 + 15], DeleteEdgeEq(device2_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 16], CreateEdgeEq(device1_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 17], DeleteEdgeEq(device2_format2_source, consumer3));
    EXPECT_THAT(calls[call0 + 18], CreateEdgeEq(device1_format2_source, consumer3));
  } else {
    EXPECT_THAT(calls[call0 + 15], DeleteEdgeEq(device2_format2_source, consumer3));
    EXPECT_THAT(calls[call0 + 16], CreateEdgeEq(device1_format2_source, consumer3));
    EXPECT_THAT(calls[call0 + 17], DeleteEdgeEq(device2_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 18], CreateEdgeEq(device1_format1_source, consumer2));
  }

  // Remove capturer3, which should be disconnected from the graph.
  r->RemoveCapturer(capturer3.server);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 20) << "call0=" << call0;
  EXPECT_THAT(calls[call0 + 19], DeleteEdgeEq(device1_format2_source, consumer3));

  // Remove device1, which should disconnect capturer1 and capturer2.
  r->RemoveInputDevice(device1);
  h.loop.RunUntilIdle();

  ASSERT_EQ(calls.size(), call0 + 22) << "call0=" << call0;
  if (capturer1.server < capturer2.server) {
    EXPECT_THAT(calls[call0 + 20], DeleteEdgeEq(device1_format1_source, consumer1));
    EXPECT_THAT(calls[call0 + 21], DeleteEdgeEq(device1_format1_source, consumer2));
  } else {
    EXPECT_THAT(calls[call0 + 20], DeleteEdgeEq(device1_format1_source, consumer2));
    EXPECT_THAT(calls[call0 + 21], DeleteEdgeEq(device1_format1_source, consumer1));
  }
}

}  // namespace
}  // namespace media_audio
