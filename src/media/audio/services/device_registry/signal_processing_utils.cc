// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/signal_processing_utils.h"

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <lib/syslog/cpp/macros.h>

#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

std::unordered_set<ElementId> dai_endpoints(
    const std::unordered_map<ElementId, fuchsia_hardware_audio_signalprocessing::Element>&
        element_map) {
  std::unordered_set<ElementId> dai_endpoints;
  for (const auto& element : element_map) {
    if (element.second.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint &&
        element.second.type_specific()->endpoint()->type() ==
            fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect) {
      dai_endpoints.insert(element.first);
    }
  }
  return dai_endpoints;
}

std::unordered_set<ElementId> ring_buffer_endpoints(
    const std::unordered_map<ElementId, fuchsia_hardware_audio_signalprocessing::Element>&
        element_map) {
  std::unordered_set<ElementId> ring_buffer_endpoints;
  for (const auto& element : element_map) {
    if (element.second.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint &&
        element.second.type_specific()->endpoint()->type() ==
            fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer) {
      ring_buffer_endpoints.insert(element.first);
    }
  }
  return ring_buffer_endpoints;
}

std::unordered_map<ElementId, fuchsia_hardware_audio_signalprocessing::Element> MapElements(
    const std::vector<fuchsia_hardware_audio_signalprocessing::Element>& elements) {
  auto element_map =
      std::unordered_map<ElementId, fuchsia_hardware_audio_signalprocessing::Element>{};

  for (const auto& element : elements) {
    if (ValidateElement(element) != ZX_OK) {
      FX_LOGS(WARNING) << "invalid element";
      return {};
    }
    if (!element_map.insert({*element.id(), element}).second) {
      FX_LOGS(WARNING) << "duplicate element_id " << *element.id();
      return {};
    }
  }
  return element_map;
}

// Returns empty map if any topology_id values are duplicated.
std::unordered_map<TopologyId, std::vector<fuchsia_hardware_audio_signalprocessing::EdgePair>>
MapTopologies(const std::vector<fuchsia_hardware_audio_signalprocessing::Topology>& topologies) {
  auto topology_map =
      std::unordered_map<TopologyId,
                         std::vector<fuchsia_hardware_audio_signalprocessing::EdgePair>>{};

  for (const auto& topology : topologies) {
    if (!topology.id().has_value() || !topology.processing_elements_edge_pairs().has_value() ||
        topology.processing_elements_edge_pairs()->empty()) {
      FX_LOGS(WARNING) << "incomplete topology";
      return {};
    }
    if (!topology_map.insert({*topology.id(), *topology.processing_elements_edge_pairs()}).second) {
      FX_LOGS(WARNING) << "Cannot map duplicate topology_id " << *topology.id();
      return {};
    }
  }
  return topology_map;
}

}  // namespace media_audio
