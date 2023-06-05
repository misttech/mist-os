// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/logging.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <iomanip>
#include <memory>
#include <sstream>

#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/control_creator_server.h"
#include "src/media/audio/services/device_registry/control_server.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/observer_server.h"
#include "src/media/audio/services/device_registry/provider_server.h"
#include "src/media/audio/services/device_registry/registry_server.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"

namespace media_audio {

std::string UidToString(std::optional<UniqueId> unique_instance_id) {
  if (!unique_instance_id) {
    return "NONE";
  }

  auto id = *unique_instance_id;
  char s[35];
  sprintf(s,
          "0x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x",  //
          id[0], id[1], id[2], id[3], id[4], id[5], id[6], id[7],                //
          id[8], id[9], id[10], id[11], id[12], id[13], id[14], id[15]);
  s[34] = '\0';
  std::string str(s);

  if (id[0] == 0x55 && id[1] == 0x53 && id[2] == 0x42) {  // ASCII 'U', 'S', 'B'
    str += "  (in the range reserved for USB devices)";   //
  } else if (id[0] == 0x42 && id[1] == 0x54) {            // ASCII 'B', 'T'
    str += "  (in the range reserved for Bluetooth devices)";
  }
  return str;
}

void LogStreamProperties(const fuchsia_hardware_audio::StreamProperties& props) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/StreamProperties:";

  FX_LOGS(INFO) << "    unique_id         " << UidToString(props.unique_id());
  FX_LOGS(INFO) << "    is_input          "
                << (props.is_input() ? (*props.is_input() ? "TRUE" : "FALSE")
                                     : "NONE (non-compliant)");
  FX_LOGS(INFO) << "    can_mute          "
                << (props.can_mute() ? (*props.can_mute() ? "TRUE" : "FALSE")
                                     : "NONE (cannot mute)");
  FX_LOGS(INFO) << "    can_agc           "
                << (props.can_agc() ? (*props.can_agc() ? "TRUE" : "FALSE")
                                    : "NONE (cannot enable AGC)");
  if (props.min_gain_db()) {
    FX_LOGS(INFO) << "    min_gain_db       " << *props.min_gain_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    min_gain_db       NONE (non-compliant)";
  }
  if (props.max_gain_db()) {
    FX_LOGS(INFO) << "    max_gain_db       " << *props.max_gain_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    max_gain_db       NONE (non-compliant)";
  }
  if (props.gain_step_db()) {
    FX_LOGS(INFO) << "    gain_step_db      " << *props.gain_step_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    gain_step_db      NONE (non-compliant)";
  }
  if (props.plug_detect_capabilities()) {
    FX_LOGS(INFO) << "    plug_detect_caps  " << *props.plug_detect_capabilities();
  } else {
    FX_LOGS(INFO) << "    plug_detect_caps  NONE (non-compliant)";
  }
  FX_LOGS(INFO) << "    manufacturer      "
                << (props.manufacturer()
                        ? ("'" +
                           std::string(props.manufacturer()->data(), props.manufacturer()->size()) +
                           "'")
                        : "NONE");
  FX_LOGS(INFO) << "    product           "
                << (props.product()
                        ? ("'" + std::string(props.product()->data(), props.product()->size()) +
                           "'")
                        : "NONE");

  std::string clock_domain_str{"   clock _domain     "};
  if (props.clock_domain()) {
    clock_domain_str += std::to_string(*props.clock_domain());
    if (*props.clock_domain() == fuchsia_hardware_audio::kClockDomainMonotonic) {
      clock_domain_str += "  (CLOCK_DOMAIN_MONOTONIC)";
    } else if (*props.clock_domain() == fuchsia_hardware_audio::kClockDomainExternal) {
      clock_domain_str += "  (CLOCK_DOMAIN_EXTERNAL)";
    }
  } else {
    clock_domain_str += "NONE (non-compliant)";
  }
  FX_LOGS(INFO) << clock_domain_str;
}

