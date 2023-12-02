// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite-server.h"

#include <lib/driver/component/cpp/driver_base.h>

namespace audio::aml_g12 {

// TODO(fxbug.dev/132252): Implement audio-composite server support.
Server::Server(std::array<std::optional<fdf::MmioBuffer>, kNumberOfTdmEngines> mmios,
               async_dispatcher_t* dispatcher)
    : dispatcher_(dispatcher) {
  // TODO(fxbug.dev/132252): Configure this driver with passed in metadata.
  for (size_t i = 0; i < kNumberOfPipelines; ++i) {
    // Default supported DAI formats.
    supported_dai_formats_[i].number_of_channels({2});
    supported_dai_formats_[i].sample_formats({fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned});
    supported_dai_formats_[i].frame_formats(
        {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
            fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)});
    supported_dai_formats_[i].frame_rates({48'000});
    supported_dai_formats_[i].bits_per_slot({16, 32});
    supported_dai_formats_[i].bits_per_sample({16});

    // Take values from supported list to define current DAI format.
    current_dai_formats_[i].emplace(
        supported_dai_formats_[i].number_of_channels()[0],
        (1 << supported_dai_formats_[i].number_of_channels()[0]) - 1,  // enable all channels.
        supported_dai_formats_[i].sample_formats()[0], supported_dai_formats_[i].frame_formats()[0],
        supported_dai_formats_[i].frame_rates()[0],
        supported_dai_formats_[i].bits_per_slot()[1],  // Take 32 bits for default I2S.
        supported_dai_formats_[i].bits_per_sample()[0]);
  }

  // Output engines.
  ZX_ASSERT(ConfigEngine(0, 0, false, std::move(mmios[0].value())) == ZX_OK);
  ZX_ASSERT(ConfigEngine(1, 1, false, std::move(mmios[1].value())) == ZX_OK);
  ZX_ASSERT(ConfigEngine(2, 2, false, std::move(mmios[2].value())) == ZX_OK);

  // Input engines.
  ZX_ASSERT(ConfigEngine(3, 0, true, std::move(mmios[3].value())) == ZX_OK);
  ZX_ASSERT(ConfigEngine(4, 1, true, std::move(mmios[4].value())) == ZX_OK);
  ZX_ASSERT(ConfigEngine(5, 2, true, std::move(mmios[5].value())) == ZX_OK);
}

zx_status_t Server::ConfigEngine(size_t index, size_t dai_index, bool input, fdf::MmioBuffer mmio) {
  // Common configuration. TODO(fxbug.dev/132252): Configure this driver with passed in metadata.
  engines_[index].config = {};
  engines_[index].config.version = metadata::AmlVersion::kA311D;
  engines_[index].config.mClockDivFactor = 10;
  engines_[index].config.sClockDivFactor = 25;

  engines_[index].dai_index = dai_index;
  engines_[index].config.is_input = input;

  switch (dai_index) {
      // clang-format off
    case 0: engines_[index].config.bus = metadata::AmlBus::TDM_A; break;
    case 1: engines_[index].config.bus = metadata::AmlBus::TDM_B; break;
    case 2: engines_[index].config.bus = metadata::AmlBus::TDM_C; break;
      // clang-format on
    default:
      return ZX_ERR_INVALID_ARGS;
  }

  engines_[index].device.emplace(engines_[index].config, std::move(mmio));

  // Unconditional reset on configuration.
  return ResetEngine(index);
}

zx_status_t Server::ResetEngine(size_t index) {
  // Resets engine using AmlTdmConfigDevice, so we need to translate the current state
  // into parameters used by AmlTdmConfigDevice.
  ZX_ASSERT(engines_[index].dai_index < kNumberOfPipelines);
  ZX_ASSERT(current_dai_formats_[engines_[index].dai_index].has_value());
  const fuchsia_hardware_audio::DaiFormat& dai_format =
      *current_dai_formats_[engines_[index].dai_index];
  if (dai_format.sample_format() != fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  auto& dai_type = engines_[index].config.dai.type;
  using StandardFormat = fuchsia_hardware_audio::DaiFrameFormatStandard;
  using DaiType = metadata::DaiType;
  switch (dai_format.frame_format().frame_format_standard().value()) {
      // clang-format off
    case StandardFormat::kI2S:        dai_type = DaiType::I2s;                 break;
    case StandardFormat::kStereoLeft: dai_type = DaiType::StereoLeftJustified; break;
    case StandardFormat::kTdm1:       dai_type = DaiType::Tdm1;                break;
    case StandardFormat::kTdm2:       dai_type = DaiType::Tdm2;                break;
    case StandardFormat::kTdm3:       dai_type = DaiType::Tdm3;                break;
      // clang-format on
    case StandardFormat::kNone:
      [[fallthrough]];
    case StandardFormat::kStereoRight:
      return ZX_ERR_NOT_SUPPORTED;
  }
  engines_[index].config.dai.bits_per_sample = dai_format.bits_per_sample();
  engines_[index].config.dai.bits_per_slot = dai_format.bits_per_slot();
  engines_[index].config.dai.number_of_channels =
      static_cast<uint8_t>(dai_format.number_of_channels());
  engines_[index].config.ring_buffer.number_of_channels =
      engines_[index].config.dai.number_of_channels;
  engines_[index].config.lanes_enable_mask[0] =
      (1 << dai_format.number_of_channels()) - 1;  // enable all channels.
  zx_status_t status = AmlTdmConfigDevice::Normalize(engines_[index].config);
  if (status != ZX_OK) {
    return status;
  }

  status = engines_[index].device->InitHW(
      engines_[index].config, dai_format.channels_to_use_bitmask(), dai_format.frame_rate());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to init hardware: %s", zx_status_get_string(status));
  }
  return status;
}

void Server::Reset(ResetCompleter::Sync& completer) {
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    if (zx_status_t status = ResetEngine(i); status != ZX_OK) {
      completer.Reply(zx::error(status));
      return;
    }
  }
  completer.Reply(zx::ok());
}

void Server::GetProperties(
    fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync& completer) {
  fuchsia_hardware_audio::CompositeProperties props;
  props.clock_domain(fuchsia_hardware_audio::kClockDomainMonotonic);
  completer.Reply(std::move(props));
}

void Server::GetHealthState(GetHealthStateCompleter::Sync& completer) {
  completer.Reply(fuchsia_hardware_audio::HealthState{}.healthy(true));
}

void Server::SignalProcessingConnect(SignalProcessingConnectRequest& request,
                                     SignalProcessingConnectCompleter::Sync& completer) {
  if (signal_) {
    request.protocol().Close(ZX_ERR_ALREADY_BOUND);
    return;
  }
  signal_.emplace(dispatcher(), std::move(request.protocol()), this,
                  std::mem_fn(&Server::OnSignalProcessingClosed));
}

void Server::OnSignalProcessingClosed(fidl::UnbindInfo info) {
  if (info.is_peer_closed()) {
    FDF_LOG(INFO, "Client disconnected");
  } else if (!info.is_user_initiated()) {
    // Do not log canceled cases; these happen particularly frequently in certain test cases.
    if (info.status() != ZX_ERR_CANCELED) {
      FDF_LOG(ERROR, "Client connection unbound: %s", info.status_string());
    }
  }
  if (signal_) {
    signal_.reset();
  }
}

void Server::GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                                  GetRingBufferFormatsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::CreateRingBuffer(CreateRingBufferRequest& request,
                              CreateRingBufferCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetDaiFormats(GetDaiFormatsRequest& request, GetDaiFormatsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetProperties(
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync& completer) {
  completer.Reply({});
}

void Server::GetVmo(
    GetVmoRequest& request,
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetVmoCompleter::Sync& completer) {
  completer.Reply(zx::error(fuchsia_hardware_audio::GetVmoError::kInternalError));
}

void Server::Start(StartCompleter::Sync& completer) { completer.Reply({}); }

void Server::Stop(StopCompleter::Sync& completer) { completer.Reply(); }

void Server::WatchClockRecoveryPositionInfo(
    WatchClockRecoveryPositionInfoCompleter::Sync& completer) {
  completer.Reply({});
}

void Server::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) { completer.Reply({}); }

void Server::SetActiveChannels(fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
                               SetActiveChannelsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetElements(GetElementsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::WatchElementState(WatchElementStateRequest& request,
                               WatchElementStateCompleter::Sync& completer) {
  completer.Reply({});
}

void Server::SetElementState(SetElementStateRequest& request,
                             SetElementStateCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::WatchTopology(WatchTopologyCompleter::Sync& completer) { completer.Reply({}); }

void Server::SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

}  // namespace audio::aml_g12
