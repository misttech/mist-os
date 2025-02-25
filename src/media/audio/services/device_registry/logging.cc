// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/logging.h"

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <iomanip>
#include <string>

#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/control_creator_server.h"
#include "src/media/audio/services/device_registry/control_server.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/observer_server.h"
#include "src/media/audio/services/device_registry/provider_server.h"
#include "src/media/audio/services/device_registry/registry_server.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"

namespace media_audio {

namespace fad = fuchsia_audio_device;
namespace fha = fuchsia_hardware_audio;
namespace fhasp = fuchsia_hardware_audio_signalprocessing;

std::string UidToString(std::optional<UniqueId> unique_instance_id) {
  if (!unique_instance_id.has_value()) {
    return "<none>";
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

void LogElementRingBufferFormatSets(
    const std::vector<fad::ElementRingBufferFormatSet>& element_ring_buffer_format_sets) {
  if constexpr (!kLogCompositeFidlResponseValues) {
    return;
  }

  for (const auto& element_ring_buffer_format_set : element_ring_buffer_format_sets) {
    LogElementRingBufferFormatSet(element_ring_buffer_format_set);
  }
}

void LogElementRingBufferFormatSet(
    const fad::ElementRingBufferFormatSet& element_ring_buffer_format_set) {
  if constexpr (!kLogCompositeFidlResponseValues) {
    return;
  }

  if (element_ring_buffer_format_set.element_id().has_value()) {
    FX_LOGS(INFO) << "   .element_id  " << *element_ring_buffer_format_set.element_id();
  } else {
    FX_LOGS(INFO) << "   .element_id  <none> (non-compliant)";
  }
  if (element_ring_buffer_format_set.format_sets().has_value()) {
    FX_LOGS(INFO) << "   .format_set  [" << element_ring_buffer_format_set.format_sets()->size()
                  << "]";
    LogTranslatedRingBufferFormatSets(*element_ring_buffer_format_set.format_sets());
  } else {
    FX_LOGS(INFO) << "   .format_set  <none> (non-compliant)";
  }
}

void LogTranslatedRingBufferFormatSets(
    const std::vector<fad::PcmFormatSet>& translated_ring_buffer_format_sets) {
  if constexpr (!kLogCompositeFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_audio_device/translated_ring_buffer_format_sets";
  FX_LOGS(INFO) << "    PcmFormatSet[" << translated_ring_buffer_format_sets.size() << "]";
  for (auto idx = 0u; idx < translated_ring_buffer_format_sets.size(); ++idx) {
    FX_LOGS(INFO) << "      [" << idx << "]";
    LogTranslatedRingBufferFormatSet(translated_ring_buffer_format_sets[idx]);
  }
}

void LogTranslatedRingBufferFormatSet(const fad::PcmFormatSet& translated_ring_buffer_format_set) {
  if constexpr (!kLogCompositeFidlResponseValues) {
    return;
  }

  if (translated_ring_buffer_format_set.channel_sets().has_value()) {
    const auto& channel_sets = *translated_ring_buffer_format_set.channel_sets();
    FX_LOGS(INFO) << "            channel_sets  [" << channel_sets.size() << "]";
    for (auto idx = 0u; idx < channel_sets.size(); ++idx) {
      if (!channel_sets[idx].attributes().has_value()) {
        FX_LOGS(INFO) << "              [" << idx << "] <none> (non-compliant)";
        continue;
      }
      const auto& attribs = *channel_sets[idx].attributes();
      FX_LOGS(INFO) << "              [" << idx << "] attributes[" << attribs.size() << "]";
      for (auto idx = 0u; idx < attribs.size(); ++idx) {
        FX_LOGS(INFO) << "                  [" << idx << "]";
        FX_LOGS(INFO) << "                     min_frequency   "
                      << (attribs[idx].min_frequency().has_value()
                              ? std::to_string(*attribs[idx].min_frequency())
                              : "<none>");
        FX_LOGS(INFO) << "                     max_frequency   "
                      << (attribs[idx].max_frequency().has_value()
                              ? std::to_string(*attribs[idx].max_frequency())
                              : "<none>");
      }
    }
  } else {
    FX_LOGS(INFO) << "            channel_sets  <none> (non-compliant)";
  }

  if (translated_ring_buffer_format_set.sample_types().has_value()) {
    const auto& sample_types = *translated_ring_buffer_format_set.sample_types();
    FX_LOGS(INFO) << "            sample_types [" << sample_types.size() << "]";
    for (auto idx = 0u; idx < sample_types.size(); ++idx) {
      FX_LOGS(INFO) << "              [" << idx << "]    " << sample_types[idx];
    }
  } else {
    FX_LOGS(INFO) << "            sample_types <none> (non-compliant)";
  }

  if (translated_ring_buffer_format_set.frame_rates().has_value()) {
    const auto& frame_rates = *translated_ring_buffer_format_set.frame_rates();
    FX_LOGS(INFO) << "            frame_rates  [" << frame_rates.size() << "]";
    for (auto idx = 0u; idx < frame_rates.size(); ++idx) {
      FX_LOGS(INFO) << "              [" << idx << "]    " << frame_rates[idx];
    }
  } else {
    FX_LOGS(INFO) << "            frame_rates  <none> (non-compliant)";
  }
}

void LogRingBufferFormatSets(const std::vector<fha::SupportedFormats>& ring_buffer_format_sets) {
  if constexpr (!kLogCompositeFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/SupportedFormats";
  FX_LOGS(INFO) << "    ring_buffer_format_sets [" << ring_buffer_format_sets.size() << "]";
  for (auto idx = 0u; idx < ring_buffer_format_sets.size(); ++idx) {
    auto ring_buffer_format_set = ring_buffer_format_sets[idx];
    if (!ring_buffer_format_set.pcm_supported_formats().has_value()) {
      FX_LOGS(INFO) << "      [" << idx << "] <none> (non-compliant)";
      continue;
    }
    FX_LOGS(INFO) << "      [" << idx << "] pcm_supported_formats";
    const auto& pcm_format_set = *ring_buffer_format_set.pcm_supported_formats();
    if (pcm_format_set.channel_sets().has_value()) {
      const auto& channel_sets = *pcm_format_set.channel_sets();
      FX_LOGS(INFO) << "            channel_sets [" << channel_sets.size() << "]";
      for (auto idx = 0u; idx < channel_sets.size(); ++idx) {
        if (!channel_sets[idx].attributes().has_value()) {
          FX_LOGS(INFO) << "              [" << idx << "] <none> (non-compliant)";
          continue;
        }
        const auto& attribs = *channel_sets[idx].attributes();
        FX_LOGS(INFO) << "              [" << idx << "] attributes [" << attribs.size() << "]";
        for (auto idx = 0u; idx < attribs.size(); ++idx) {
          FX_LOGS(INFO) << "                   [" << idx << "]";
          FX_LOGS(INFO) << "                     min_frequency   "
                        << (attribs[idx].min_frequency().has_value()
                                ? std::to_string(*attribs[idx].min_frequency())
                                : "<none>");
          FX_LOGS(INFO) << "                     max_frequency   "
                        << (attribs[idx].max_frequency().has_value()
                                ? std::to_string(*attribs[idx].max_frequency())
                                : "<none>");
        }
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }

    if (pcm_format_set.sample_formats().has_value()) {
      const auto& sample_formats = *pcm_format_set.sample_formats();
      FX_LOGS(INFO) << "            sample_formats [" << sample_formats.size() << "]";
      for (auto idx = 0u; idx < sample_formats.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    " << sample_formats[idx];
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }
    if (pcm_format_set.bytes_per_sample().has_value()) {
      const auto& bytes_per_sample = *pcm_format_set.bytes_per_sample();
      FX_LOGS(INFO) << "            bytes_per_sample [" << bytes_per_sample.size() << "]";
      for (auto idx = 0u; idx < bytes_per_sample.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    "
                      << static_cast<int16_t>(bytes_per_sample[idx]);
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }
    if (pcm_format_set.valid_bits_per_sample().has_value()) {
      const auto& valid_bits_per_sample = *pcm_format_set.valid_bits_per_sample();
      FX_LOGS(INFO) << "            valid_bits_per_sample [" << valid_bits_per_sample.size() << "]";
      for (auto idx = 0u; idx < valid_bits_per_sample.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    "
                      << static_cast<int16_t>(valid_bits_per_sample[idx]);
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }
    if (pcm_format_set.frame_rates().has_value()) {
      const auto& frame_rates = *pcm_format_set.frame_rates();
      FX_LOGS(INFO) << "            frame_rates [" << frame_rates.size() << "]";
      for (auto idx = 0u; idx < frame_rates.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    " << frame_rates[idx];
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }
  }
}

void LogPlugState(const fha::PlugState& plug_state) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/PlugState";
  FX_LOGS(INFO) << "    plugged          "
                << (plug_state.plugged().has_value() ? (*plug_state.plugged() ? "TRUE" : "FALSE")
                                                     : "<none> (non-compliant)");
  FX_LOGS(INFO) << "    plug_state_time  "
                << (plug_state.plug_state_time().has_value()
                        ? std::to_string(*plug_state.plug_state_time())
                        : "<none> (non-compliant)");
}

void LogCodecProperties(const fha::CodecProperties& codec_props) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/CodecProperties";

  FX_LOGS(INFO) << "    is_input          "
                << (codec_props.is_input().has_value()
                        ? (*codec_props.is_input() ? "TRUE" : "FALSE")
                        : "<none> (non-compliant)");
  FX_LOGS(INFO) << "    manufacturer      "
                << (codec_props.manufacturer().has_value()
                        ? ("'" +
                           std::string(codec_props.manufacturer()->data(),
                                       codec_props.manufacturer()->size()) +
                           "'")
                        : "<none>");
  FX_LOGS(INFO)
      << "    product           "
      << (codec_props.product().has_value()
              ? ("'" + std::string(codec_props.product()->data(), codec_props.product()->size()) +
                 "'")
              : "<none>");
  FX_LOGS(INFO) << "    unique_id         " << UidToString(codec_props.unique_id());
  if (codec_props.plug_detect_capabilities().has_value()) {
    FX_LOGS(INFO) << "    plug_detect_caps  " << *codec_props.plug_detect_capabilities();
  } else {
    FX_LOGS(INFO) << "    plug_detect_caps  <none> (non-compliant)";
  }
}

void LogElementDaiFormatSets(const std::vector<fad::ElementDaiFormatSet>& element_dai_format_sets) {
  if constexpr (!kLogCodecFidlResponseValues && !kLogCompositeFidlResponseValues) {
    return;
  }

  for (const auto& element_dai_format_set : element_dai_format_sets) {
    LogElementDaiFormatSet(element_dai_format_set);
  }
}

void LogElementDaiFormatSet(const fad::ElementDaiFormatSet& element_dai_format_set) {
  if constexpr (!kLogCodecFidlResponseValues && !kLogCompositeFidlResponseValues) {
    return;
  }

  if (element_dai_format_set.element_id().has_value()) {
    FX_LOGS(INFO) << "   .element_id  " << *element_dai_format_set.element_id();
  } else {
    FX_LOGS(INFO) << "   .element_id  <none> (non-compliant)";
  }
  if (element_dai_format_set.format_sets().has_value()) {
    FX_LOGS(INFO) << "   .format_set  [" << element_dai_format_set.format_sets()->size() << "]";
    LogDaiFormatSets(*element_dai_format_set.format_sets());
  } else {
    FX_LOGS(INFO) << "   .format_set  <none> (non-compliant)";
  }
}

void LogDaiFormatSets(const std::vector<fha::DaiSupportedFormats>& dai_format_sets) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/DaiSupportedFormats";
  FX_LOGS(INFO) << "    dai_supported_formats [" << dai_format_sets.size() << "]";
  for (auto idx = 0u; idx < dai_format_sets.size(); ++idx) {
    auto dai_format_set = dai_format_sets[idx];
    FX_LOGS(INFO) << "      [" << idx << "]";

    const auto& channel_counts = dai_format_set.number_of_channels();
    FX_LOGS(INFO) << "        number_of_channels [" << channel_counts.size() << "]";
    for (auto idx = 0u; idx < channel_counts.size(); ++idx) {
      FX_LOGS(INFO) << "          [" << idx << "]   " << channel_counts[idx];
    }

    const auto& sample_formats = dai_format_set.sample_formats();
    FX_LOGS(INFO) << "        sample_formats [" << sample_formats.size() << "]";
    for (auto idx = 0u; idx < sample_formats.size(); ++idx) {
      FX_LOGS(INFO) << "          [" << idx << "]    " << sample_formats[idx];
    }

    const auto& frame_formats = dai_format_set.frame_formats();
    FX_LOGS(INFO) << "        frame_formats [" << frame_formats.size() << "]";
    for (auto idx = 0u; idx < frame_formats.size(); ++idx) {
      FX_LOGS(INFO) << "          [" << idx << "]    " << frame_formats[idx];
    }

    const auto& frame_rates = dai_format_set.frame_rates();
    FX_LOGS(INFO) << "        frame_rates [" << frame_rates.size() << "]";
    for (auto idx = 0u; idx < frame_rates.size(); ++idx) {
      FX_LOGS(INFO) << "          [" << idx << "]    " << frame_rates[idx];
    }

    const auto& bits_per_slot = dai_format_set.bits_per_slot();
    FX_LOGS(INFO) << "        bits_per_slot [" << bits_per_slot.size() << "]";
    for (auto idx = 0u; idx < bits_per_slot.size(); ++idx) {
      FX_LOGS(INFO) << "          [" << idx << "]    " << static_cast<int16_t>(bits_per_slot[idx]);
    }

    const auto& bits_per_sample = dai_format_set.bits_per_sample();
    FX_LOGS(INFO) << "        bits_per_sample [" << bits_per_sample.size() << "]";
    for (auto idx = 0u; idx < bits_per_sample.size(); ++idx) {
      FX_LOGS(INFO) << "          [" << idx << "]    "
                    << static_cast<int16_t>(bits_per_sample[idx]);
    }
  }
}

void LogDaiFormat(std::optional<fha::DaiFormat> dai_format) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/DaiFormat";
  if (!dai_format.has_value()) {
    FX_LOGS(INFO) << "    UNSET";
    return;
  }
  FX_LOGS(INFO) << "    number_of_channels      " << dai_format->number_of_channels();
  FX_LOGS(INFO) << "    channels_to_use_bitmask " << std::hex
                << dai_format->channels_to_use_bitmask();
  FX_LOGS(INFO) << "    sample_format           " << dai_format->sample_format();
  FX_LOGS(INFO) << "    frame_format            " << dai_format->frame_format();
  FX_LOGS(INFO) << "    frame_rate              " << dai_format->frame_rate();
  FX_LOGS(INFO) << "    bits_per_slot           "
                << static_cast<uint16_t>(dai_format->bits_per_slot());
  FX_LOGS(INFO) << "    bits_per_sample         "
                << static_cast<uint16_t>(dai_format->bits_per_sample());
}

void LogCodecFormatInfo(std::optional<fha::CodecFormatInfo> format_info) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/CodecFormatInfo";
  if (!format_info.has_value()) {
    FX_LOGS(INFO) << "    UNSET";
    return;
  }
  FX_LOGS(INFO) << "    external_delay (ns)   "
                << (format_info->external_delay().has_value()
                        ? std::to_string(*format_info->external_delay())
                        : "<none>");
  FX_LOGS(INFO) << "    turn_on_delay  (ns)   "
                << (format_info->turn_on_delay().has_value()
                        ? std::to_string(*format_info->turn_on_delay())
                        : "<none>");
  FX_LOGS(INFO) << "    turn_off_delay (ns)   "
                << (format_info->turn_off_delay().has_value()
                        ? std::to_string(*format_info->turn_off_delay())
                        : "<none>");
}

void LogCompositeProperties(const fha::CompositeProperties& composite_props) {
  if constexpr (!kLogCompositeFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << " fuchsia_hardware_audio/CompositeProperties";

  FX_LOGS(INFO) << "   manufacturer                 "
                << (composite_props.manufacturer().has_value()
                        ? ("'" +
                           std::string(composite_props.manufacturer()->data(),
                                       composite_props.manufacturer()->size()) +
                           "'")
                        : "<none>");

  FX_LOGS(INFO) << "   product                      "
                << (composite_props.product().has_value()
                        ? ("'" +
                           std::string(composite_props.product()->data(),
                                       composite_props.product()->size()) +
                           "'")
                        : "<none>");

  FX_LOGS(INFO) << "   unique_id                    " << UidToString(composite_props.unique_id());

  if (composite_props.clock_domain().has_value()) {
    FX_LOGS(INFO) << "   clock_domain                 " << *composite_props.clock_domain();
  } else {
    FX_LOGS(INFO) << "   clock_domain                 <none> (non-compliant)";
  }
}

void LogDynamicsBandStatesInternal(
    const std::optional<std::vector<fhasp::DynamicsBandState>>& states, const std::string& indent) {
  FX_LOGS(INFO) << indent << "type_specific (Dynamics)";
  if (!states.has_value()) {
    FX_LOGS(INFO) << indent << "                band_states           <none> (non-compliant)";
    return;
  }
  FX_LOGS(INFO) << indent << "                band_states [" << states->size() << "]"
                << (states->empty() ? " (non-compliant)" : "");
  for (auto i = 0u; i < states->size(); ++i) {
    const auto& bs = states->at(i);
    FX_LOGS(INFO) << indent << "                 [" << i << "]  id                "
                  << (bs.id().has_value() ? std::to_string(*bs.id()) : "<none> (non-compliant)");
    FX_LOGS(INFO) << indent << "                      min_frequency     "
                  << (bs.min_frequency().has_value() ? std::to_string(*bs.min_frequency())
                                                     : "<none> (non-compliant)");
    FX_LOGS(INFO) << indent << "                      max_frequency     "
                  << (bs.max_frequency().has_value() ? std::to_string(*bs.max_frequency())
                                                     : "<none> (non-compliant)");
    FX_LOGS(INFO) << indent << "                      threshold_db      "
                  << (bs.threshold_db().has_value() ? std::to_string(*bs.threshold_db())
                                                    : "<none> (non-compliant)");
    FX_LOGS(INFO) << indent << "                      threshold_type    " << bs.threshold_type();
    FX_LOGS(INFO) << indent << "                      ratio             "
                  << (bs.ratio().has_value() ? std::to_string(*bs.ratio())
                                             : "<none> (non-compliant)");
    FX_LOGS(INFO) << indent << "                      knee_width_db     "
                  << (bs.knee_width_db().has_value() ? std::to_string(*bs.knee_width_db())
                                                     : "<none>");
    FX_LOGS(INFO) << indent << "                      attack            "
                  << (bs.attack().has_value() ? std::to_string(*bs.attack()) + " nsec" : "<none>");
    FX_LOGS(INFO) << indent << "                      release           "
                  << (bs.release().has_value() ? std::to_string(*bs.release()) + " nsec"
                                               : "<none>");
    FX_LOGS(INFO) << indent << "                      output_gain_db    "
                  << (bs.output_gain_db().has_value() ? std::to_string(*bs.output_gain_db()) + " dB"
                                                      : "<none>");
    FX_LOGS(INFO) << indent << "                      input_gain_db     "
                  << (bs.input_gain_db().has_value() ? std::to_string(*bs.input_gain_db()) + " dB"
                                                     : "<none>");
    FX_LOGS(INFO) << indent << "                      level_type        " << bs.level_type();
    FX_LOGS(INFO) << indent << "                      lookahead         "
                  << (bs.lookahead().has_value() ? std::to_string(*bs.lookahead()) + " nsec"
                                                 : "<none>");
    FX_LOGS(INFO) << indent << "                      linked_channels   "
                  << (bs.linked_channels().has_value() ? (*bs.linked_channels() ? "TRUE" : "FALSE")
                                                       : "<none>");
  }
}

void LogEqualizerBandStatesInternal(
    const std::optional<std::vector<fhasp::EqualizerBandState>>& states,
    const std::string& indent) {
  FX_LOGS(INFO) << indent << "type_specific (Equalizer)";
  if (!states.has_value()) {
    FX_LOGS(INFO) << indent << "                band_states           <none> (non-compliant)";
    return;
  }
  FX_LOGS(INFO) << indent << "                band_states [" << states->size() << "]"
                << (states->empty() ? " (non-compliant)" : "");
  for (auto i = 0u; i < states->size(); ++i) {
    const auto& bs = states->at(i);
    FX_LOGS(INFO) << indent << "                 [" << i << "]  id                "
                  << (bs.id().has_value() ? std::to_string(*bs.id()) : "<none> (non-compliant)");
    FX_LOGS(INFO) << indent << "                      type              " << bs.type();
    FX_LOGS(INFO) << indent << "                      frequency         "
                  << (bs.frequency().has_value() ? std::to_string(*bs.frequency()) : "<none>");
    FX_LOGS(INFO) << indent << "                      gain_db           "
                  << (bs.gain_db().has_value() ? std::to_string(*bs.gain_db()) : "<none>");
    FX_LOGS(INFO) << indent << "                      enabled           "
                  << (bs.enabled().has_value() ? (*bs.enabled() ? "TRUE" : "FALSE") : "<none>");
  }
}

void LogGainDbInternal(const std::optional<float>& gain_db, const std::string& indent) {
  FX_LOGS(INFO) << indent << "type_specific (Gain)";
  if (!gain_db.has_value()) {
    FX_LOGS(INFO) << indent << "                      gain    <none> (non-compliant)";
  } else {
    FX_LOGS(INFO) << indent << "                      gain    " << *gain_db << " dB";
  }
}

void LogElementStateInternal(const std::optional<fhasp::ElementState>& element_state,
                             const std::string& indent) {
  std::string new_indent = indent + "                 ";
  if (!element_state.has_value()) {
    FX_LOGS(INFO) << new_indent << "     <none>  (during device initialization)";
    return;
  }

  if (element_state->type_specific().has_value()) {
    switch (element_state->type_specific()->Which()) {
      case fhasp::TypeSpecificElementState::Tag::kDaiInterconnect:
        if (!element_state->type_specific()->dai_interconnect().has_value()) {
          FX_LOGS(INFO) << indent << "type_specific (DaiInterconnect)    <none> (non-compliant)";
          break;
        }
        FX_LOGS(INFO) << indent << "type_specific (DaiInterconnect)";
        if (!element_state->type_specific()->dai_interconnect()->plug_state().has_value()) {
          FX_LOGS(INFO) << new_indent << "     PlugState  <none> (non-compliant)";
          break;
        }
        FX_LOGS(INFO)
            << new_indent << "     PlugState  plugged          "
            << (element_state->type_specific()
                        ->dai_interconnect()
                        ->plug_state()
                        ->plugged()
                        .has_value()
                    ? (element_state->type_specific()->dai_interconnect()->plug_state()->plugged()
                           ? "Plugged"
                           : "Unplugged")
                    : "<none> (non-compliant)");
        FX_LOGS(INFO) << new_indent << "                plug_state_time  "
                      << (element_state->type_specific()
                                  ->dai_interconnect()
                                  ->plug_state()
                                  ->plug_state_time()
                                  .has_value()
                              ? std::to_string(*element_state->type_specific()
                                                    ->dai_interconnect()
                                                    ->plug_state()
                                                    ->plug_state_time()) +
                                    " nsec"
                              : "<none> (non-compliant)");
        FX_LOGS(INFO)
            << new_indent << "     external_delay              "
            << (element_state->type_specific()->dai_interconnect()->external_delay().has_value()
                    ? std::to_string(
                          *element_state->type_specific()->dai_interconnect()->external_delay()) +
                          " nsec"
                    : "<none>");
        break;
      case fhasp::TypeSpecificElementState::Tag::kDynamics:
        if (!element_state->type_specific()->dynamics().has_value()) {
          FX_LOGS(INFO) << indent << "type_specific (Dynamics)              <none> (non-compliant)";
          break;
        }
        LogDynamicsBandStatesInternal(element_state->type_specific()->dynamics()->band_states(),
                                      indent);
        break;
      case fhasp::TypeSpecificElementState::Tag::kEqualizer:
        if (!element_state->type_specific()->equalizer().has_value()) {
          FX_LOGS(INFO) << indent << "type_specific (Equalizer)             <none> (non-compliant)";
          break;
        }
        LogEqualizerBandStatesInternal(element_state->type_specific()->equalizer()->band_states(),
                                       indent);
        break;
      case fhasp::TypeSpecificElementState::Tag::kGain:
        if (!element_state->type_specific()->gain().has_value()) {
          FX_LOGS(INFO) << indent << "type_specific (Gain)        <none> (non-compliant)";
          break;
        }
        LogGainDbInternal(element_state->type_specific()->gain()->gain(), indent);
        break;
      case fhasp::TypeSpecificElementState::Tag::kVendorSpecific:
        FX_LOGS(INFO) << indent << "type_specific (VendorSpecific)";
        break;
      default:
        FX_LOGS(INFO) << indent << "type_specific <unknown union>  (non-compliant)";
        break;
    }
  } else {
    FX_LOGS(INFO) << indent << "type_specific <none> (non-compliant)";
  }

  if (element_state->vendor_specific_data().has_value()) {
    FX_LOGS(INFO) << indent << "vendor_specific_data  ["
                  << element_state->vendor_specific_data()->size() << "]  (not shown here)"
                  << (element_state->vendor_specific_data()->empty() ? " (non-compliant)" : "");
  } else {
    FX_LOGS(INFO) << indent << "vendor_specific_data    <none>";
  }

  FX_LOGS(INFO) << indent << "started                 "
                << (element_state->started().has_value()
                        ? (*element_state->started() ? "TRUE" : "FALSE")
                        : "<none> (non-compliant)");

  FX_LOGS(INFO) << indent << "bypassed                "
                << (element_state->bypassed().has_value()
                        ? (*element_state->bypassed() ? "TRUE" : "FALSE")
                        : "<none>");

  FX_LOGS(INFO) << indent << "turn_on_delay  (ns)     "
                << (element_state->turn_on_delay().has_value()
                        ? std::to_string(*element_state->turn_on_delay())
                        : "<none>");

  FX_LOGS(INFO) << indent << "turn_off_delay (ns)     "
                << (element_state->turn_off_delay().has_value()
                        ? std::to_string(*element_state->turn_off_delay())
                        : "<none>");

  FX_LOGS(INFO) << indent << "processing_delay (ns)   "
                << (element_state->processing_delay().has_value()
                        ? std::to_string(*element_state->processing_delay())
                        : "<none>");
}

void LogSettableElementStateInternal(
    const std::optional<fhasp::SettableElementState>& element_state, const std::string& indent) {
  std::string new_indent = indent + "                 ";
  if (!element_state.has_value()) {
    FX_LOGS(INFO) << new_indent << "     <none>  (during device initialization)";
    return;
  }

  if (element_state->type_specific().has_value()) {
    switch (element_state->type_specific()->Which()) {
      case fhasp::SettableTypeSpecificElementState::Tag::kDynamics:
        if (!element_state->type_specific()->dynamics().has_value()) {
          FX_LOGS(INFO) << indent << "type_specific (Dynamics)              <none> (non-compliant)";
          break;
        }
        LogDynamicsBandStatesInternal(element_state->type_specific()->dynamics()->band_states(),
                                      indent);
        break;
      case fhasp::SettableTypeSpecificElementState::Tag::kEqualizer:
        if (!element_state->type_specific()->equalizer().has_value()) {
          FX_LOGS(INFO) << indent << "type_specific (Equalizer)             <none> (non-compliant)";
          break;
        }
        LogEqualizerBandStatesInternal(element_state->type_specific()->equalizer()->band_states(),
                                       indent);
        break;
      case fhasp::SettableTypeSpecificElementState::Tag::kGain:
        if (!element_state->type_specific()->gain().has_value()) {
          FX_LOGS(INFO) << indent << "type_specific (Gain)        <none> (non-compliant)";
          break;
        }
        LogGainDbInternal(element_state->type_specific()->gain()->gain(), indent);
        break;
      case fhasp::SettableTypeSpecificElementState::Tag::kVendorSpecific:
        FX_LOGS(INFO) << indent << "type_specific (VendorSpecific)";
        break;
      default:
        FX_LOGS(INFO) << indent << "type_specific <unknown union>  (non-compliant)";
        break;
    }
  } else {
    FX_LOGS(INFO) << indent << "type_specific <none> (non-compliant)";
  }

  if (element_state->vendor_specific_data().has_value()) {
    FX_LOGS(INFO) << indent << "vendor_specific_data  ["
                  << element_state->vendor_specific_data()->size() << "]  (not shown here)"
                  << (element_state->vendor_specific_data()->empty() ? " (non-compliant)" : "");
  } else {
    FX_LOGS(INFO) << indent << "vendor_specific_data    <none>";
  }

  FX_LOGS(INFO) << indent << "started                 "
                << (element_state->started().has_value()
                        ? (*element_state->started() ? "TRUE" : "FALSE")
                        : "<none> (non-compliant)");

  FX_LOGS(INFO) << indent << "bypassed                "
                << (element_state->bypassed().has_value()
                        ? (*element_state->bypassed() ? "TRUE" : "FALSE")
                        : "<none>");
}

void LogSettableElementState(const std::optional<fhasp::SettableElementState>& element_state) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/SettableElementState";
  LogSettableElementStateInternal(element_state, "    ");
}

void LogElementState(const std::optional<fhasp::ElementState>& element_state) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/ElementState";
  LogElementStateInternal(element_state, "    ");
}

void LogElementInternal(const fhasp::Element& element, std::string indent,
                        std::optional<size_t> index = std::nullopt,
                        std::string_view addl_indent = "") {
  std::string first_indent{indent};
  if (index.has_value()) {
    first_indent.append(" [")
        .append(std::to_string(*index))
        .append("]")
        .append(addl_indent.substr(3));
  } else {
    first_indent.append(addl_indent);
  }
  indent.append(addl_indent);

  FX_LOGS(INFO) << first_indent << "id              "
                << (element.id().has_value() ? std::to_string(*element.id())
                                             : "<none> (non-compliant)");

  FX_LOGS(INFO) << indent << " type            " << element.type();

  std::string ts_indent = indent + "                       ";
  if (element.type_specific().has_value()) {
    switch (element.type_specific()->Which()) {
      case fhasp::TypeSpecificElement::Tag::kDaiInterconnect:
        FX_LOGS(INFO) << indent << " type_specific   "
                      << element.type_specific()->dai_interconnect().value();
        break;
      case fhasp::TypeSpecificElement::Tag::kDynamics:
        FX_LOGS(INFO) << indent << " type_specific   DYNAMICS";
        if (element.type_specific()->dynamics().has_value()) {
          if (element.type_specific()->dynamics()->bands().has_value()) {
            FX_LOGS(INFO) << ts_indent << "bands ["
                          << element.type_specific()->dynamics()->bands()->size() << "]";
            for (auto i = 0u; i < element.type_specific()->dynamics()->bands()->size(); ++i) {
              FX_LOGS(INFO) << ts_indent << " [" << i << "]          id    "
                            << (element.type_specific()->dynamics()->bands()->at(i).id().has_value()
                                    ? std::to_string(
                                          *element.type_specific()->dynamics()->bands()->at(i).id())
                                    : "<none> (non-compliant)");
            }
          } else {
            FX_LOGS(INFO) << ts_indent << "bands               <none> (non-compliant)";
            break;
          }
        } else {
          FX_LOGS(INFO) << ts_indent << "<none> (non-compliant)";
        }
        FX_LOGS(INFO) << ts_indent << "supported_controls  ";
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kKneeWidth) {
          FX_LOGS(INFO) << ts_indent << "                    KNEE_WIDTH";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kAttack) {
          FX_LOGS(INFO) << ts_indent << "                    ATTACK";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kRelease) {
          FX_LOGS(INFO) << ts_indent << "                    RELEASE";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kOutputGain) {
          FX_LOGS(INFO) << ts_indent << "                    OUTPUT_GAIN";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kInputGain) {
          FX_LOGS(INFO) << ts_indent << "                    INPUT_GAIN";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kLookahead) {
          FX_LOGS(INFO) << ts_indent << "                    LOOKAHEAD";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kLevelType) {
          FX_LOGS(INFO) << ts_indent << "                    LEVEL_TYPE";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kLinkedChannels) {
          FX_LOGS(INFO) << ts_indent << "                    LINKED_CHANNELS";
        }
        if (*element.type_specific()->dynamics()->supported_controls() &
            fhasp::DynamicsSupportedControls::kThresholdType) {
          FX_LOGS(INFO) << ts_indent << "                    THRESHOLD_TYPE";
        }
        //
        break;
      case fhasp::TypeSpecificElement::Tag::kEqualizer:
        FX_LOGS(INFO) << indent << " type_specific   EQUALIZER";
        if (element.type_specific()->equalizer().has_value()) {
          if (element.type_specific()->equalizer()->bands().has_value()) {
            FX_LOGS(INFO) << ts_indent << "bands ["
                          << element.type_specific()->equalizer()->bands()->size() << "]";
            for (auto i = 0u; i < element.type_specific()->equalizer()->bands()->size(); ++i) {
              FX_LOGS(INFO)
                  << ts_indent << " [" << i << "]          id    "
                  << (element.type_specific()->equalizer()->bands()->at(i).id().has_value()
                          ? std::to_string(
                                *element.type_specific()->equalizer()->bands()->at(i).id())
                          : "<none> (non-compliant)");
            }
          } else {
            FX_LOGS(INFO) << indent
                          << "                       bands               <none> (non-compliant)";
            break;
          }
        } else {
          FX_LOGS(INFO) << ts_indent << "<none> (non-compliant)";
        }
        FX_LOGS(INFO) << ts_indent << "supported_controls  ";
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kCanControlFrequency) {
          FX_LOGS(INFO) << ts_indent << "                    CAN_CONTROL_FREQUENCY";
        }
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kCanControlQ) {
          FX_LOGS(INFO) << ts_indent << "                    CAN_CONTROL_Q";
        }
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kSupportsTypePeak) {
          FX_LOGS(INFO) << ts_indent << "                    SUPPORTS_TYPE_PEAK";
        }
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kSupportsTypeNotch) {
          FX_LOGS(INFO) << ts_indent << "                    SUPPORTS_TYPE_NOTCH";
        }
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kSupportsTypeLowCut) {
          FX_LOGS(INFO) << ts_indent << "                    SUPPORTS_TYPE_LOW_CUT";
        }
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kSupportsTypeHighCut) {
          FX_LOGS(INFO) << ts_indent << "                    SUPPORTS_TYPE_HIGH_CUT";
        }
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kSupportsTypeLowShelf) {
          FX_LOGS(INFO) << ts_indent << "                    SUPPORTS_TYPE_LOW_SHELF";
        }
        if (*element.type_specific()->equalizer()->supported_controls() &
            fhasp::EqualizerSupportedControls::kSupportsTypeHighShelf) {
          FX_LOGS(INFO) << ts_indent << "                    SUPPORTS_TYPE_HIGH_SHELF";
        }

        FX_LOGS(INFO) << ts_indent << "can_disable_bands   "
                      << (element.type_specific()->equalizer()->can_disable_bands().has_value()
                              ? (*element.type_specific()->equalizer()->can_disable_bands()
                                     ? "TRUE"
                                     : "FALSE")
                              : "<none>");
        FX_LOGS(INFO) << ts_indent << "min_frequency       "
                      << (element.type_specific()->equalizer()->min_frequency().has_value()
                              ? std::to_string(
                                    *element.type_specific()->equalizer()->min_frequency())
                              : "<none> (non-compliant)");
        FX_LOGS(INFO) << ts_indent << "max_frequency       "
                      << (element.type_specific()->equalizer()->max_frequency().has_value()
                              ? std::to_string(
                                    *element.type_specific()->equalizer()->max_frequency())
                              : "<none> (non-compliant)");
        FX_LOGS(INFO) << ts_indent << "max_q               "
                      << (element.type_specific()->equalizer()->max_q().has_value()
                              ? std::to_string(*element.type_specific()->equalizer()->max_q())
                              : "<none>");
        FX_LOGS(INFO) << ts_indent << "min_gain_db         "
                      << (element.type_specific()->equalizer()->min_gain_db().has_value()
                              ? std::to_string(*element.type_specific()->equalizer()->min_gain_db())
                              : "<none>");
        FX_LOGS(INFO) << ts_indent << "max_gain_db         "
                      << (element.type_specific()->equalizer()->max_gain_db().has_value()
                              ? std::to_string(*element.type_specific()->equalizer()->max_gain_db())
                              : "<none>");
        break;
      case fhasp::TypeSpecificElement::Tag::kGain:
        FX_LOGS(INFO) << indent << " type_specific   GAIN";
        FX_LOGS(INFO) << ts_indent << "type                "
                      << element.type_specific()->gain()->type();
        FX_LOGS(INFO) << ts_indent << "domain              "
                      << element.type_specific()->gain()->domain();
        FX_LOGS(INFO) << ts_indent << "min_gain            "
                      << (element.type_specific()->gain()->min_gain().has_value()
                              ? std::to_string(*element.type_specific()->gain()->min_gain()) + " dB"
                              : "<none>");
        FX_LOGS(INFO) << ts_indent << "max_gain            "
                      << (element.type_specific()->gain()->max_gain().has_value()
                              ? std::to_string(*element.type_specific()->gain()->max_gain()) + " dB"
                              : "<none>");
        FX_LOGS(INFO) << ts_indent << "min_gain_step       "
                      << (element.type_specific()->gain()->min_gain_step().has_value()
                              ? std::to_string(*element.type_specific()->gain()->min_gain_step()) +
                                    " dB"
                              : "<none>");
        break;
      case fhasp::TypeSpecificElement::Tag::kVendorSpecific:
        FX_LOGS(INFO) << indent << "type_specific   VENDOR_SPECIFIC";
        break;
      default:
        FX_LOGS(INFO) << indent << "type_specific   OTHER - unknown enum";
        break;
    }
  } else {
    FX_LOGS(INFO) << indent << "type_specific   <none>";
  }

  FX_LOGS(INFO) << indent << "description     "
                << (element.description().has_value()
                        ? std::string("'") + *element.description() + "'"
                        : "<none>");

  FX_LOGS(INFO) << indent << "can_stop        "
                << (element.can_stop().has_value() ? (*element.can_stop() ? "TRUE" : "FALSE")
                                                   : "<none> (FALSE)");

  FX_LOGS(INFO) << indent << "can_bypass      "
                << (element.can_bypass().has_value() ? (*element.can_bypass() ? "TRUE" : "FALSE")
                                                     : "<none> (FALSE)");
}

