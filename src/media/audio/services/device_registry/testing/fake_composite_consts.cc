// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>

#include <unordered_map>

#include "src/media/audio/services/device_registry/testing/fake_composite.h"

namespace media_audio {

// static definitions: signalprocessing elements/topologies and format sets

// DaiFormats and format sets
//
const fuchsia_hardware_audio::DaiFrameFormat FakeComposite::kDefaultDaiFrameFormat =
    fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
        fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S);
const fuchsia_hardware_audio::DaiFrameFormat FakeComposite::kDefaultDaiFrameFormat2 =
    fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
        fuchsia_hardware_audio::DaiFrameFormatStandard::kNone);

const std::vector<uint32_t> FakeComposite::kDefaultDaiNumberOfChannelsSet{
    kDefaultDaiNumberOfChannels, kDefaultDaiNumberOfChannels2};
const std::vector<uint32_t> FakeComposite::kDefaultDaiNumberOfChannelsSet2{
    kDefaultDaiNumberOfChannels2};
const std::vector<fuchsia_hardware_audio::DaiSampleFormat>
    FakeComposite::kDefaultDaiSampleFormatsSet{kDefaultDaiSampleFormat};
const std::vector<fuchsia_hardware_audio::DaiSampleFormat>
    FakeComposite::kDefaultDaiSampleFormatsSet2{kDefaultDaiSampleFormat, kDefaultDaiSampleFormat2};
const std::vector<fuchsia_hardware_audio::DaiFrameFormat> FakeComposite::kDefaultDaiFrameFormatsSet{
    kDefaultDaiFrameFormat, kDefaultDaiFrameFormat2};
const std::vector<fuchsia_hardware_audio::DaiFrameFormat>
    FakeComposite::kDefaultDaiFrameFormatsSet2{kDefaultDaiFrameFormat2};
const std::vector<uint32_t> FakeComposite::kDefaultDaiFrameRates{kDefaultDaiFrameRate};
const std::vector<uint32_t> FakeComposite::kDefaultDaiFrameRates2{kDefaultDaiFrameRate,
                                                                  kDefaultDaiFrameRate2};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSlotSet{kDefaultDaiBitsPerSlot,
                                                                    kDefaultDaiBitsPerSlot2};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSlotSet2{kDefaultDaiBitsPerSlot2};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSampleSet{kDefaultDaiBitsPerSample};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSampleSet2{kDefaultDaiBitsPerSample,
                                                                       kDefaultDaiBitsPerSample2};

const fuchsia_hardware_audio::DaiSupportedFormats FakeComposite::kDefaultDaiFormatSet{{
    .number_of_channels = kDefaultDaiNumberOfChannelsSet,
    .sample_formats = kDefaultDaiSampleFormatsSet,
    .frame_formats = kDefaultDaiFrameFormatsSet,
    .frame_rates = kDefaultDaiFrameRates,
    .bits_per_slot = kDefaultDaiBitsPerSlotSet,
    .bits_per_sample = kDefaultDaiBitsPerSampleSet,
}};
const fuchsia_hardware_audio::DaiSupportedFormats FakeComposite::kDefaultDaiFormatSet2{{
    .number_of_channels = kDefaultDaiNumberOfChannelsSet2,
    .sample_formats = kDefaultDaiSampleFormatsSet2,
    .frame_formats = kDefaultDaiFrameFormatsSet2,
    .frame_rates = kDefaultDaiFrameRates2,
    .bits_per_slot = kDefaultDaiBitsPerSlotSet2,
    .bits_per_sample = kDefaultDaiBitsPerSampleSet2,
}};

// DaiFormatSets that are returned by the driver.
const std::vector<fuchsia_hardware_audio::DaiSupportedFormats> FakeComposite::kDefaultDaiFormatSets{
    kDefaultDaiFormatSet};
const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>
    FakeComposite::kDefaultDaiFormatSets2{kDefaultDaiFormatSet2};

// Map of Dai format sets, by element. Used within the driver.
const std::unordered_map<ElementId, std::vector<fuchsia_hardware_audio::DaiSupportedFormats>>
    FakeComposite::kDefaultDaiFormatsMap = {{
        {kSourceDaiElementId, kDefaultDaiFormatSets},
        {kDestDaiElementId, kDefaultDaiFormatSets2},
    }};