void LogSupportedFormats(const std::vector<fuchsia_hardware_audio::SupportedFormats>& formats) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/SupportedFormats:";
  FX_LOGS(INFO) << "    supported_formats[" << formats.size() << "]:";
  for (auto idx = 0u; idx < formats.size(); ++idx) {
    auto supported_formats = formats[idx];
    if (!supported_formats.pcm_supported_formats()) {
      FX_LOGS(INFO) << "      [" << idx << "] NONE (non-compliant)";
      continue;
    }
    FX_LOGS(INFO) << "      [" << idx << "] pcm_supported_formats:";
    const auto& pcm_formats = *supported_formats.pcm_supported_formats();
    if (pcm_formats.channel_sets()) {
      const auto& channel_sets = *pcm_formats.channel_sets();
      FX_LOGS(INFO) << "            channel_sets[" << channel_sets.size() << "]:";
      for (auto idx = 0u; idx < channel_sets.size(); ++idx) {
        if (!channel_sets[idx].attributes()) {
          FX_LOGS(INFO) << "              [" << idx << "] NONE (non-compliant)";
          continue;
        }
        const auto& attribs = *channel_sets[idx].attributes();
        FX_LOGS(INFO) << "              [" << idx << "] attributes[" << attribs.size() << "]";
        for (auto idx = 0u; idx < attribs.size(); ++idx) {
          FX_LOGS(INFO) << "                  [" << idx << "]:";
          FX_LOGS(INFO) << "                     min_frequency   "
                        << (attribs[idx].min_frequency()
                                ? std::to_string(*attribs[idx].min_frequency())
                                : "NONE");
          FX_LOGS(INFO) << "                     max_frequency   "
                        << (attribs[idx].max_frequency()
                                ? std::to_string(*attribs[idx].max_frequency())
                                : "NONE");
        }
      }
    } else {
      FX_LOGS(INFO) << "            NONE (non-compliant)";
    }

    if (pcm_formats.sample_formats()) {
      const auto& sample_formats = *pcm_formats.sample_formats();
      FX_LOGS(INFO) << "            sample_formats[" << sample_formats.size() << "]:";
      for (auto idx = 0u; idx < sample_formats.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    " << sample_formats[idx];
      }
    } else {
      FX_LOGS(INFO) << "            NONE (non-compliant)";
    }
    if (pcm_formats.bytes_per_sample()) {
      const auto& bytes_per_sample = *pcm_formats.bytes_per_sample();
      FX_LOGS(INFO) << "            bytes_per_sample[" << bytes_per_sample.size() << "]:";
      for (auto idx = 0u; idx < bytes_per_sample.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    "
                      << static_cast<int16_t>(bytes_per_sample[idx]);
      }
    } else {
      FX_LOGS(INFO) << "            NONE (non-compliant)";
    }
    if (pcm_formats.valid_bits_per_sample()) {
      const auto& valid_bits_per_sample = *pcm_formats.valid_bits_per_sample();
      FX_LOGS(INFO) << "            valid_bits_per_sample[" << valid_bits_per_sample.size() << "]:";
      for (auto idx = 0u; idx < valid_bits_per_sample.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    "
                      << static_cast<int16_t>(valid_bits_per_sample[idx]);
      }
    } else {
      FX_LOGS(INFO) << "            NONE (non-compliant)";
    }
    if (pcm_formats.frame_rates()) {
      const auto& frame_rates = *pcm_formats.frame_rates();
      FX_LOGS(INFO) << "            frame_rates[" << frame_rates.size() << "]:";
      for (auto idx = 0u; idx < frame_rates.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    " << frame_rates[idx];
      }
    } else {
      FX_LOGS(INFO) << "            NONE (non-compliant)";
    }
  }
}