void LogElement(const fhasp::Element& element) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Element";
  LogElementInternal(element, "   ");
}

void LogElements(const std::vector<fhasp::Element>& elements) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Elements";
  FX_LOGS(INFO) << "  elements [" << elements.size() << "]";

  for (auto i = 0u; i < elements.size(); ++i) {
    LogElementInternal(elements[i], "   ", i, "    ");
  }
}

void LogElementMap(const std::unordered_map<ElementId, ElementRecord>& element_map) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "ElementMap <ElementId, ElementRecord>";
  for (auto& [element_id, element_record] : element_map) {
    FX_LOGS(INFO) << "ElementId     " << element_id;
    FX_LOGS(INFO) << "ElementRecord";

    FX_LOGS(INFO) << "   element";
    LogElementInternal(element_record.element, "        ");

    FX_LOGS(INFO) << "   state";
    LogElementStateInternal(element_record.state, "        ");
  }
}

void LogTopologyInternal(const fhasp::Topology& topology, std::string indent,
                         std::optional<size_t> index = std::nullopt,
                         std::string_view addl_indent = "") {
  std::string first_indent{indent};
  if (index.has_value()) {
    first_indent.append("[")
        .append(std::to_string(*index))
        .append("]")
        .append(addl_indent.substr(3));
  } else {
    first_indent.append(addl_indent);
  }
  indent.append(addl_indent);

  FX_LOGS(INFO) << first_indent << "id              "
                << (topology.id().has_value() ? std::to_string(*topology.id())
                                              : "<none> (non-compliant)");
  if (topology.processing_elements_edge_pairs().has_value()) {
    FX_LOGS(INFO) << indent << "processing_elements_edge_pairs ["
                  << topology.processing_elements_edge_pairs()->size() << "]";
    for (auto idx = 0u; idx < topology.processing_elements_edge_pairs()->size(); ++idx) {
      FX_LOGS(INFO)
          << indent << " [" << idx << "]            "
          << topology.processing_elements_edge_pairs()->at(idx).processing_element_id_from()
          << " -> "
          << topology.processing_elements_edge_pairs()->at(idx).processing_element_id_to();
    }
  } else {
    FX_LOGS(INFO) << indent << "processing_elements_edge_pairs <none> (non-compliant)";
  }
}

