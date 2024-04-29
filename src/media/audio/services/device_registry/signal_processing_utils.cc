// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/signal_processing_utils.h"

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <lib/syslog/cpp/macros.h>

namespace media_audio {

namespace fhasp = fuchsia_hardware_audio_signalprocessing;

std::unordered_set<ElementId> dai_endpoints(
    const std::unordered_map<ElementId, ElementRecord>& element_map) {
  std::unordered_set<ElementId> dai_endpoints;
  for (const auto& element_entry_pair : element_map) {
    if (element_entry_pair.second.element.type() == fhasp::ElementType::kEndpoint &&
        element_entry_pair.second.element.type_specific()->endpoint()->type() ==
            fhasp::EndpointType::kDaiInterconnect) {
      dai_endpoints.insert(element_entry_pair.first);
    }
  }
  return dai_endpoints;
}

std::unordered_set<ElementId> ring_buffer_endpoints(
    const std::unordered_map<ElementId, ElementRecord>& element_map) {
  std::unordered_set<ElementId> ring_buffer_endpoints;
  for (const auto& element_entry_pair : element_map) {
    if (element_entry_pair.second.element.type() == fhasp::ElementType::kEndpoint &&
        element_entry_pair.second.element.type_specific()->endpoint()->type() ==
            fhasp::EndpointType::kRingBuffer) {
      ring_buffer_endpoints.insert(element_entry_pair.first);
    }
  }
  return ring_buffer_endpoints;
}

// This maps ElementId->ElementRecord but populates only the Element portion of the ElementRecord.
std::unordered_map<ElementId, ElementRecord> MapElements(
    const std::vector<fhasp::Element>& elements) {
  auto element_map = std::unordered_map<ElementId, ElementRecord>{};

  for (const auto& element : elements) {
    if (!element.id().has_value()) {
      FX_LOGS(WARNING) << "invalid element_id";
      return {};
    }
    auto element_insertion = element_map.insert({*element.id(), ElementRecord{.element = element}});
    if (!element_insertion.second) {
      FX_LOGS(WARNING) << "duplicate element_id " << *element.id();
      return {};
    }
  }
  return element_map;
}

// Returns empty map if any topology_id values are duplicated.
std::unordered_map<TopologyId, std::vector<fhasp::EdgePair>> MapTopologies(
    const std::vector<fhasp::Topology>& topologies) {
  auto topology_map = std::unordered_map<TopologyId, std::vector<fhasp::EdgePair>>{};

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
