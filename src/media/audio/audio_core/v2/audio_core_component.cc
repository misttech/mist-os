// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/audio_core_component.h"

#include <fidl/fuchsia.audio.mixer/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.media/cpp/hlcpp_conversion.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/fidl/cpp/wire/client.h>

#include "src/media/audio/audio_core/shared/policy_loader.h"
#include "src/media/audio/audio_core/shared/process_config_loader.h"
#include "src/media/audio/audio_core/shared/profile_acquirer.h"
#include "src/media/audio/audio_core/shared/reporter.h"
#include "src/media/audio/audio_core/shared/volume_curve.h"
#include "src/media/audio/audio_core/v2/audio_core_server.h"
#include "src/media/audio/audio_core/v2/audio_server.h"
#include "src/media/audio/audio_core/v2/ultrasound_factory_server.h"

namespace media_audio {

namespace {

using ::media::audio::AcquireMemoryRole;
using ::media::audio::ActivityDispatcherImpl;
using ::media::audio::PolicyLoader;
using ::media::audio::ProcessConfig;
using ::media::audio::ProcessConfigLoader;
using ::media::audio::ProfileProvider;
using ::media::audio::Reporter;
using ::media::audio::StreamVolumeManager;
using ::media::audio::UsageGainReporterImpl;
using ::media::audio::UsageReporterImpl;
using ::media::audio::VolumeCurve;

constexpr char kProcessConfigPath[] = "/config/data/audio_core_config.json";

ProcessConfig LoadProcessConfig() {
  auto result = ProcessConfigLoader::LoadProcessConfig(kProcessConfigPath);
  if (result.is_ok()) {
    return std::move(result.value());
  }

  FX_LOGS(WARNING) << "Failed to load " << kProcessConfigPath << ": " << result.error()
                   << ". Falling back to default configuration.";
  return ProcessConfig::Builder()
      .SetDefaultVolumeCurve(VolumeCurve::DefaultForMinGain(VolumeCurve::kDefaultGainForMinVolume))
      .Build();
}

std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> MakeGraphClient(
    async_dispatcher_t* fidl_dispatcher) {
  // Connect to a GraphCreator.
  auto creator_client_end = component::Connect<fuchsia_audio_mixer::GraphCreator>();
  if (!creator_client_end.is_ok()) {
    FX_PLOGS(FATAL, creator_client_end.error_value())
        << "Failed connecting to fuchsia.audio.mixer.GraphCreator";
  }
  auto creator_client = fidl::WireSyncClient(std::move(*creator_client_end));

  // Create the Graph.
  auto graph_endpoints = fidl::CreateEndpoints<fuchsia_audio_mixer::Graph>();
  if (!graph_endpoints.is_ok()) {
    FX_PLOGS(FATAL, graph_endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }
  fidl::Arena<> arena;
  auto result =
      creator_client->Create(fuchsia_audio_mixer::wire::GraphCreatorCreateRequest::Builder(arena)
                                 .graph(std::move(graph_endpoints->server))
                                 .name(fidl::StringView::FromExternal("AudioCoreGraph"))
                                 // TODO(https://fxbug.dev/42181009): set .fidl_deadline_profile()
                                 // to the same profile used for the main FIDL thread (see main.cc)
                                 .Build());
  if (!result.ok()) {
    FX_LOGS(FATAL) << "CreateGraph failed with status="
                   << fidl::ToUnderlying(result->error_value());
  }
  return std::make_shared<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>>(
      std::move(graph_endpoints->client), fidl_dispatcher);
}

// Servers from `shared/` use an older style of API. This function bridges the older style.
template <typename ProtocolT, typename HandlerT>
void PublishHandler(component::OutgoingDirectory& outgoing, HandlerT handler) {
  auto result =
      outgoing.AddUnmanagedProtocol<ProtocolT>([handler = std::move(handler)](auto server_end) {
        handler(fidl::NaturalToHLCPP(std::move(server_end)));
      });
  if (!result.is_ok()) {
    FX_PLOGS(FATAL, result.error_value()) << "Failed to publish service";
  }
}

// Servers from `v2/` usee the newer style of API.
template <typename ProtocolT>
void PublishProtocol(component::OutgoingDirectory& outgoing,
                     fit::function<void(fidl::ServerEnd<ProtocolT>)> handler) {
  auto result = outgoing.AddUnmanagedProtocol<ProtocolT>(
      [handler = std::move(handler)](auto server_end) { handler(std::move(server_end)); });
  if (!result.is_ok()) {
    FX_PLOGS(FATAL, result.error_value()) << "Failed to publish service";
  }
}

}  // namespace

AudioCoreComponent::AudioCoreComponent(component::OutgoingDirectory& outgoing,
                                       std::shared_ptr<const FidlThread> fidl_thread,
                                       bool enable_cobalt)
    : fidl_thread_(fidl_thread),
      // Load configs
      process_config_(LoadProcessConfig()),
      policy_config_(PolicyLoader::LoadPolicy()) {
  const auto fidl_dispatcher = fidl_thread->dispatcher();

  // Start a thread for background tasks.
  io_loop_ = std::make_unique<async::Loop>(&kAsyncLoopConfigNoAttachToCurrentThread);
  io_loop_->StartThread("io");
  const auto io_dispatcher = io_loop_->dispatcher();

  // Pin all memory pages backed by executable files.
  auto result = AcquireMemoryRole(zx::vmar::root_self(), "fuchsia.media.audio.core");
  if (result.is_error()) {
    FX_PLOGS(ERROR, result.status_value())
        << "Unable to set memory role for the audio_core process";
  }

  // TODO(https://fxbug.dev/42181009): replace uses of this function with newer functions such as
  // component::Connect().
  auto component_context = sys::ComponentContext::Create();

  // Initialize metrics reporting and tracing before creating any objects.
  Reporter::InitializeSingleton(*component_context, fidl_dispatcher, io_dispatcher, enable_cobalt);
  Reporter::Singleton().SetNumThermalStates(process_config_.thermal_config().states().size());
  trace_provider_ = std::make_unique<trace::TraceProviderWithFdio>(io_dispatcher);

  // Connect to the mixer service.
  graph_client_ = MakeGraphClient(fidl_dispatcher);

  // TODO(https://fxbug.dev/42181009): Create two graph threads by calling
  // graph_client_->sync()->CreateThread twice: once for input devices + capturers, and once for
  // output devices + renderers.

  // Create objects.
  stream_volume_manager_ = std::make_shared<StreamVolumeManager>(fidl_dispatcher);
  activity_dispatcher_ = std::make_shared<ActivityDispatcherImpl>();
  profile_provider_ =
      std::make_unique<ProfileProvider>(*component_context, process_config_.mix_profile_config());
  usage_gain_reporter_ = std::make_shared<UsageGainReporterImpl>(
      empty_device_lister_, *stream_volume_manager_, process_config_);
  usage_reporter_ = std::make_shared<UsageReporterImpl>();
  route_graph_ = std::make_shared<RouteGraph>(graph_client_);
  renderer_capturer_creator_ =
      std::make_shared<RendererCapturerCreator>(fidl_thread, graph_client_, route_graph_);

  // Publish services.
  PublishHandler<fuchsia_media::ActivityReporter>(outgoing,
                                                  activity_dispatcher_->GetFidlRequestHandler());
  PublishHandler<fuchsia_media::ProfileProvider>(outgoing,
                                                 profile_provider_->GetFidlRequestHandler());
  PublishHandler<fuchsia_media::UsageGainReporter>(outgoing,
                                                   usage_gain_reporter_->GetFidlRequestHandler());
  PublishHandler<fuchsia_media::UsageReporter>(outgoing, usage_reporter_->GetFidlRequestHandler());

  PublishProtocol<fuchsia_media::Audio>(outgoing, [this](auto server_end) mutable {
    AudioServer::Create(fidl_thread_, std::move(server_end), renderer_capturer_creator_);
  });

  PublishProtocol<fuchsia_media::AudioCore>(outgoing, [this](auto server_end) mutable {
    AudioCoreServer::Create(fidl_thread_, std::move(server_end),
                            {
                                .creator = renderer_capturer_creator_,
                                .route_graph = route_graph_,
                                .stream_volume_manager = stream_volume_manager_,
                                // TODO(https://fxbug.dev/42181009): set `.audio_admin` here
                                .default_volume_curve = process_config_.default_volume_curve(),
                            });
  });

  PublishProtocol<fuchsia_ultrasound::Factory>(outgoing, [this](auto server_end) mutable {
    UltrasoundFactoryServer::Create(
        fidl_thread_, std::move(server_end),
        {
            .creator = renderer_capturer_creator_,
            // TODO(https://fxbug.dev/42181009): add ultrasound render and capture formats to
            // process_config, then set the values below. For now these are placeholders.
            //
            // In audio_core/v1, these formats were computed lazily once the renderer or capturer
            // was routed to an actual device (at which point we could directly use the format from
            // the actual device). This is harder to do with the audio_core/v2 APIs.
            //
            // In practice, the ultrasound formats are fixed per product, so there's no harm in
            // hardcoding that format in audio_core_config.json. Also, ultrasound's sample_type is
            // ALWAYS kFloat32, so only channels and fps need to specified (separately for the
            // renderer and capturer).
            .renderer_format = Format::CreateOrDie({
                .sample_type = fuchsia_audio::SampleType::kFloat32,
                .channels = 1,
                .frames_per_second = 96000,
            }),
            .capturer_format = Format::CreateOrDie({
                .sample_type = fuchsia_audio::SampleType::kFloat32,
                .channels = 1,
                .frames_per_second = 96000,
            }),
        });
  });
}

}  // namespace media_audio