void LogTopology(const fhasp::Topology& topology) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Topology";
  LogTopologyInternal(topology, "       ");
}

void LogTopologies(const std::vector<fhasp::Topology>& topologies) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Topologies";
  FX_LOGS(INFO) << "  topologies [" << topologies.size() << "]";

  for (auto idx = 0u; idx < topologies.size(); ++idx) {
    LogTopologyInternal(topologies[idx], "   ", idx, "    ");
  }
}

// Signal the successful detection, querying, initialization and addition of a device.
void LogDeviceAddition(const fad::Info& device_info) {
  if constexpr (kLogDeviceAddErrorRemove) {
    FX_DCHECK(device_info.device_type().has_value());
    FX_LOGS(INFO) << device_info.device_type() << " device "
                  << (device_info.device_name().has_value()
                          ? std::string("'") + *device_info.device_name() + "'"
                          : "<none>")
                  << " with token_id "
                  << (device_info.token_id().has_value() ? std::to_string(*device_info.token_id())
                                                         : "<none> (non-compliant)")
                  << " has been added";
  }
}

// Mirror (bookend) the analogous `LogDeviceAddition` for device removal. Removals may be "normal"
// (USB unplug) or caused by fatal error. The latter can happen before `device_info` is created.
void LogDeviceRemoval(const std::optional<fad::Info>& device_info) {
  if constexpr (kLogDeviceAddErrorRemove) {
    if (device_info.has_value()) {
      FX_DCHECK(device_info->device_type().has_value());
      FX_LOGS(INFO) << device_info->device_type() << " device "
                    << (device_info->device_name().has_value()
                            ? std::string("'") + *device_info->device_name() + "'"
                            : "<none>")
                    << " with token_id "
                    << (device_info->token_id().has_value()
                            ? std::to_string(*device_info->token_id())
                            : "<none> (non-compliant)")
                    << " has been removed";
    } else {
      FX_LOGS(WARNING) << "UNKNOWN device has been removed before initialization could complete";
    }
  }
}

