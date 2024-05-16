// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/zx/time.h>

#include <unordered_map>

#include "src/media/audio/services/device_registry/testing/fake_composite.h"

namespace media_audio {

namespace fha = fuchsia_hardware_audio;
namespace fhasp = fuchsia_hardware_audio_signalprocessing;

// static definitions: signalprocessing elements/topologies and format sets

// DaiFormats and format sets
//
const fha::DaiFrameFormat FakeComposite::kDefaultDaiFrameFormat =
    fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S);
const fha::DaiFrameFormat FakeComposite::kDefaultDaiFrameFormat2 =
    fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kNone);

const std::vector<uint32_t> FakeComposite::kDefaultDaiNumberOfChannelsSet{
    kDefaultDaiNumberOfChannels, kDefaultDaiNumberOfChannels2};
const std::vector<uint32_t> FakeComposite::kDefaultDaiNumberOfChannelsSet2{
    kDefaultDaiNumberOfChannels2};
const std::vector<fha::DaiSampleFormat> FakeComposite::kDefaultDaiSampleFormatsSet{
    kDefaultDaiSampleFormat};
const std::vector<fha::DaiSampleFormat> FakeComposite::kDefaultDaiSampleFormatsSet2{
    kDefaultDaiSampleFormat, kDefaultDaiSampleFormat2};
const std::vector<fha::DaiFrameFormat> FakeComposite::kDefaultDaiFrameFormatsSet{
    kDefaultDaiFrameFormat, kDefaultDaiFrameFormat2};
const std::vector<fha::DaiFrameFormat> FakeComposite::kDefaultDaiFrameFormatsSet2{
    kDefaultDaiFrameFormat2};
const std::vector<uint32_t> FakeComposite::kDefaultDaiFrameRates{kDefaultDaiFrameRate};
const std::vector<uint32_t> FakeComposite::kDefaultDaiFrameRates2{kDefaultDaiFrameRate,
                                                                  kDefaultDaiFrameRate2};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSlotSet{kDefaultDaiBitsPerSlot,
                                                                    kDefaultDaiBitsPerSlot2};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSlotSet2{kDefaultDaiBitsPerSlot2};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSampleSet{kDefaultDaiBitsPerSample};
const std::vector<uint8_t> FakeComposite::kDefaultDaiBitsPerSampleSet2{kDefaultDaiBitsPerSample,
                                                                       kDefaultDaiBitsPerSample2};

const fha::DaiSupportedFormats FakeComposite::kDefaultDaiFormatSet{{
    .number_of_channels = kDefaultDaiNumberOfChannelsSet,
    .sample_formats = kDefaultDaiSampleFormatsSet,
    .frame_formats = kDefaultDaiFrameFormatsSet,
    .frame_rates = kDefaultDaiFrameRates,
    .bits_per_slot = kDefaultDaiBitsPerSlotSet,
    .bits_per_sample = kDefaultDaiBitsPerSampleSet,
}};
const fha::DaiSupportedFormats FakeComposite::kDefaultDaiFormatSet2{{
    .number_of_channels = kDefaultDaiNumberOfChannelsSet2,
    .sample_formats = kDefaultDaiSampleFormatsSet2,
    .frame_formats = kDefaultDaiFrameFormatsSet2,
    .frame_rates = kDefaultDaiFrameRates2,
    .bits_per_slot = kDefaultDaiBitsPerSlotSet2,
    .bits_per_sample = kDefaultDaiBitsPerSampleSet2,
}};

// DaiFormatSets that are returned by the driver.
const std::vector<fha::DaiSupportedFormats> FakeComposite::kDefaultDaiFormatSets{
    kDefaultDaiFormatSet};
const std::vector<fha::DaiSupportedFormats> FakeComposite::kDefaultDaiFormatSets2{
    kDefaultDaiFormatSet2};

// Map of Dai format sets, by element. Used within the driver.
const std::unordered_map<ElementId, std::vector<fha::DaiSupportedFormats>>
    FakeComposite::kDefaultDaiFormatsMap = {{
        {kSourceDaiElementId, kDefaultDaiFormatSets},
        {kDestDaiElementId, kDefaultDaiFormatSets2},
    }};

