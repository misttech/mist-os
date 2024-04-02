// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/logging.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <iomanip>
#include <string>

#include "fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h"
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

void LogStreamProperties(const fuchsia_hardware_audio::StreamProperties& stream_props) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/StreamProperties";

  FX_LOGS(INFO) << "    unique_id         " << UidToString(stream_props.unique_id());
  FX_LOGS(INFO) << "    is_input          "
                << (stream_props.is_input() ? (*stream_props.is_input() ? "TRUE" : "FALSE")
                                            : "<none> (non-compliant)");
  FX_LOGS(INFO) << "    can_mute          "
                << (stream_props.can_mute() ? (*stream_props.can_mute() ? "TRUE" : "FALSE")
                                            : "<none> (cannot mute)");
  FX_LOGS(INFO) << "    can_agc           "
                << (stream_props.can_agc() ? (*stream_props.can_agc() ? "TRUE" : "FALSE")
                                           : "<none> (cannot enable AGC)");
  if (stream_props.min_gain_db()) {
    FX_LOGS(INFO) << "    min_gain_db       " << *stream_props.min_gain_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    min_gain_db       <none> (non-compliant)";
  }
  if (stream_props.max_gain_db()) {
    FX_LOGS(INFO) << "    max_gain_db       " << *stream_props.max_gain_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    max_gain_db       <none> (non-compliant)";
  }
  if (stream_props.gain_step_db()) {
    FX_LOGS(INFO) << "    gain_step_db      " << *stream_props.gain_step_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    gain_step_db      <none> (non-compliant)";
  }
  if (stream_props.plug_detect_capabilities()) {
    FX_LOGS(INFO) << "    plug_detect_caps  " << *stream_props.plug_detect_capabilities();
  } else {
    FX_LOGS(INFO) << "    plug_detect_caps  <none> (non-compliant)";
  }
  FX_LOGS(INFO) << "    manufacturer      "
                << (stream_props.manufacturer()
                        ? ("'" +
                           std::string(stream_props.manufacturer()->data(),
                                       stream_props.manufacturer()->size()) +
                           "'")
                        : "<none>");
  FX_LOGS(INFO) << "    product           "
                << (stream_props.product() ? ("'" +
                                              std::string(stream_props.product()->data(),
                                                          stream_props.product()->size()) +
                                              "'")
                                           : "<none>");

  std::string clock_domain_str{"   clock _domain     "};
  if (stream_props.clock_domain()) {
    clock_domain_str += std::to_string(*stream_props.clock_domain());
    if (*stream_props.clock_domain() == fuchsia_hardware_audio::kClockDomainMonotonic) {
      clock_domain_str += "  (CLOCK_DOMAIN_MONOTONIC)";
    } else if (*stream_props.clock_domain() == fuchsia_hardware_audio::kClockDomainExternal) {
      clock_domain_str += "  (CLOCK_DOMAIN_EXTERNAL)";
    }
  } else {
    clock_domain_str += "<none> (non-compliant)";
  }
  FX_LOGS(INFO) << clock_domain_str;
}