// Mirror (bookend) the analogous `LogDeviceAddition`, for a device error.
// This can also occur before a device has been successfully added: device_info may not be set.
void LogDeviceError(const std::optional<fad::Info>& device_info) {
  if constexpr (kLogDeviceAddErrorRemove) {
    if (device_info.has_value()) {
      FX_DCHECK(device_info->device_type().has_value());
      FX_LOGS(WARNING) << device_info->device_type() << " device "
                       << (device_info->device_name().has_value()
                               ? std::string("'") + *device_info->device_name() + "'"
                               : "<none>")
                       << " with token_id "
                       << (device_info->token_id().has_value()
                               ? std::to_string(*device_info->token_id())
                               : "<none> (non-compliant)")
                       << " has encountered a fatal error";
    } else {
      FX_LOGS(WARNING) << "UNKNOWN device has encountered a fatal error during initialization";
    }
  }
}

void LogDeviceInfo(const fad::Info& device_info) {
  if constexpr (!kLogDeviceInfo) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_audio_device/Info";

  FX_LOGS(INFO) << "  token_id                     "
                << (device_info.token_id().has_value() ? std::to_string(*device_info.token_id())
                                                       : "<none> (non-compliant)");

  FX_LOGS(INFO) << "  device_type                  " << device_info.device_type();

  FX_LOGS(INFO) << "  device_name                  "
                << (device_info.device_name().has_value()
                        ? std::string("'") + *device_info.device_name() + "'"
                        : "<none>");

  FX_LOGS(INFO) << "  manufacturer                 "
                << (device_info.manufacturer().has_value() ? "'" + *device_info.manufacturer() + "'"
                                                           : "<none>");

  FX_LOGS(INFO) << "  product                      "
                << (device_info.product().has_value() ? "'" + *device_info.product() + "'"
                                                      : "<none>");

  FX_LOGS(INFO) << "  unique_instance_id           "
                << UidToString(device_info.unique_instance_id());

  FX_LOGS(INFO) << "  is_input                     "
                << (device_info.is_input().has_value()
                        ? (*device_info.is_input() ? "TRUE" : "FALSE")
                        : "<none>");

  if (device_info.ring_buffer_format_sets().has_value()) {
    FX_LOGS(INFO) << "  ring_buffer_format_sets [" << device_info.ring_buffer_format_sets()->size()
                  << "]";
    for (auto i = 0u; i < device_info.ring_buffer_format_sets()->size(); ++i) {
      const auto& element_ring_buffer_format_set = device_info.ring_buffer_format_sets()->at(i);
      FX_LOGS(INFO) << "   [" << i << "] element_id              "
                    << (element_ring_buffer_format_set.element_id().has_value()
                            ? std::to_string(*element_ring_buffer_format_set.element_id())
                            : "<none> (non-compliant)");
      if (element_ring_buffer_format_set.format_sets().has_value()) {
        FX_LOGS(INFO) << "       format_set ["
                      << element_ring_buffer_format_set.format_sets()->size() << "]";
        for (auto j = 0u; j < element_ring_buffer_format_set.format_sets()->size(); ++j) {
          const auto& pcm_format_set = element_ring_buffer_format_set.format_sets()->at(j);
          if (pcm_format_set.channel_sets().has_value()) {
            FX_LOGS(INFO) << "        [" << j << "] channel_sets ["
                          << pcm_format_set.channel_sets()->size() << "]";
            for (auto k = 0u; k < pcm_format_set.channel_sets()->size(); ++k) {
              const auto& channel_set = pcm_format_set.channel_sets()->at(k);
              if (channel_set.attributes().has_value()) {
                FX_LOGS(INFO) << "             [" << k << "] attributes ["
                              << channel_set.attributes()->size() << "]";
                for (auto idx = 0u; idx < channel_set.attributes()->size(); ++idx) {
                  const auto& attributes = channel_set.attributes()->at(idx);
                  if (attributes.min_frequency().has_value()) {
                    FX_LOGS(INFO) << "                  [" << idx << "] min_freq "
                                  << *attributes.min_frequency();
                  } else {
                    FX_LOGS(INFO) << "                  [" << idx << "] min_freq <none>";
                  }
                  if (attributes.max_frequency().has_value()) {
                    FX_LOGS(INFO) << "                      max_freq "
                                  << *attributes.max_frequency();
                  } else {
                    FX_LOGS(INFO) << "                      max_freq <none>";
                  }
                }
              } else {
                FX_LOGS(INFO) << "              [" << k
                              << "] attributes     <none>  (non-compliant)";
              }
            }
          } else {
            FX_LOGS(INFO) << "        [" << j << "] channel_sets       <none> (non-compliant)";
          }

          if (pcm_format_set.sample_types().has_value()) {
            FX_LOGS(INFO) << "            sample_types [" << pcm_format_set.sample_types()->size()
                          << "]";
            for (auto idx = 0u; idx < pcm_format_set.sample_types()->size(); ++idx) {
              FX_LOGS(INFO) << "             [" << idx << "]               "
                            << pcm_format_set.sample_types()->at(idx);
            }
          } else {
            FX_LOGS(INFO) << "            sample_types       <none> (non-compliant)";
          }
          if (pcm_format_set.frame_rates().has_value()) {
            FX_LOGS(INFO) << "            frame_rates [" << pcm_format_set.frame_rates()->size()
                          << "]";
            for (auto idx = 0u; idx < pcm_format_set.frame_rates()->size(); ++idx) {
              FX_LOGS(INFO) << "             [" << idx << "]               "
                            << pcm_format_set.frame_rates()->at(idx);
            }
          } else {
            FX_LOGS(INFO) << "            frame_rates        <none> (non-compliant)";
          }
        }
      } else {
        FX_LOGS(INFO) << "       format_set              <none> (non-compliant)";
      }
    }
  } else {
    FX_LOGS(INFO) << "  ring_buffer_format_sets      <none>";
  }

  // dai_format_sets
  if (device_info.dai_format_sets().has_value()) {
    FX_LOGS(INFO) << "  dai_format_sets [" << device_info.dai_format_sets()->size() << "]";
    for (auto i = 0u; i < device_info.dai_format_sets()->size(); ++i) {
      auto element_dai_format_set = device_info.dai_format_sets()->at(i);
      FX_LOGS(INFO) << "   [" << i << "]  element_id             "
                    << (element_dai_format_set.format_sets().has_value()
                            ? std::to_string(*element_dai_format_set.element_id())
                            : "<none> (non-compliant)");
      if (element_dai_format_set.format_sets().has_value()) {
        FX_LOGS(INFO) << "        format_set [" << element_dai_format_set.format_sets()->size()
                      << "]";
        for (auto j = 0u; j < element_dai_format_set.format_sets()->size(); ++j) {
          const auto& dai_format_set = element_dai_format_set.format_sets()->at(j);
          const auto& channel_counts = dai_format_set.number_of_channels();
          FX_LOGS(INFO) << "         [" << j << "] number_of_channels [" << channel_counts.size()
                        << "]";
          for (auto idx = 0u; idx < channel_counts.size(); ++idx) {
            FX_LOGS(INFO) << "              [" << idx << "]              " << channel_counts[idx];
          }

          const auto& sample_formats = dai_format_set.sample_formats();
          FX_LOGS(INFO) << "             sample_formats [" << sample_formats.size() << "]";
          for (auto idx = 0u; idx < sample_formats.size(); ++idx) {
            FX_LOGS(INFO) << "              [" << idx << "]              " << sample_formats[idx];
          }

          const auto& frame_formats = dai_format_set.frame_formats();
          FX_LOGS(INFO) << "             frame_formats [" << frame_formats.size() << "]";
          for (auto idx = 0u; idx < frame_formats.size(); ++idx) {
            FX_LOGS(INFO) << "              [" << idx << "]              " << frame_formats[idx];
          }

          const auto& frame_rates = dai_format_set.frame_rates();
          FX_LOGS(INFO) << "             frame_rates [" << frame_rates.size() << "]";
          for (auto idx = 0u; idx < frame_rates.size(); ++idx) {
            FX_LOGS(INFO) << "              [" << idx << "]              " << frame_rates[idx];
          }

          const auto& bits_per_slot = dai_format_set.bits_per_slot();
          FX_LOGS(INFO) << "             bits_per_slot [" << bits_per_slot.size() << "]";
          for (auto idx = 0u; idx < bits_per_slot.size(); ++idx) {
            FX_LOGS(INFO) << "              [" << idx << "]              "
                          << static_cast<int16_t>(bits_per_slot[idx]);
          }

          const auto& bits_per_sample = dai_format_set.bits_per_sample();
          FX_LOGS(INFO) << "             bits_per_sample [" << bits_per_sample.size() << "]";
          for (auto idx = 0u; idx < bits_per_sample.size(); ++idx) {
            FX_LOGS(INFO) << "              [" << idx << "]              "
                          << static_cast<int16_t>(bits_per_sample[idx]);
          }
        }
      } else {
        FX_LOGS(INFO) << "        format_set <none> (non-compliant)";
      }
    }
  } else {
    FX_LOGS(INFO) << "  dai_format_sets              <none>"
                  << ((device_info.device_type() == fad::DeviceType::kCodec) ? " (non-compliant)"
                                                                             : "");
  }

  FX_LOGS(INFO) << "  plug_detect_caps             " << device_info.plug_detect_caps();

  std::string clock_domain_str{"  clock_domain                 "};
  if (device_info.clock_domain().has_value()) {
    clock_domain_str += std::to_string(*device_info.clock_domain());
    if (*device_info.clock_domain() == fha::kClockDomainMonotonic) {
      clock_domain_str += "  (CLOCK_DOMAIN_MONOTONIC)";
    } else if (*device_info.clock_domain() == fha::kClockDomainExternal) {
      clock_domain_str += "  (CLOCK_DOMAIN_EXTERNAL)";
    }
  } else {
    clock_domain_str += "<none>";
    if (device_info.device_type() == fad::DeviceType::kComposite) {
      clock_domain_str += " (non-compliant)";
    }
  }
  FX_LOGS(INFO) << clock_domain_str;

  if (device_info.signal_processing_elements().has_value()) {
    FX_LOGS(INFO) << "  signal_processing_elements ["
                  << device_info.signal_processing_elements()->size() << "]"
                  << (device_info.signal_processing_elements()->empty() ? " (non-compliant)" : "");
    for (auto idx = 0u; idx < device_info.signal_processing_elements()->size(); ++idx) {
      LogElementInternal(device_info.signal_processing_elements()->at(idx), "           ", idx,
                         "    ");
    }
  } else {
    FX_LOGS(INFO) << "  signal_processing_elements   <none>"
                  << (device_info.device_type() == fad::DeviceType::kComposite ? " (non-compliant)"
                                                                               : "");
  }

  if (device_info.signal_processing_topologies().has_value()) {
    FX_LOGS(INFO) << "  signal_processing_topologies ["
                  << device_info.signal_processing_topologies()->size() << "]"
                  << (device_info.signal_processing_topologies()->empty() ? " (non-compliant)"
                                                                          : "");
    for (auto idx = 0u; idx < device_info.signal_processing_topologies()->size(); ++idx) {
      LogTopologyInternal(device_info.signal_processing_topologies()->at(idx), "           ", idx,
                          "    ");
    }
  } else {
    FX_LOGS(INFO) << "  signal_processing_topologies <none>"
                  << (device_info.device_type() == fad::DeviceType::kComposite ? " (non-compliant)"
                                                                               : "");
  }
}

