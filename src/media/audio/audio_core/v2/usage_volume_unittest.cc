// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/usage_volume.h"

#include <lib/async-testing/test_loop.h>
#include <lib/syslog/cpp/macros.h>

#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/audio_core/v2/testing/fake_gain_control_server.h"
#include "src/media/audio/audio_core/v2/testing/fake_graph_server.h"
#include "src/media/audio/audio_core/v2/testing/matchers.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/common/logging.h"

using ::media::audio::RenderUsage;
using ::media::audio::StreamUsage;
using ::media::audio::VolumeCurve;

using VolumeMapping = VolumeCurve::VolumeMapping;

using ::fuchsia_audio::GainControlSetGainRequest;
using ::fuchsia_audio_mixer::GraphCreateGainControlRequest;
using ::fuchsia_audio_mixer::GraphDeleteGainControlRequest;

namespace media_audio {
namespace {

struct TestHarness {
  TestHarness();
  ~TestHarness();

  async::TestLoop loop;
  fidl::Arena<> arena;

  std::shared_ptr<FakeGraphServer> graph_server;
  std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;
};

TestHarness::TestHarness() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_audio_mixer::Graph>();
  if (!endpoints.is_ok()) {
    FX_PLOGS(FATAL, endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  auto thread = FidlThread::CreateFromCurrentThread("test", loop.dispatcher());
  graph_server = FakeGraphServer::Create(thread, std::move(endpoints->server));
  graph_client = std::make_shared<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>>(
      std::move(endpoints->client), loop.dispatcher());
}

TestHarness::~TestHarness() {
  graph_client = nullptr;
  loop.RunUntilIdle();
  EXPECT_TRUE(graph_server->WaitForShutdown(zx::nsec(0)));
}

TEST(UsageVolumeTest, RealizeVolume) {
  TestHarness h;

  std::shared_ptr<UsageVolume> usage_volume;
  UsageVolume::Create({
      .graph_client = h.graph_client,
      .dispatcher = h.loop.dispatcher(),
      .volume_curve = std::shared_ptr<VolumeCurve>(
          new VolumeCurve(VolumeCurve::FromMappings({
                                                        VolumeMapping(0.0, -160.0f),
                                                        VolumeMapping(1.0, 0.0f),
                                                    })
                              .value())),
      .usage = StreamUsage::WithRenderUsage(RenderUsage::MEDIA),
      .callback = [&usage_volume](auto uv) { usage_volume = uv; },
  });

  h.loop.RunUntilIdle();
  ASSERT_TRUE(usage_volume);
  ASSERT_TRUE(usage_volume->GetStreamUsage().is_render_usage());
  EXPECT_EQ(usage_volume->GetStreamUsage().render_usage(),
            fuchsia::media::AudioRenderUsage2::MEDIA);

  // Expect two CreateGainControl calls.
  auto& graph_calls = h.graph_server->calls();
  ASSERT_EQ(graph_calls.size(), 2u);
  std::shared_ptr<FakeGainControlServer> gain_control_servers[2];

  for (int k = 0; k < 2; k++) {
    SCOPED_TRACE("graph_calls[" + std::to_string(k) + "]");
    auto call = std::get_if<GraphCreateGainControlRequest>(&graph_calls[k]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->control());
    ASSERT_TRUE(call->control()->is_valid());
    gain_control_servers[k] =
        FakeGainControlServer::Create(h.graph_server->thread_ptr(), std::move(*call->control()));
  }

  h.loop.RunUntilIdle();
  auto& gain0_calls = gain_control_servers[0]->calls();
  auto& gain1_calls = gain_control_servers[1]->calls();
  ASSERT_EQ(gain0_calls.size(), 0u);
  ASSERT_EQ(gain1_calls.size(), 0u);

  // RealizeVolume w/out ramp.
  usage_volume->RealizeVolume({.volume = 0.5f, .gain_db_adjustment = -33.0f});
  h.loop.RunUntilIdle();
  ASSERT_EQ(gain0_calls.size(), 1u);
  ASSERT_EQ(gain1_calls.size(), 1u);

  {
    auto call = std::get_if<GainControlSetGainRequest>(&gain0_calls[0]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how());
    ASSERT_TRUE(call->how()->gain_db());
    EXPECT_EQ(call->how()->gain_db().value(), -80.0f);  // 50%
    ASSERT_TRUE(call->when());
    EXPECT_TRUE(call->when()->asap());
  }
  {
    auto call = std::get_if<GainControlSetGainRequest>(&gain1_calls[0]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how());
    ASSERT_TRUE(call->how()->gain_db());
    EXPECT_EQ(call->how()->gain_db().value(), -33.0f);
    ASSERT_TRUE(call->when());
    EXPECT_TRUE(call->when()->asap());
  }

  // RealizeVolume w/ramp.
  usage_volume->RealizeVolume({.volume = 0.25f,
                               .gain_db_adjustment = -99.0f,
                               .ramp = media::audio::Ramp{
                                   .duration = zx::nsec(20),
                                   .ramp_type = fuchsia::media::audio::RampType::SCALE_LINEAR,
                               }});
  h.loop.RunUntilIdle();
  ASSERT_EQ(gain0_calls.size(), 2u);
  ASSERT_EQ(gain1_calls.size(), 2u);

  {
    auto call = std::get_if<GainControlSetGainRequest>(&gain0_calls[1]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how()->ramped());
    EXPECT_EQ(call->how()->ramped().value().target_gain_db(), -120.0f);  // 25%
    EXPECT_EQ(call->how()->ramped().value().duration(), 20);             // 20ns
    ASSERT_TRUE(call->how()->ramped().value().function());
    ASSERT_TRUE(call->how()->ramped().value().function()->linear_slope());
    ASSERT_TRUE(call->when());
    EXPECT_TRUE(call->when()->asap());
  }
  {
    auto call = std::get_if<GainControlSetGainRequest>(&gain1_calls[1]);
    ASSERT_TRUE(call);
    ASSERT_TRUE(call->how()->ramped());
    EXPECT_EQ(call->how()->ramped().value().target_gain_db(), -99.0f);
    EXPECT_EQ(call->how()->ramped().value().duration(), 20);  // 20ns
    ASSERT_TRUE(call->how()->ramped().value().function());
    ASSERT_TRUE(call->how()->ramped().value().function()->linear_slope());
    ASSERT_TRUE(call->when());
    EXPECT_TRUE(call->when()->asap());
  }
}

}  // namespace
}  // namespace media_audio
