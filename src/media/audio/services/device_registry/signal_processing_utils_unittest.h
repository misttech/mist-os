// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_SIGNAL_PROCESSING_UTILS_UNITTEST_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_SIGNAL_PROCESSING_UTILS_UNITTEST_H_

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>

#include <vector>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

constexpr ElementId kElementId1 = 1;
constexpr ElementId kElementId2 = 2;
constexpr ElementId kElementId3 = 42;
constexpr ElementId kElementId4 = 0;
constexpr ElementId kOtherElementId = 68;

const fuchsia_hardware_audio_signalprocessing::ElementState kElementState1{{
    .type_specific =
        fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint({{
            .plug_state = fuchsia_hardware_audio_signalprocessing::PlugState{{
                .plugged = true,
                .plug_state_time = 0,
            }},
        }}),
}};
const fuchsia_hardware_audio_signalprocessing::ElementState kElementStateEmpty{};

const fuchsia_hardware_audio_signalprocessing::Element kElement1{{
    .id = kElementId1,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint({{
        .type = fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
        .plug_detect_capabilities =
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
    .can_disable = false,
    .description = " ",
}};
const fuchsia_hardware_audio_signalprocessing::Element kElement2{{
    .id = kElementId2,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kAutomaticGainControl,
    // .type_specific is missing
    // .can_disable is missing
    // .description is missing
}};
const fuchsia_hardware_audio_signalprocessing::Element kElement3{{
    .id = kElementId3,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kDynamics,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithDynamics({{
        .bands = {{
            {{.id = 0}},
        }},
        .supported_controls =
            fuchsia_hardware_audio_signalprocessing::DynamicsSupportedControls::kKneeWidth,
    }}),
    .can_disable = true,
    .description = std::string("Test signalprocessing element description                       ") +
                   std::string("                   At this point, we are nearing the end of the ") +
                   std::string("          maximal-length 256-char string. Note that this string ") +
                   std::string("           has an upper-case 'x' as the last character.   54321X"),
}};
const fuchsia_hardware_audio_signalprocessing::Element kElement4{{
    .id = kElementId4,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint({{
        .type = fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer,
        .plug_detect_capabilities =
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired,
    }}),
    // .can_disable is missing
    // .description is missing
}};
const fuchsia_hardware_audio_signalprocessing::Element kElementNoId{{
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kAutomaticGainControl,
}};
const fuchsia_hardware_audio_signalprocessing::Element kElementNoType{{
    .id = kOtherElementId,
}};
const fuchsia_hardware_audio_signalprocessing::Element kElementNoRequiredTypeSpecific{{
    .id = kOtherElementId,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint,
}};
const fuchsia_hardware_audio_signalprocessing::Element kElementWrongTypeSpecific{{
    .id = kOtherElementId,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kDynamics,
    .type_specific = fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint({{
        .type = fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect,
        .plug_detect_capabilities =
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify,
    }}),
}};
const fuchsia_hardware_audio_signalprocessing::Element kElementEmptyDescription{{
    .id = kOtherElementId,
    .type = fuchsia_hardware_audio_signalprocessing::ElementType::kAutomaticGainControl,
    .description = "",
}};

const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElements{kElement1, kElement2,
                                                                              kElement3, kElement4};
const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kEmptyElements{};
const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElementsDuplicateId{kElement1,
                                                                                         kElement1};
const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElementsWithNoId{
    kElement1, kElement2, kElement3, kElement4, kElementNoId};
const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElementsWithNoType{
    kElement1, kElement2, kElement3, kElement4, kElementNoType};
const std::vector<fuchsia_hardware_audio_signalprocessing::Element>
    kElementsWithNoRequiredTypeSpecific{kElement1, kElement2, kElement3, kElement4,
                                        kElementNoRequiredTypeSpecific};
const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElementsWithWrongTypeSpecific{
    kElement1, kElement2, kElement3, kElement4, kElementWrongTypeSpecific};
const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElementsWithEmptyDescription{
    kElement1, kElement2, kElement3, kElement4, kElementEmptyDescription};

constexpr TopologyId kTopologyId1234 = 0;
constexpr TopologyId kTopologyId14 = 10;
constexpr TopologyId kTopologyId41 = 7;
constexpr TopologyId kOtherTopologyId = 42;

const fuchsia_hardware_audio_signalprocessing::EdgePair kEdge12{{kElementId1, kElementId2}};
const fuchsia_hardware_audio_signalprocessing::EdgePair kEdge23{{kElementId2, kElementId3}};
const fuchsia_hardware_audio_signalprocessing::EdgePair kEdge34{{kElementId3, kElementId4}};
const fuchsia_hardware_audio_signalprocessing::EdgePair kEdge14{{kElementId1, kElementId4}};
const fuchsia_hardware_audio_signalprocessing::EdgePair kEdge41{{kElementId4, kElementId1}};
const fuchsia_hardware_audio_signalprocessing::EdgePair kEdgeToSelf{{kElementId4, kElementId4}};
const fuchsia_hardware_audio_signalprocessing::EdgePair kEdgeUnknownId{
    {kElementId4, kOtherElementId}};

const fuchsia_hardware_audio_signalprocessing::Topology kTopology1234{{
    .id = kTopologyId1234,
    .processing_elements_edge_pairs = {{kEdge12, kEdge23, kEdge34}},
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopology14{{
    .id = kTopologyId14,
    .processing_elements_edge_pairs = {{kEdge14}},
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopology41{{
    .id = kTopologyId41,
    .processing_elements_edge_pairs = {{kEdge41}},
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopologyMissingId{{
    .processing_elements_edge_pairs = {{kEdge14}},
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopologyMissingEdgePairs{{
    .id = kOtherTopologyId,
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopologyEmptyEdgePairs{{
    .id = kOtherTopologyId,
    .processing_elements_edge_pairs = {{}},
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopologyUnknownElementId{{
    .id = kOtherTopologyId,
    .processing_elements_edge_pairs = {{kEdge12, kEdge23, kEdge34, kEdgeUnknownId}},
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopologyEdgePairLoop{{
    .id = kOtherTopologyId,
    .processing_elements_edge_pairs = {{kEdge12, kEdge23, kEdge34, kEdgeToSelf}},
}};
const fuchsia_hardware_audio_signalprocessing::Topology kTopologyTerminalNotEndpoint{{
    .id = kOtherTopologyId,
    .processing_elements_edge_pairs = {{kEdge41, kEdge12, kEdge23}},
}};

const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologies{
    kTopology1234, kTopology14, kTopology41};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kEmptyTopologies{};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologiesWithDuplicateId{
    kTopology1234, kTopology1234};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologiesWithoutAllElements{
    kTopology14};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologiesWithMissingId{
    kTopology1234, kTopologyMissingId};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology>
    kTopologiesWithMissingEdgePairs{kTopology1234, kTopologyMissingEdgePairs};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologiesWithEmptyEdgePairs{
    kTopology1234, kTopologyEmptyEdgePairs};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology>
    kTopologiesWithUnknownElementId{kTopology1234, kTopologyUnknownElementId};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologiesWithLoop{
    kTopology1234, kTopologyEdgePairLoop};
const std::vector<fuchsia_hardware_audio_signalprocessing::Topology>
    kTopologiesWithTerminalNotEndpoint{kTopology1234, kTopologyTerminalNotEndpoint};

const std::unordered_map<ElementId, ElementRecord> kEmptyElementMap{};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_SIGNAL_PROCESSING_UTILS_UNITTEST_H_