void LogRingBufferProperties(const fha::RingBufferProperties& rb_props) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/RingBufferProperties";
  FX_LOGS(INFO) << "    needs_cache_flush       "
                << (rb_props.needs_cache_flush_or_invalidate().has_value()
                        ? (*rb_props.needs_cache_flush_or_invalidate() ? "TRUE" : "FALSE")
                        : "<none> (non-compliant)");

  if (rb_props.turn_on_delay().has_value()) {
    FX_LOGS(INFO) << "    turn_on_delay           " << *rb_props.turn_on_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    turn_on_delay           <none> (0 ns)";
  }

  if (rb_props.driver_transfer_bytes().has_value()) {
    FX_LOGS(INFO) << "    driver_transfer_bytes   " << *rb_props.driver_transfer_bytes()
                  << " bytes";
  } else {
    FX_LOGS(INFO) << "    driver_transfer_bytes   <none> (non-compliant)";
  }
}

void LogRingBufferFormat(const fha::Format& ring_buffer_format) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/Format";
  if (!ring_buffer_format.pcm_format().has_value()) {
    FX_LOGS(INFO) << "    pcm_format           <none> (non-compliant)";
    return;
  }

  FX_LOGS(INFO) << "    pcm_format";
  FX_LOGS(INFO) << "        number_of_channels    "
                << static_cast<uint16_t>(ring_buffer_format.pcm_format()->number_of_channels());
  FX_LOGS(INFO) << "        sample_format         "
                << ring_buffer_format.pcm_format()->sample_format();
  FX_LOGS(INFO) << "        bytes_per_sample      "
                << static_cast<uint16_t>(ring_buffer_format.pcm_format()->bytes_per_sample());
  FX_LOGS(INFO) << "        valid_bits_per_sample "
                << static_cast<uint16_t>(ring_buffer_format.pcm_format()->valid_bits_per_sample());
  FX_LOGS(INFO) << "        frame_rate            "
                << ring_buffer_format.pcm_format()->frame_rate();
}