// Specific DAI formats
const fha::DaiFormat FakeComposite::kDefaultDaiFormat{{
    .number_of_channels = kDefaultDaiNumberOfChannels,
    .channels_to_use_bitmask = (1u << kDefaultDaiNumberOfChannels) - 1u,
    .sample_format = kDefaultDaiSampleFormat,
    .frame_format = kDefaultDaiFrameFormat,
    .frame_rate = kDefaultDaiFrameRate,
    .bits_per_slot = kDefaultDaiBitsPerSlot,
    .bits_per_sample = kDefaultDaiBitsPerSample,
}};
const fha::DaiFormat FakeComposite::kDefaultDaiFormat2{{
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
const fha::ChannelAttributes FakeComposite::kDefaultRbAttributes{{
    .min_frequency = kDefaultRbChannelAttributeMinFrequency,
    .max_frequency = kDefaultRbChannelAttributeMaxFrequency,
}};
const fha::ChannelAttributes FakeComposite::kDefaultRbAttributes2{{
    .min_frequency = kDefaultRbChannelAttributeMinFrequency2,
}};
const fha::ChannelAttributes FakeComposite::kDefaultRbAttributes3{{
    .max_frequency = kDefaultRbChannelAttributeMaxFrequency2,
}};
const std::vector<fha::ChannelAttributes> FakeComposite::kDefaultRbAttributeSet{
    kDefaultRbAttributes,
};
const std::vector<fha::ChannelAttributes> FakeComposite::kDefaultRbAttributeSet2{
    kDefaultRbAttributes2,
};
const fha::ChannelSet FakeComposite::kDefaultRbChannelsSet{{
    .attributes = kDefaultRbAttributeSet,
}};
const fha::ChannelSet FakeComposite::kDefaultRbChannelsSet2{{
    .attributes = kDefaultRbAttributeSet2,
}};
const std::vector<fha::ChannelSet> FakeComposite::kDefaultRbChannelsSets{
    kDefaultRbChannelsSet,
};
const std::vector<fha::ChannelSet> FakeComposite::kDefaultRbChannelsSets2{kDefaultRbChannelsSet2};

const std::vector<fha::SampleFormat> FakeComposite::kDefaultRbSampleFormats{kDefaultRbSampleFormat};
const std::vector<fha::SampleFormat> FakeComposite::kDefaultRbSampleFormats2{
    kDefaultRbSampleFormat2};
const std::vector<uint8_t> FakeComposite::kDefaultRbBytesPerSampleSet{kDefaultRbBytesPerSample};
const std::vector<uint8_t> FakeComposite::kDefaultRbBytesPerSampleSet2{kDefaultRbBytesPerSample2};
const std::vector<uint8_t> FakeComposite::kDefaultRbValidBitsPerSampleSet{
    kDefaultRbValidBitsPerSample};
const std::vector<uint8_t> FakeComposite::kDefaultRbValidBitsPerSampleSet2{
    kDefaultRbValidBitsPerSample2};
const std::vector<uint32_t> FakeComposite::kDefaultRbFrameRates{kDefaultRbFrameRate};
const std::vector<uint32_t> FakeComposite::kDefaultRbFrameRates2{kDefaultRbFrameRate2};

const fha::PcmSupportedFormats FakeComposite::kDefaultPcmRingBufferFormatSet{{
    .channel_sets = kDefaultRbChannelsSets,
    .sample_formats = kDefaultRbSampleFormats,
    .bytes_per_sample = kDefaultRbBytesPerSampleSet,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSampleSet,
    .frame_rates = kDefaultRbFrameRates,
}};
const fha::PcmSupportedFormats FakeComposite::kDefaultPcmRingBufferFormatSet2{{
    .channel_sets = kDefaultRbChannelsSets2,
    .sample_formats = kDefaultRbSampleFormats2,
    .bytes_per_sample = kDefaultRbBytesPerSampleSet2,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSampleSet2,
    .frame_rates = kDefaultRbFrameRates2,
}};
const fha::SupportedFormats FakeComposite::kDefaultRbFormatSet{{
    .pcm_supported_formats = kDefaultPcmRingBufferFormatSet,
}};
const fha::SupportedFormats FakeComposite::kDefaultRbFormatSet2{{
    .pcm_supported_formats = kDefaultPcmRingBufferFormatSet2,
}};

// RingBuffer format sets that are returned by the driver.
const std::vector<fha::SupportedFormats> FakeComposite::kDefaultRbFormatSets{kDefaultRbFormatSet};
const std::vector<fha::SupportedFormats> FakeComposite::kDefaultRbFormatSets2{kDefaultRbFormatSet2};

// Map of RingBuffer format sets, by element. Used internally by the driver.
const std::unordered_map<ElementId, std::vector<fha::SupportedFormats>>
    FakeComposite::kDefaultRbFormatsMap = {{
        {kDestRbElementId, kDefaultRbFormatSets},
        {kSourceRbElementId, kDefaultRbFormatSets2},
    }};

// Specific RingBuffer formats
const fha::PcmFormat FakeComposite::kDefaultRbFormat{{
    .number_of_channels = kDefaultRbNumberOfChannels,
    .sample_format = kDefaultRbSampleFormat,
    .bytes_per_sample = kDefaultRbBytesPerSample,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSample,
    .frame_rate = kDefaultRbFrameRate,
}};
const fha::PcmFormat FakeComposite::kDefaultRbFormat2{{
    .number_of_channels = kDefaultRbNumberOfChannels2,
    .sample_format = kDefaultRbSampleFormat2,
    .bytes_per_sample = kDefaultRbBytesPerSample2,
    .valid_bits_per_sample = kDefaultRbValidBitsPerSample2,
    .frame_rate = kDefaultRbFrameRate2,
}};

// signalprocessing elements and topologies
//
// Individual elements
const fhasp::Element FakeComposite::kSourceDaiElement{{
    .id = kSourceDaiElementId,
    .type = fhasp::ElementType::kDaiInterconnect,
    .type_specific = fhasp::TypeSpecificElement::WithDaiInterconnect({{
        .plug_detect_capabilities = fhasp::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
    .description = "DaiInterconnect source element description",
    .can_stop = true,
    .can_bypass = false,
}};
const fhasp::Element FakeComposite::kDestDaiElement{{
    .id = kDestDaiElementId,
    .type = fhasp::ElementType::kDaiInterconnect,
    .type_specific = fhasp::TypeSpecificElement::WithDaiInterconnect({{
        .plug_detect_capabilities = fhasp::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
    .description = "DaiInterconnect destination element description",
    .can_stop = true,
    .can_bypass = false,
}};
const fhasp::Element FakeComposite::kSourceRbElement{{
    .id = kSourceRbElementId,
    .type = fhasp::ElementType::kRingBuffer,
    .description = "RingBuffer source element description",
    .can_stop = false,
    .can_bypass = false,
}};
const fhasp::Element FakeComposite::kDestRbElement{{
    .id = kDestRbElementId,
    .type = fhasp::ElementType::kRingBuffer,
    .description = "RingBuffer destination element description",
    .can_stop = false,
    .can_bypass = false,
}};
const fhasp::Element FakeComposite::kMuteElement{{
    .id = kMuteElementId,
    .type = fhasp::ElementType::kMute,
    .description = "Mute element description",
    .can_stop = false,
    .can_bypass = true,
}};

// ElementStates - note that the two Dai elements have vendor_specific_data that can be queried.
const fhasp::Latency FakeComposite::kSourceDaiElementLatency = fhasp::Latency::WithLatencyTime(0);
const fhasp::Latency FakeComposite::kDestDaiElementLatency = fhasp::Latency::WithLatencyTime(10417);
const fhasp::Latency FakeComposite::kSourceRbElementLatency = fhasp::Latency::WithLatencyFrames(1);
const fhasp::Latency FakeComposite::kDestRbElementLatency = fhasp::Latency::WithLatencyFrames(0);
const fhasp::ElementState FakeComposite::kSourceDaiElementInitState{{
    .type_specific = fhasp::TypeSpecificElementState::WithDaiInterconnect({{
        .plug_state = fhasp::PlugState{{
            .plugged = true,
            .plug_state_time = 0,
        }},
    }}),
    .latency = kSourceDaiElementLatency,
    .vendor_specific_data = std::vector<uint8_t>{1, 2, 3, 4, 5, 6, 7, 8},
    .started = false,
    .bypassed = false,
}};
const fhasp::ElementState FakeComposite::kDestDaiElementInitState{{
    .type_specific = fhasp::TypeSpecificElementState::WithDaiInterconnect({{
        .plug_state = fhasp::PlugState{{
            .plugged = true,
            .plug_state_time = 0,
        }},
    }}),
    .latency = kDestDaiElementLatency,
    .vendor_specific_data = std::vector<uint8_t>{8, 7, 6, 5, 4, 3, 2, 1, 0},
    .started = false,
    .bypassed = false,
}};
const fhasp::ElementState FakeComposite::kSourceRbElementInitState{{
    .latency = kSourceRbElementLatency,
    .started = true,
    .bypassed = false,
}};
const fhasp::ElementState FakeComposite::kDestRbElementInitState{{
    .latency = kDestRbElementLatency,
    .started = true,
    .bypassed = false,
}};
const fhasp::ElementState FakeComposite::kMuteElementInitState{{
    .started = true,
    .bypassed = true,
}};

// Element set
const std::vector<fhasp::Element> FakeComposite::kElements{{
    kSourceDaiElement,
    kDestDaiElement,
    kSourceRbElement,
    kDestRbElement,
    kMuteElement,
}};

// Topologies and element paths
//
// element paths
const fhasp::EdgePair FakeComposite::kTopologyInputEdgePair{{
    .processing_element_id_from = kSourceDaiElementId,
    .processing_element_id_to = kDestRbElementId,
}};
const fhasp::EdgePair FakeComposite::kTopologyOutputEdgePair{{
    .processing_element_id_from = kSourceRbElementId,
    .processing_element_id_to = kDestDaiElementId,
}};
const fhasp::EdgePair FakeComposite::kTopologyRbToMuteEdgePair{{
    .processing_element_id_from = kSourceRbElementId,
    .processing_element_id_to = kMuteElementId,
}};
const fhasp::EdgePair FakeComposite::kTopologyMuteToDaiEdgePair{{
    .processing_element_id_from = kMuteElementId,
    .processing_element_id_to = kDestDaiElementId,
}};

// Individual topologies
const fhasp::Topology FakeComposite::kInputOnlyTopology{{
    .id = kInputOnlyTopologyId,
    .processing_elements_edge_pairs = {{
        kTopologyInputEdgePair,
    }},
}};
const fhasp::Topology FakeComposite::kFullDuplexTopology{{
    .id = kFullDuplexTopologyId,
    .processing_elements_edge_pairs = {{
        kTopologyInputEdgePair,
        kTopologyOutputEdgePair,
    }},
}};
const fhasp::Topology FakeComposite::kOutputOnlyTopology{{
    .id = kOutputOnlyTopologyId,
    .processing_elements_edge_pairs = {{
        kTopologyOutputEdgePair,
    }},
}};
const fhasp::Topology FakeComposite::kOutputWithMuteTopology{{
    .id = kOutputWithMuteTopologyId,
    .processing_elements_edge_pairs = {{
        kTopologyRbToMuteEdgePair,
        kTopologyMuteToDaiEdgePair,
    }},
}};

// Topology set
const std::vector<fhasp::Topology> FakeComposite::kTopologies{{
    kInputOnlyTopology,
    kFullDuplexTopology,
    kOutputOnlyTopology,
    kOutputWithMuteTopology,
}};

}  // namespace media_audio