void LogGainState(const fuchsia_hardware_audio::GainState& gain_state) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/GainState:";
  FX_LOGS(INFO) << "    muted            "
                << (gain_state.muted() ? (*gain_state.muted() ? "TRUE" : "FALSE")
                                       : "NONE (Unmuted)");
  FX_LOGS(INFO) << "    agc_enabled      "
                << (gain_state.agc_enabled() ? (*gain_state.agc_enabled() ? "TRUE" : "FALSE")
                                             : "NONE (Disabled)");
  if (gain_state.gain_db()) {
    FX_LOGS(INFO) << "    gain_db          " << *gain_state.gain_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    gain_db          NONE (non-compliant)";
  }
}

void LogPlugState(const fuchsia_hardware_audio::PlugState& plug_state) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/PlugState:";
  FX_LOGS(INFO) << "    plugged          "
                << (plug_state.plugged() ? (*plug_state.plugged() ? "TRUE" : "FALSE")
                                         : "NONE (non-compliant)");
  FX_LOGS(INFO) << "    plug_state_time  "
                << (plug_state.plug_state_time() ? std::to_string(*plug_state.plug_state_time())
                                                 : "NONE (non-compliant)");
}

void LogDeviceInfo(const fuchsia_audio_device::Info& device_info) {
  if constexpr (!kLogFinalDeviceInfo) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_audio_device/Info:";

  FX_LOGS(INFO) << "   token_id          "
                << (device_info.token_id() ? std::to_string(*device_info.token_id())
                                           : "NONE (non-compliant)");

  FX_LOGS(INFO) << "   device_type       " << device_info.device_type();

  FX_LOGS(INFO) << "   device_name       "
                << (device_info.device_name() ? std::string("'") + *device_info.device_name() + "'"
                                              : "NONE");

  FX_LOGS(INFO) << "   manufacturer      "
                << (device_info.manufacturer() ? ("'" +
                                                  std::string(device_info.manufacturer()->data(),
                                                              device_info.manufacturer()->size()) +
                                                  "'")
                                               : "NONE");

  FX_LOGS(INFO) << "   product           "
                << (device_info.product() ? ("'" +
                                             std::string(device_info.product()->data(),
                                                         device_info.product()->size()) +
                                             "'")
                                          : "NONE");

  FX_LOGS(INFO) << "   unique_instance_id " << UidToString(device_info.unique_instance_id());

  if (device_info.supported_formats()) {
    FX_LOGS(INFO) << "   supported_formats[" << device_info.supported_formats()->size() << "]";
    for (auto idx = 0u; idx < device_info.supported_formats()->size(); ++idx) {
      const auto& pcm_format_set = device_info.supported_formats()->at(idx);
      if (pcm_format_set.channel_sets()) {
        FX_LOGS(INFO) << "    [" << idx << "]  channel_sets["
                      << pcm_format_set.channel_sets()->size() << "]";
        for (auto idx = 0u; idx < pcm_format_set.channel_sets()->size(); ++idx) {
          const auto& channel_set = pcm_format_set.channel_sets()->at(idx);
          if (channel_set.attributes()) {
            FX_LOGS(INFO) << "          [" << idx << "]  attributes["
                          << channel_set.attributes()->size() << "]";
            for (auto idx = 0u; idx < channel_set.attributes()->size(); ++idx) {
              const auto& attributes = channel_set.attributes()->at(idx);
              if (attributes.min_frequency()) {
                FX_LOGS(INFO) << "                [" << idx << "]  min_frequency  "
                              << *attributes.min_frequency();
              } else {
                FX_LOGS(INFO) << "                [" << idx << "]  min_frequency  NONE";
              }
              if (attributes.max_frequency()) {
                FX_LOGS(INFO) << "                     max_frequency  "
                              << *attributes.max_frequency();
              } else {
                FX_LOGS(INFO) << "                     max_frequency  NONE";
              }
            }
          } else {
            FX_LOGS(INFO) << "          [" << idx << "]  attributes  NONE  (non-compliant)";
          }
        }
      } else {
        FX_LOGS(INFO) << "    [" << idx << "]  channel_sets     NONE (non-compliant)";
      }

      if (pcm_format_set.sample_types()) {
        FX_LOGS(INFO) << "         sample_types[" << pcm_format_set.sample_types()->size() << "]";
        for (auto idx = 0u; idx < pcm_format_set.sample_types()->size(); ++idx) {
          FX_LOGS(INFO) << "          [" << idx << "]  " << pcm_format_set.sample_types()->at(idx);
        }
      } else {
        FX_LOGS(INFO) << "         sample_types     NONE (non-compliant)";
      }
      if (pcm_format_set.frame_rates()) {
        FX_LOGS(INFO) << "         frame_rates[" << pcm_format_set.frame_rates()->size() << "]";
        for (auto idx = 0u; idx < pcm_format_set.frame_rates()->size(); ++idx) {
          FX_LOGS(INFO) << "          [" << idx << "]  " << pcm_format_set.frame_rates()->at(idx);
        }
      } else {
        FX_LOGS(INFO) << "         frame_rates      NONE (non-compliant)";
      }
    }
  } else {
    FX_LOGS(INFO) << "    supported_formats     NONE (non-compliant)";
  }

  if (device_info.gain_caps()) {
    if (device_info.gain_caps()->min_gain_db()) {
      FX_LOGS(INFO) << "   gain_caps         min_gain_db   "
                    << *device_info.gain_caps()->min_gain_db() << " dB";
    } else {
      FX_LOGS(INFO) << "   gain_caps         min_gain_db   NONE (non-compliant)";
    }
    if (device_info.gain_caps()->max_gain_db()) {
      FX_LOGS(INFO) << "                     max_gain_db   "
                    << *device_info.gain_caps()->max_gain_db() << " dB";
    } else {
      FX_LOGS(INFO) << "                     max_gain_db   NONE (non-compliant)";
    }
    if (device_info.gain_caps()->gain_step_db()) {
      FX_LOGS(INFO) << "                     gain_step_db  "
                    << *device_info.gain_caps()->gain_step_db() << " dB";
    } else {
      FX_LOGS(INFO) << "                     gain_step_db  NONE (non-compliant)";
    }
    FX_LOGS(INFO) << "                     can_mute      "
                  << (device_info.gain_caps()->can_mute()
                          ? (*device_info.gain_caps()->can_mute() ? "true" : "false")
                          : "NONE (false)");
    FX_LOGS(INFO) << "                     can_agc       "
                  << (device_info.gain_caps()->can_agc()
                          ? (*device_info.gain_caps()->can_agc() ? "true" : "false")
                          : "NONE (false)");
  } else {
    FX_LOGS(INFO) << "   gain_caps         NONE (non-compliant)";
  }

  FX_LOGS(INFO) << "   plug_detect_caps  " << device_info.plug_detect_caps();

  std::string clock_domain_str{"   clock_domain      "};
  if (device_info.clock_domain()) {
    clock_domain_str += std::to_string(*device_info.clock_domain());
    if (*device_info.clock_domain() == fuchsia_hardware_audio::kClockDomainMonotonic) {
      clock_domain_str += "  (CLOCK_DOMAIN_MONOTONIC)";
    } else if (*device_info.clock_domain() == fuchsia_hardware_audio::kClockDomainExternal) {
      clock_domain_str += "  (CLOCK_DOMAIN_EXTERNAL)";
    }
  } else {
    clock_domain_str += "NONE (non-compliant)";
  }

  FX_LOGS(INFO) << clock_domain_str;
}