void LogRingBufferVmo(const zx::vmo& vmo, uint32_t num_frames, fha::Format rb_format) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  zx_info_handle_basic_t info;
  auto status = vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "vmo.get_info returned error";
    return;
  }
  uint64_t size;
  status = vmo.get_size(&size);
  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "vmo.get_size returned size " << size;
    return;
  }
  FX_LOGS(INFO) << "fuchsia_hardware_audio/GetVmo -> zx::Vmo";
  FX_LOGS(INFO) << "    koid                 0x" << std::hex << info.koid;
  FX_LOGS(INFO) << "    rights               0x" << std::hex << info.rights << " ("
                << ((info.rights & ZX_RIGHT_DUPLICATE) ? " DUPLICATE" : "")
                << ((info.rights & ZX_RIGHT_TRANSFER) ? " TRANSFER" : "")
                << ((info.rights & ZX_RIGHT_READ) ? " READ" : "")
                << ((info.rights & ZX_RIGHT_WRITE) ? " WRITE" : "")
                << ((info.rights & ZX_RIGHT_MAP) ? " MAP" : "")
                << ((info.rights & ZX_RIGHT_GET_PROPERTY) ? " GET_PROPERTY" : "")
                << ((info.rights & ZX_RIGHT_SET_PROPERTY) ? " SET_PROPERTY" : "")
                << ((info.rights & ZX_RIGHT_RESIZE) ? " RESIZE" : "") << " )";
  FX_LOGS(INFO) << "    size                 " << size << " bytes";
  FX_LOGS(INFO) << "    calculated_size      "
                << num_frames * rb_format.pcm_format()->number_of_channels() *
                       rb_format.pcm_format()->bytes_per_sample();
  FX_LOGS(INFO) << "        num_frames           " << num_frames;
  FX_LOGS(INFO) << "        num_channels         "
                << static_cast<uint16_t>(rb_format.pcm_format()->number_of_channels());
  FX_LOGS(INFO) << "        bytes_per_sample     "
                << static_cast<uint16_t>(rb_format.pcm_format()->bytes_per_sample());
}

void LogActiveChannels(uint64_t channel_bitmask, zx::time set_time) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/SetActiveChannels";
  FX_LOGS(INFO) << "    channel_bitmask      0x" << std::setfill('0') << std::setw(2) << std::hex
                << channel_bitmask;
  FX_LOGS(INFO) << "    set_time             " << set_time.get();
}

void LogDelayInfo(const fha::DelayInfo& info) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/DelayInfo";
  if (info.internal_delay().has_value()) {
    FX_LOGS(INFO) << "    internal_delay           " << *info.internal_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    internal_delay           <none> (non-compliant)";
  }

  if (info.external_delay().has_value()) {
    FX_LOGS(INFO) << "    external_delay           " << *info.external_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    external_delay           <none> (0 ns)";
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