void LogRingBufferFormatSets(
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& ring_buffer_format_sets) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/SupportedFormats";
  FX_LOGS(INFO) << "    ring_buffer_format_sets [" << ring_buffer_format_sets.size() << "]";
  for (auto idx = 0u; idx < ring_buffer_format_sets.size(); ++idx) {
    auto ring_buffer_format_set = ring_buffer_format_sets[idx];
    if (!ring_buffer_format_set.pcm_supported_formats()) {
      FX_LOGS(INFO) << "      [" << idx << "] <none> (non-compliant)";
      continue;
    }
    FX_LOGS(INFO) << "      [" << idx << "] pcm_supported_formats";
    const auto& pcm_format_set = *ring_buffer_format_set.pcm_supported_formats();
    if (pcm_format_set.channel_sets()) {
      const auto& channel_sets = *pcm_format_set.channel_sets();
      FX_LOGS(INFO) << "            channel_sets [" << channel_sets.size() << "]";
      for (auto idx = 0u; idx < channel_sets.size(); ++idx) {
        if (!channel_sets[idx].attributes()) {
          FX_LOGS(INFO) << "              [" << idx << "] <none> (non-compliant)";
          continue;
        }
        const auto& attribs = *channel_sets[idx].attributes();
        FX_LOGS(INFO) << "              [" << idx << "] attributes [" << attribs.size() << "]";
        for (auto idx = 0u; idx < attribs.size(); ++idx) {
          FX_LOGS(INFO) << "                   [" << idx << "]";
          FX_LOGS(INFO) << "                     min_frequency   "
                        << (attribs[idx].min_frequency()
                                ? std::to_string(*attribs[idx].min_frequency())
                                : "<none>");
          FX_LOGS(INFO) << "                     max_frequency   "
                        << (attribs[idx].max_frequency()
                                ? std::to_string(*attribs[idx].max_frequency())
                                : "<none>");
        }
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }

    if (pcm_format_set.sample_formats()) {
      const auto& sample_formats = *pcm_format_set.sample_formats();
      FX_LOGS(INFO) << "            sample_formats [" << sample_formats.size() << "]";
      for (auto idx = 0u; idx < sample_formats.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    " << sample_formats[idx];
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }
    if (pcm_format_set.bytes_per_sample()) {
      const auto& bytes_per_sample = *pcm_format_set.bytes_per_sample();
      FX_LOGS(INFO) << "            bytes_per_sample [" << bytes_per_sample.size() << "]";
      for (auto idx = 0u; idx < bytes_per_sample.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    "
                      << static_cast<int16_t>(bytes_per_sample[idx]);
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }
    if (pcm_format_set.valid_bits_per_sample()) {
      const auto& valid_bits_per_sample = *pcm_format_set.valid_bits_per_sample();
      FX_LOGS(INFO) << "            valid_bits_per_sample [" << valid_bits_per_sample.size() << "]";
      for (auto idx = 0u; idx < valid_bits_per_sample.size(); ++idx) {
        FX_LOGS(INFO) << "              [" << idx << "]    "
                      << static_cast<int16_t>(valid_bits_per_sample[idx]);
      }
    } else {
      FX_LOGS(INFO) << "            <none> (non-compliant)";
    }
    if (pcm_format_set.frame_rates()) {
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

void LogGainState(const fuchsia_hardware_audio::GainState& gain_state) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/GainState";
  FX_LOGS(INFO) << "    muted            "
                << (gain_state.muted() ? (*gain_state.muted() ? "TRUE" : "FALSE")
                                       : "<none> (Unmuted)");
  FX_LOGS(INFO) << "    agc_enabled      "
                << (gain_state.agc_enabled() ? (*gain_state.agc_enabled() ? "TRUE" : "FALSE")
                                             : "<none> (Disabled)");
  if (gain_state.gain_db()) {
    FX_LOGS(INFO) << "    gain_db          " << *gain_state.gain_db() << " dB";
  } else {
    FX_LOGS(INFO) << "    gain_db          <none> (non-compliant)";
  }
}

void LogPlugState(const fuchsia_hardware_audio::PlugState& plug_state) {
  if constexpr (!kLogStreamConfigFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/PlugState";
  FX_LOGS(INFO) << "    plugged          "
                << (plug_state.plugged() ? (*plug_state.plugged() ? "TRUE" : "FALSE")
                                         : "<none> (non-compliant)");
  FX_LOGS(INFO) << "    plug_state_time  "
                << (plug_state.plug_state_time() ? std::to_string(*plug_state.plug_state_time())
                                                 : "<none> (non-compliant)");
}

void LogCodecProperties(const fuchsia_hardware_audio::CodecProperties& codec_props) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/CodecProperties";

  FX_LOGS(INFO) << "    is_input          "
                << (codec_props.is_input() ? (*codec_props.is_input() ? "TRUE" : "FALSE")
                                           : "<none> (non-compliant)");
  FX_LOGS(INFO) << "    manufacturer      "
                << (codec_props.manufacturer() ? ("'" +
                                                  std::string(codec_props.manufacturer()->data(),
                                                              codec_props.manufacturer()->size()) +
                                                  "'")
                                               : "<none>");
  FX_LOGS(INFO) << "    product           "
                << (codec_props.product() ? ("'" +
                                             std::string(codec_props.product()->data(),
                                                         codec_props.product()->size()) +
                                             "'")
                                          : "<none>");
  FX_LOGS(INFO) << "    unique_id         " << UidToString(codec_props.unique_id());
  if (codec_props.plug_detect_capabilities()) {
    FX_LOGS(INFO) << "    plug_detect_caps  " << *codec_props.plug_detect_capabilities();
  } else {
    FX_LOGS(INFO) << "    plug_detect_caps  <none> (non-compliant)";
  }
}

void LogDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
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

void LogDaiFormat(std::optional<fuchsia_hardware_audio::DaiFormat> dai_format) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/DaiFormat";
  if (!dai_format) {
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

void LogCodecFormatInfo(std::optional<fuchsia_hardware_audio::CodecFormatInfo> format_info) {
  if constexpr (!kLogCodecFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/CodecFormatInfo";
  if (!format_info) {
    FX_LOGS(INFO) << "    UNSET";
    return;
  }
  FX_LOGS(INFO) << "    external_delay (ns)   "
                << (format_info->external_delay() ? std::to_string(*format_info->external_delay())
                                                  : "<none>");
  FX_LOGS(INFO) << "    turn_on_delay  (ns)   "
                << (format_info->turn_on_delay() ? std::to_string(*format_info->turn_on_delay())
                                                 : "<none>");
  FX_LOGS(INFO) << "    turn_off_delay (ns)   "
                << (format_info->turn_off_delay() ? std::to_string(*format_info->turn_off_delay())
                                                  : "<none>");
}

void LogElementState(const fuchsia_hardware_audio_signalprocessing::ElementState& element_state) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/ElementState";

  if (element_state.type_specific().has_value()) {
    switch (element_state.type_specific()->Which()) {
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kVendorSpecific:
        FX_LOGS(INFO) << "    type_specific         VendorSpecific";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kGain:
        FX_LOGS(INFO) << "    type_specific         Gain";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEqualizer:
        FX_LOGS(INFO) << "    type_specific         Equalizer";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kDynamics:
        FX_LOGS(INFO) << "    type_specific         Dynamics";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEndpoint:
        FX_LOGS(INFO) << "    type_specific         Endpoint";
        break;
      default:
        FX_LOGS(INFO) << "    type_specific         <unknown union>  (non-compliant)";
        break;
    }
  } else {
    FX_LOGS(INFO) << "    type_specific         <none>";
  }

  FX_LOGS(INFO) << "    enabled               "
                << (element_state.enabled().has_value()
                        ? (*element_state.enabled() ? "TRUE" : "FALSE")
                        : "<none>");

  if (element_state.latency().has_value()) {
    switch (element_state.latency()->Which()) {
      case fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyTime:
        FX_LOGS(INFO) << "    latency (time)";
        if (element_state.latency()->latency_time().has_value()) {
          FX_LOGS(INFO) << "                          "
                        << element_state.latency()->latency_time().value() << " ns";
        } else {
          FX_LOGS(INFO) << "                          <none> ns (non-compliant)";
        }
        break;
      case fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyFrames:
        FX_LOGS(INFO) << "    latency (frames)";
        if (element_state.latency()->latency_frames().has_value()) {
          FX_LOGS(INFO) << "                          "
                        << element_state.latency()->latency_frames().value() << " frames";
        } else {
          FX_LOGS(INFO) << "                          <none> frames (non-compliant)";
        }
        break;
      default:
        FX_LOGS(INFO) << "    latency <unknown union>  ( non-compliant)";
        break;
    }
  } else {
    FX_LOGS(INFO) << "    latency               <none>";
  }

  if (element_state.vendor_specific_data().has_value()) {
    FX_LOGS(INFO) << "    vendor_specific_data  [" << element_state.vendor_specific_data()->size()
                  << "]  (not shown here)"
                  << (element_state.vendor_specific_data()->empty() ? " (non-compliant)" : "");
  } else {
    FX_LOGS(INFO) << "    vendor_specific_data  <none>";
  }
}

void LogElementInternal(const fuchsia_hardware_audio_signalprocessing::Element& element,
                        std::string indent, std::optional<size_t> index = std::nullopt,
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

  FX_LOGS(INFO) << first_indent << "id               "
                << (element.id() ? std::to_string(*element.id()) : "<none> (non-compliant)");
  FX_LOGS(INFO) << indent << "type             " << element.type();

  std::ostringstream type_specific;
  type_specific << "type_specific    ";
  if (element.type_specific()) {
    switch (element.type_specific()->Which()) {
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kVendorSpecific:
        type_specific << "vendor_specific";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kGain:
        type_specific << "gain";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kEqualizer:
        type_specific << "equalizer";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kDynamics:
        type_specific << "dynamics";
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kEndpoint:
        type_specific << element.type_specific()->endpoint().value();
        break;
      default:
        type_specific << "OTHER (unknown enum)";
        break;
    }
  } else {
    type_specific << "<none>";
  }
  FX_LOGS(INFO) << indent << type_specific.str();

  FX_LOGS(INFO) << indent << "can_disable      "
                << (element.can_disable() ? (*element.can_disable() ? "TRUE" : "FALSE")
                                          : "<none> (FALSE)");
  FX_LOGS(INFO) << indent << "description      "
                << (element.description() ? std::string("'") + *element.description() + "'"
                                          : "<none>");
}
void LogElement(const fuchsia_hardware_audio_signalprocessing::Element& element) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Element";
  LogElementInternal(element, "    ");
}
void LogElements(const std::vector<fuchsia_hardware_audio_signalprocessing::Element>& elements) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Elements";
  FX_LOGS(INFO) << "  elements [" << elements.size() << "]";

  for (auto i = 0u; i < elements.size(); ++i) {
    LogElementInternal(elements[i], "   ", i, "    ");
  }
}

void LogTopologyInternal(const fuchsia_hardware_audio_signalprocessing::Topology& topology,
                         std::string indent, std::optional<size_t> index = std::nullopt,
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

  FX_LOGS(INFO) << first_indent << "id               "
                << (topology.id() ? std::to_string(*topology.id()) : "<none> (non-compliant)");
  if (topology.processing_elements_edge_pairs()) {
    FX_LOGS(INFO) << indent << "processing_elements_edge_pairs ["
                  << topology.processing_elements_edge_pairs()->size() << "]";
    for (auto idx = 0u; idx < topology.processing_elements_edge_pairs()->size(); ++idx) {
      FX_LOGS(INFO)
          << indent << " [" << idx << "]             "
          << topology.processing_elements_edge_pairs()->at(idx).processing_element_id_from()
          << " -> "
          << topology.processing_elements_edge_pairs()->at(idx).processing_element_id_to();
    }
  } else {
    FX_LOGS(INFO) << indent << "processing_elements_edge_pairs <none> (non-compliant)";
  }
}
void LogTopology(const fuchsia_hardware_audio_signalprocessing::Topology& topology) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Topology";
  LogTopologyInternal(topology, "        ");
}
void LogTopologies(
    const std::vector<fuchsia_hardware_audio_signalprocessing::Topology>& topologies) {
  if constexpr (!kLogSignalProcessingFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio_signalprocessing/Topologies";
  FX_LOGS(INFO) << "  topologies [" << topologies.size() << "]";

  for (auto idx = 0u; idx < topologies.size(); ++idx) {
    LogTopologyInternal(topologies[idx], "   ", idx, "    ");
  }
}

void LogDeviceInfo(const fuchsia_audio_device::Info& device_info) {
  if constexpr (kLogSummaryFinalDeviceInfo) {
    FX_LOGS(INFO) << "Detected " << device_info.device_type() << " device "
                  << (device_info.device_name()
                          ? std::string("'") + *device_info.device_name() + "'"
                          : "[nameless]")
                  << ", assigned token_id "
                  << (device_info.token_id() ? std::to_string(*device_info.token_id())
                                             : "<none> (non-compliant)");
  }
  if constexpr (!kLogDetailedFinalDeviceInfo) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_audio_device/Info";

  FX_LOGS(INFO) << "   token_id                     "
                << (device_info.token_id() ? std::to_string(*device_info.token_id())
                                           : "<none> (non-compliant)");

  FX_LOGS(INFO) << "   device_type                  " << device_info.device_type();

  FX_LOGS(INFO) << "   device_name                  "
                << (device_info.device_name() ? std::string("'") + *device_info.device_name() + "'"
                                              : "<none>");

  FX_LOGS(INFO) << "   manufacturer                 "
                << (device_info.manufacturer() ? "'" + *device_info.manufacturer() + "'"
                                               : "<none>");

  FX_LOGS(INFO) << "   product                      "
                << (device_info.product() ? "'" + *device_info.product() + "'" : "<none>");

  FX_LOGS(INFO) << "   unique_instance_id           "
                << UidToString(device_info.unique_instance_id());

  FX_LOGS(INFO) << "   is_input                     "
                << (device_info.is_input() ? (*device_info.is_input() ? "TRUE" : "FALSE")
                                           : "<none>");

  if (device_info.ring_buffer_format_sets()) {
    FX_LOGS(INFO) << "   ring_buffer_format_sets [" << device_info.ring_buffer_format_sets()->size()
                  << "]";
    for (auto j = 0u; j < device_info.ring_buffer_format_sets()->size(); ++j) {
      const auto& pcm_format_set = device_info.ring_buffer_format_sets()->at(j);
      if (pcm_format_set.channel_sets()) {
        FX_LOGS(INFO) << "         [" << j << "] channel_sets ["
                      << pcm_format_set.channel_sets()->size() << "]";
        for (auto k = 0u; k < pcm_format_set.channel_sets()->size(); ++k) {
          const auto& channel_set = pcm_format_set.channel_sets()->at(k);
          if (channel_set.attributes()) {
            FX_LOGS(INFO) << "              [" << k << "] attributes ["
                          << channel_set.attributes()->size() << "]";
            for (auto idx = 0u; idx < channel_set.attributes()->size(); ++idx) {
              const auto& attributes = channel_set.attributes()->at(idx);
              if (attributes.min_frequency()) {
                FX_LOGS(INFO) << "                   [" << idx << "] min_freq "
                              << *attributes.min_frequency();
              } else {
                FX_LOGS(INFO) << "                   [" << idx << "] min_freq <none>";
              }
              if (attributes.max_frequency()) {
                FX_LOGS(INFO) << "                       max_freq " << *attributes.max_frequency();
              } else {
                FX_LOGS(INFO) << "                       max_freq <none>";
              }
            }
          } else {
            FX_LOGS(INFO) << "               [" << k << "] attributes     <none>  (non-compliant)";
          }
        }
      } else {
        FX_LOGS(INFO) << "         [" << j << "] channel_sets       <none> (non-compliant)";
      }

      if (pcm_format_set.sample_types()) {
        FX_LOGS(INFO) << "             sample_types [" << pcm_format_set.sample_types()->size()
                      << "]";
        for (auto idx = 0u; idx < pcm_format_set.sample_types()->size(); ++idx) {
          FX_LOGS(INFO) << "              [" << idx << "]               "
                        << pcm_format_set.sample_types()->at(idx);
        }
      } else {
        FX_LOGS(INFO) << "             sample_types       <none> (non-compliant)";
      }
      if (pcm_format_set.frame_rates()) {
        FX_LOGS(INFO) << "             frame_rates [" << pcm_format_set.frame_rates()->size()
                      << "]";
        for (auto idx = 0u; idx < pcm_format_set.frame_rates()->size(); ++idx) {
          FX_LOGS(INFO) << "              [" << idx << "]               "
                        << pcm_format_set.frame_rates()->at(idx);
        }
      } else {
        FX_LOGS(INFO) << "             frame_rates        <none> (non-compliant)";
      }
    }
  } else {
    FX_LOGS(INFO) << "   ring_buffer_format_sets      <none>"
                  << ((device_info.device_type() == fuchsia_audio_device::DeviceType::kInput ||
                       device_info.device_type() == fuchsia_audio_device::DeviceType::kOutput)
                          ? " (non-compliant)"
                          : "");
  }

  // dai_format_sets
  if (device_info.dai_format_sets()) {
    FX_LOGS(INFO) << "   dai_format_sets [" << device_info.dai_format_sets()->size() << "]";
    for (auto j = 0u; j < device_info.dai_format_sets()->size(); ++j) {
      const auto& dai_format_set = device_info.dai_format_sets()->at(j);
      const auto& channel_counts = dai_format_set.number_of_channels();
      FX_LOGS(INFO) << "          [" << j << "] number_of_channels [" << channel_counts.size()
                    << "]";
      for (auto idx = 0u; idx < channel_counts.size(); ++idx) {
        FX_LOGS(INFO) << "               [" << idx << "]              " << channel_counts[idx];
      }

      const auto& sample_formats = dai_format_set.sample_formats();
      FX_LOGS(INFO) << "              sample_formats [" << sample_formats.size() << "]";
      for (auto idx = 0u; idx < sample_formats.size(); ++idx) {
        FX_LOGS(INFO) << "               [" << idx << "]              " << sample_formats[idx];
      }

      const auto& frame_formats = dai_format_set.frame_formats();
      FX_LOGS(INFO) << "              frame_formats [" << frame_formats.size() << "]";
      for (auto idx = 0u; idx < frame_formats.size(); ++idx) {
        FX_LOGS(INFO) << "               [" << idx << "]              " << frame_formats[idx];
      }

      const auto& frame_rates = dai_format_set.frame_rates();
      FX_LOGS(INFO) << "              frame_rates [" << frame_rates.size() << "]";
      for (auto idx = 0u; idx < frame_rates.size(); ++idx) {
        FX_LOGS(INFO) << "               [" << idx << "]              " << frame_rates[idx];
      }

      const auto& bits_per_slot = dai_format_set.bits_per_slot();
      FX_LOGS(INFO) << "              bits_per_slot [" << bits_per_slot.size() << "]";
      for (auto idx = 0u; idx < bits_per_slot.size(); ++idx) {
        FX_LOGS(INFO) << "               [" << idx << "]              "
                      << static_cast<int16_t>(bits_per_slot[idx]);
      }

      const auto& bits_per_sample = dai_format_set.bits_per_sample();
      FX_LOGS(INFO) << "              bits_per_sample [" << bits_per_sample.size() << "]";
      for (auto idx = 0u; idx < bits_per_sample.size(); ++idx) {
        FX_LOGS(INFO) << "               [" << idx << "]              "
                      << static_cast<int16_t>(bits_per_sample[idx]);
      }
    }
  } else {
    FX_LOGS(INFO) << "   dai_format_sets              <none>"
                  << ((device_info.device_type() == fuchsia_audio_device::DeviceType::kCodec)
                          ? " (non-compliant)"
                          : "");
  }

  if (device_info.gain_caps()) {
    if (device_info.gain_caps()->min_gain_db()) {
      FX_LOGS(INFO) << "   gain_caps  min_gain_db       " << *device_info.gain_caps()->min_gain_db()
                    << " dB";
    } else {
      FX_LOGS(INFO) << "   gain_caps  min_gain_db       <none> (non-compliant)";
    }
    if (device_info.gain_caps()->max_gain_db()) {
      FX_LOGS(INFO) << "              max_gain_db       " << *device_info.gain_caps()->max_gain_db()
                    << " dB";
    } else {
      FX_LOGS(INFO) << "              max_gain_db       <none> (non-compliant)";
    }
    if (device_info.gain_caps()->gain_step_db()) {
      FX_LOGS(INFO) << "              gain_step_db      "
                    << *device_info.gain_caps()->gain_step_db() << " dB";
    } else {
      FX_LOGS(INFO) << "              gain_step_db      <none> (non-compliant)";
    }
    FX_LOGS(INFO) << "              can_mute          "
                  << (device_info.gain_caps()->can_mute()
                          ? (*device_info.gain_caps()->can_mute() ? "true" : "false")
                          : "<none> (false)");
    FX_LOGS(INFO) << "              can_agc           "
                  << (device_info.gain_caps()->can_agc()
                          ? (*device_info.gain_caps()->can_agc() ? "true" : "false")
                          : "<none> (false)");
  } else {
    FX_LOGS(INFO) << "   gain_caps                    <none>"
                  << ((device_info.device_type() == fuchsia_audio_device::DeviceType::kInput ||
                       device_info.device_type() == fuchsia_audio_device::DeviceType::kOutput)
                          ? " (non-compliant)"
                          : "");
  }

  FX_LOGS(INFO) << "   plug_detect_caps             " << device_info.plug_detect_caps();

  std::string clock_domain_str{"   clock_domain                 "};
  if (device_info.clock_domain()) {
    clock_domain_str += std::to_string(*device_info.clock_domain());
    if (*device_info.clock_domain() == fuchsia_hardware_audio::kClockDomainMonotonic) {
      clock_domain_str += "  (CLOCK_DOMAIN_MONOTONIC)";
    } else if (*device_info.clock_domain() == fuchsia_hardware_audio::kClockDomainExternal) {
      clock_domain_str += "  (CLOCK_DOMAIN_EXTERNAL)";
    }
  } else {
    clock_domain_str += "<none>";
    if (device_info.device_type() == fuchsia_audio_device::DeviceType::kInput ||
        device_info.device_type() == fuchsia_audio_device::DeviceType::kOutput) {
      clock_domain_str += " (non-compliant)";
    }
  }
  FX_LOGS(INFO) << clock_domain_str;

  if (device_info.signal_processing_elements()) {
    FX_LOGS(INFO) << "   signal_processing_elements ["
                  << device_info.signal_processing_elements()->size() << "]"
                  << (device_info.signal_processing_elements()->empty() ? " (non-compliant)" : "");
    for (auto idx = 0u; idx < device_info.signal_processing_elements()->size(); ++idx) {
      LogElementInternal(device_info.signal_processing_elements()->at(idx), "           ", idx,
                         "    ");
    }
  } else {
    FX_LOGS(INFO) << "   signal_processing_elements   <none>";
  }

  if (device_info.signal_processing_topologies()) {
    FX_LOGS(INFO) << "   signal_processing_topologies ["
                  << device_info.signal_processing_topologies()->size() << "]"
                  << (device_info.signal_processing_topologies()->empty() ? " (non-compliant)"
                                                                          : "");
    for (auto idx = 0u; idx < device_info.signal_processing_topologies()->size(); ++idx) {
      LogTopologyInternal(device_info.signal_processing_topologies()->at(idx), "           ", idx,
                          "    ");
    }
  } else {
    FX_LOGS(INFO) << "   signal_processing_topologies <none>";
  }
}

void LogRingBufferProperties(const fuchsia_hardware_audio::RingBufferProperties& rb_props) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/RingBufferProperties";
  FX_LOGS(INFO) << "    needs_cache_flush       "
                << (rb_props.needs_cache_flush_or_invalidate()
                        ? (*rb_props.needs_cache_flush_or_invalidate() ? "TRUE" : "FALSE")
                        : "<none> (non-compliant)");

  if (rb_props.turn_on_delay()) {
    FX_LOGS(INFO) << "    turn_on_delay           " << *rb_props.turn_on_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    turn_on_delay           <none> (0 ns)";
  }

  if (rb_props.driver_transfer_bytes()) {
    FX_LOGS(INFO) << "    driver_transfer_bytes   " << *rb_props.driver_transfer_bytes()
                  << " bytes";
  } else {
    FX_LOGS(INFO) << "    driver_transfer_bytes   <none> (non-compliant)";
  }
}

void LogRingBufferFormat(const fuchsia_hardware_audio::Format& ring_buffer_format) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/Format";
  if (!ring_buffer_format.pcm_format()) {
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

void LogRingBufferVmo(const zx::vmo& vmo, uint32_t num_frames,
                      fuchsia_hardware_audio::Format rb_format) {
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
  FX_LOGS(INFO) << "fuchsia_hardware_audio/Vmo";
  FX_LOGS(INFO) << "    koid                 0x" << std::hex << info.koid;
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

void LogDelayInfo(const fuchsia_hardware_audio::DelayInfo& info) {
  if constexpr (!kLogRingBufferFidlResponseValues) {
    return;
  }

  FX_LOGS(INFO) << "fuchsia_hardware_audio/DelayInfo";
  if (info.internal_delay()) {
    FX_LOGS(INFO) << "    internal_delay       " << *info.internal_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    internal_delay       <none> (non-compliant)";
  }

  if (info.external_delay()) {
    FX_LOGS(INFO) << "    external_delay       " << *info.external_delay() << " ns";
  } else {
    FX_LOGS(INFO) << "    external_delay       <none> (0 ns)";
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