void LogRingBufferProperties(const fuchsia_hardware_audio::RingBufferProperties& props) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/RingBufferProperties:";
  if (props.external_delay()) {
    FX_LOGS(INFO) << "    external_delay          " << *props.external_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    external_delay          NONE (0 ns)";
  }

  FX_LOGS(INFO) << "    needs_cache_flush       "
                << (props.needs_cache_flush_or_invalidate()
                        ? (*props.needs_cache_flush_or_invalidate() ? "TRUE" : "FALSE")
                        : "NONE (non-compliant)");

  if (props.turn_on_delay()) {
    FX_LOGS(INFO) << "    turn_on_delay           " << *props.turn_on_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    turn_on_delay           NONE (0 ns)";
  }

  if (props.driver_transfer_bytes()) {
    FX_LOGS(INFO) << "    driver_transfer_bytes   " << *props.driver_transfer_bytes() << " bytes";
  } else {
    FX_LOGS(INFO) << "    driver_transfer_bytes   NONE (non-compliant)";
  }
}

void LogRingBufferFormat(const fuchsia_hardware_audio::Format& format) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/Format:";
  if (!format.pcm_format()) {
    FX_LOGS(INFO) << "    pcm_format           NONE (non-compliant)";
    return;
  }

  FX_LOGS(INFO) << "    pcm_format:";
  FX_LOGS(INFO) << "        number_of_channels    "
                << static_cast<uint16_t>(format.pcm_format()->number_of_channels());
  FX_LOGS(INFO) << "        sample_format         " << format.pcm_format()->sample_format();
  FX_LOGS(INFO) << "        bytes_per_sample      "
                << static_cast<uint16_t>(format.pcm_format()->bytes_per_sample());
  FX_LOGS(INFO) << "        valid_bits_per_sample "
                << static_cast<uint16_t>(format.pcm_format()->valid_bits_per_sample());
  FX_LOGS(INFO) << "        frame_rate            " << format.pcm_format()->frame_rate();
}