// Specific DAI formats
const fuchsia_hardware_audio::DaiFormat FakeComposite::kDefaultDaiFormat{{
    .number_of_channels = kDefaultDaiNumberOfChannels,
    .channels_to_use_bitmask = (1u << kDefaultDaiNumberOfChannels) - 1u,
    .sample_format = kDefaultDaiSampleFormat,
    .frame_format = kDefaultDaiFrameFormat,
    .frame_rate = kDefaultDaiFrameRate,
    .bits_per_slot = kDefaultDaiBitsPerSlot,
    .bits_per_sample = kDefaultDaiBitsPerSample,
}};
const fuchsia_hardware_audio::DaiFormat FakeComposite::kDefaultDaiFormat2{{
    .number_of_channels = kDefaultDaiNumberOfChannels2,
    .channels_to_use_bitmask = (1u << kDefaultDaiNumberOfChannels2) - 1u,
    .sample_format = kDefaultDaiSampleFormat2,
    .frame_format = kDefaultDaiFrameFormat2,
    .frame_rate = kDefaultDaiFrameRate2,
    .bits_per_slot = kDefaultDaiBitsPerSlot2,
    .bits_per_sample = kDefaultDaiBitsPerSample2,
}};

// RingBufferFormats and format sets
//
const fuchsia_hardware_audio::ChannelAttributes FakeComposite::kDefaultRbAttributes{{
    .min_frequency = kDefaultRbChannelAttributeMinFrequency,
    .max_frequency = kDefaultRbChannelAttributeMaxFrequency,
}};
const fuchsia_hardware_audio::ChannelAttributes FakeComposite::kDefaultRbAttributes2{{
    .min_frequency = kDefaultRbChannelAttributeMinFrequency2,
}};
const fuchsia_hardware_audio::ChannelAttributes FakeComposite::kDefaultRbAttributes3{{
    .max_frequency = kDefaultRbChannelAttributeMaxFrequency2,
}};
const std::vector<fuchsia_hardware_audio::ChannelAttributes> FakeComposite::kDefaultRbAttributeSet{
    kDefaultRbAttributes,
};
const std::vector<fuchsia_hardware_audio::ChannelAttributes> FakeComposite::kDefaultRbAttributeSet2{
    kDefaultRbAttributes2,
};
const fuchsia_hardware_audio::ChannelSet FakeComposite::kDefaultRbChannelsSet{{
    .attributes = kDefaultRbAttributeSet,
}};
const fuchsia_hardware_audio::ChannelSet FakeComposite::kDefaultRbChannelsSet2{{
    .attributes = kDefaultRbAttributeSet2,
}};
const std::vector<fuchsia_hardware_audio::ChannelSet> FakeComposite::kDefaultRbChannelsSets{
    kDefaultRbChannelsSet,
};
const std::vector<fuchsia_hardware_audio::ChannelSet> FakeComposite::kDefaultRbChannelsSets2{
    kDefaultRbChannelsSet2};

const std::vector<fuchsia_hardware_audio::SampleFormat> FakeComposite::kDefaultRbSampleFormats{
    kDefaultRbSampleFormat};
const std::vector<fuchsia_hardware_audio::SampleFormat> FakeComposite::kDefaultRbSampleFormats2{
    kDefaultRbSampleFormat2};
const std::vector<uint8_t> FakeComposite::kDefaultRbBytesPerSampleSet{kDefaultRbBytesPerSample};
const std::vector<uint8_t> FakeComposite::kDefaultRbBytesPerSampleSet2{kDefaultRbBytesPerSample2};
const std::vector<uint8_t> FakeComposite::kDefaultRbValidBitsPerSampleSet{
    kDefaultRbValidBitsPerSample};
const std::vector<uint8_t> FakeComposite::kDefaultRbValidBitsPerSampleSet2{
    kDefaultRbValidBitsPerSample2};
const std::vector<uint32_t> FakeComposite::kDefaultRbFrameRates{kDefaultRbFrameRate};
const std::vector<uint32_t> FakeComposite::kDefaultRbFrameRates2{kDefaultRbFrameRate2};