void LogRingBufferVmo(const zx::vmo& vmo, uint32_t num_frames,
                      fuchsia_hardware_audio::Format format) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  zx_info_handle_basic_t info;
  auto status = vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "vmo.get_info returned error:";
    return;
  }
  uint64_t size;
  status = vmo.get_size(&size);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "vmo.get_size returned size " << size << ":";
    return;
  }
  FX_LOGS(INFO) << "fuchsia_hardware_audio/Vmo:";
  FX_LOGS(INFO) << "    koid                 0x" << std::hex << info.koid;
  FX_LOGS(INFO) << "    size                 " << size << " bytes";
  FX_LOGS(INFO) << "    calculated_size      "
                << num_frames * format.pcm_format()->number_of_channels() *
                       format.pcm_format()->bytes_per_sample();
  FX_LOGS(INFO) << "        num_frames           " << num_frames;
  FX_LOGS(INFO) << "        num_channels         "
                << static_cast<uint16_t>(format.pcm_format()->number_of_channels());
  FX_LOGS(INFO) << "        bytes_per_sample     "
                << static_cast<uint16_t>(format.pcm_format()->bytes_per_sample());
}

void LogActiveChannels(uint64_t channel_bitmask, zx::time set_time) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/SetActiveChannels:";
  FX_LOGS(INFO) << "    channel_bitmask      0x" << std::setfill('0') << std::setw(2) << std::hex
                << channel_bitmask;
  FX_LOGS(INFO) << "    set_time             " << set_time.get();
}

void LogDelayInfo(const fuchsia_hardware_audio::DelayInfo& info) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/DelayInfo:";
  if (info.internal_delay()) {
    FX_LOGS(INFO) << "    internal_delay       " << *info.internal_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    internal_delay       NONE (non-compliant)";
  }

  if (info.external_delay()) {
    FX_LOGS(INFO) << "    external_delay       " << *info.external_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    external_delay       NONE (0 ns)";
  }
}

void LogObjectCounts() {
  ADR_LOG(kLogObjectCounts) << Device::count() << " Devices (" << Device::initialized_count()
                            << " active/" << Device::unhealthy_count() << " unhealthy); "
                            << ProviderServer::count() << " Prov, " << RegistryServer::count()
                            << " Reg, " << ObserverServer::count() << " Obs, "
                            << ControlCreatorServer::count() << " CtlCreators, "
                            << ControlServer::count() << " Ctls, " << RingBufferServer::count()
                            << " RingBuffs";
}

}  // namespace media_audio