const fuchsia_hardware_audio::PcmSupportedFormats FakeComposite::kDefaultPcmRingBufferFormatSet{{
    .channel_sets = kDefaultRbChannelsSets,
    .sample_formats = kDefaultRbSampleFormats,
    .bytes_per_sample = kDefaultRbBytesPerSampleSet,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSampleSet,
    .frame_rates = kDefaultRbFrameRates,
}};
const fuchsia_hardware_audio::PcmSupportedFormats FakeComposite::kDefaultPcmRingBufferFormatSet2{{
    .channel_sets = kDefaultRbChannelsSets2,
    .sample_formats = kDefaultRbSampleFormats2,
    .bytes_per_sample = kDefaultRbBytesPerSampleSet2,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSampleSet2,
    .frame_rates = kDefaultRbFrameRates2,
}};
const fuchsia_hardware_audio::SupportedFormats FakeComposite::kDefaultRbFormatSet{{
    .pcm_supported_formats = kDefaultPcmRingBufferFormatSet,
}};
const fuchsia_hardware_audio::SupportedFormats FakeComposite::kDefaultRbFormatSet2{{
    .pcm_supported_formats = kDefaultPcmRingBufferFormatSet2,
}};

// RingBuffer format sets that are returned by the driver.
const std::vector<fuchsia_hardware_audio::SupportedFormats> FakeComposite::kDefaultRbFormatSets{
    kDefaultRbFormatSet};
const std::vector<fuchsia_hardware_audio::SupportedFormats> FakeComposite::kDefaultRbFormatSets2{
    kDefaultRbFormatSet2};

// Map of RingBuffer format sets, by element. Used internally by the driver.
const std::unordered_map<ElementId, std::vector<fuchsia_hardware_audio::SupportedFormats>>
    FakeComposite::kDefaultRbFormatsMap = {{
        {kDestRbElementId, kDefaultRbFormatSets},
        {kSourceRbElementId, kDefaultRbFormatSets2},
    }};

// Specific RingBuffer formats
const fuchsia_hardware_audio::PcmFormat FakeComposite::kDefaultRbFormat{{
    .number_of_channels = kDefaultRbNumberOfChannels,
    .sample_format = kDefaultRbSampleFormat,
    .bytes_per_sample = kDefaultRbBytesPerSample,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSample,
    .frame_rate = kDefaultRbFrameRate,
}};
const fuchsia_hardware_audio::PcmFormat FakeComposite::kDefaultRbFormat2{{
    .number_of_channels = kDefaultRbNumberOfChannels2,
    .sample_format = kDefaultRbSampleFormat2,
    .bytes_per_sample = kDefaultRbBytesPerSample2,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSample2,
    .frame_rate = kDefaultRbFrameRate2,
}};

// signalprocessing elements and topologies
//
// Individual elements
const fuchsia_hardware_audio_signalprocessing::Element FakeComposite::kSourceDaiElement{{
    .id = kSourceDaiElementId,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint({{
        .type = fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
        .plug_detect_capabilities =
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
    .can_disable = false,
    .description = "Endpoint::DaiInterconnect source element description",
}};
const fuchsia_hardware_audio_signalprocessing::Element FakeComposite::kDestRbElement{{
    .id = kDestRbElementId,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint({{
        .type = fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
        .plug_detect_capabilities =
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired,
    }}),
    .can_disable = false,
    .description = "Endpoint::RingBuffer destination element description",
}};
const fuchsia_hardware_audio_signalprocessing::Element FakeComposite::kSourceRbElement{{
    .id = kSourceRbElementId,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint({{
        .type = fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
        .plug_detect_capabilities =
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired,
    }}),
    .can_disable = false,
    .description = "Endpoint::RingBuffer source element description",
}};
const fuchsia_hardware_audio_signalprocessing::Element FakeComposite::kDestDaiElement{{
    .id = kDestDaiElementId,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint({{
        .type = fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
        .plug_detect_capabilities =
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
    .can_disable = false,
    .description = "Endpoint::DaiInterconnect destination element description",
}};

// ElementStates - note that the two Dai endpoints have vendor_specific_data that can be queried.
const fuchsia_hardware_audio_signalprocessing::Latency FakeComposite::kSourceDaiElementLatency =
    fuchsia_hardware_audio_signalprocessing::Latency::WithLatencyTime(0);
const fuchsia_hardware_audio_signalprocessing::Latency FakeComposite::kDestRbElementLatency =
    fuchsia_hardware_audio_signalprocessing::Latency::WithLatencyFrames(0);
const fuchsia_hardware_audio_signalprocessing::Latency FakeComposite::kSourceRbElementLatency =
    fuchsia_hardware_audio_signalprocessing::Latency::WithLatencyFrames(1);
const fuchsia_hardware_audio_signalprocessing::Latency FakeComposite::kDestDaiElementLatency =
    fuchsia_hardware_audio_signalprocessing::Latency::WithLatencyTime(10417);
const fuchsia_hardware_audio_signalprocessing::ElementState
    FakeComposite::kSourceDaiElementInitState{{
        .type_specific =
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint({{
                .plug_state = fuchsia_hardware_audio_signalprocessing::PlugState{{
                    .plugged = true,
                    .plug_state_time = ZX_TIME_INFINITE_PAST,
                }},
            }}),
        .enabled = true,
        .latency = kSourceDaiElementLatency,
        .vendor_specific_data = std::vector<uint8_t>{1, 2, 3, 4, 5, 6, 7, 8},
    }};
const fuchsia_hardware_audio_signalprocessing::ElementState FakeComposite::kDestRbElementInitState{{
    .type_specific =
        fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint({{
            .plug_state = fuchsia_hardware_audio_signalprocessing::PlugState{{
                .plugged = true,
                .plug_state_time = ZX_TIME_INFINITE_PAST,
            }},
        }}),
    .enabled = true,
    .latency = kDestRbElementLatency,
}};
const fuchsia_hardware_audio_signalprocessing::ElementState
    FakeComposite::kSourceRbElementInitState{{
        .type_specific =
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint({{
                .plug_state = fuchsia_hardware_audio_signalprocessing::PlugState{{
                    .plugged = true,
                    .plug_state_time = ZX_TIME_INFINITE_PAST,
                }},
            }}),
        .enabled = true,
        .latency = kSourceRbElementLatency,
    }};
const fuchsia_hardware_audio_signalprocessing::ElementState FakeComposite::kDestDaiElementInitState{
    {
        .type_specific =
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint({{
                .plug_state = fuchsia_hardware_audio_signalprocessing::PlugState{{
                    .plugged = true,
                    .plug_state_time = ZX_TIME_INFINITE_PAST,
                }},
            }}),
        .enabled = true,
        .latency = kDestDaiElementLatency,
        .vendor_specific_data = std::vector<uint8_t>{8, 7, 6, 5, 4, 3, 2, 1, 0},
    }};

// Element set
const std::vector<fuchsia_hardware_audio_signalprocessing::Element> FakeComposite::kElements{{
    kSourceDaiElement,
    kDestDaiElement,
    kSourceRbElement,
    kDestRbElement,
}};

// Topologies and element paths
//
// element paths
const fuchsia_hardware_audio_signalprocessing::EdgePair FakeComposite::kTopologyInputEdgePair{{
    .processing_element_id_from = kSourceDaiElementId,
    .processing_element_id_to = kDestRbElementId,
}};
const fuchsia_hardware_audio_signalprocessing::EdgePair FakeComposite::kTopologyOutputEdgePair{{
    .processing_element_id_from = kSourceRbElementId,
    .processing_element_id_to = kDestDaiElementId,
}};

// Individual topologies
const fuchsia_hardware_audio_signalprocessing::Topology FakeComposite::kInputOnlyTopology{{
    .id = kInputOnlyTopologyId,
    .processing_elements_edge_pairs = {{
        kTopologyInputEdgePair,
    }},
}};
const fuchsia_hardware_audio_signalprocessing::Topology FakeComposite::kFullDuplexTopology{{
    .id = kFullDuplexTopologyId,
    .processing_elements_edge_pairs = {{
        kTopologyInputEdgePair,
        kTopologyOutputEdgePair,
    }},
}};
const fuchsia_hardware_audio_signalprocessing::Topology FakeComposite::kOutputOnlyTopology{{
    .id = kOutputOnlyTopologyId,
    .processing_elements_edge_pairs = {{
        kTopologyOutputEdgePair,
    }},
}};

// Topology set
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> FakeComposite::kTopologies{{
    kInputOnlyTopology,
    kFullDuplexTopology,
    kOutputOnlyTopology,
}};

}  // namespace media_audio
