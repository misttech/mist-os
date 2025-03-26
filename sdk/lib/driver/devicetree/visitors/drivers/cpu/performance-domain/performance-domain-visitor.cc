// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "performance-domain-visitor.h"

#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <cstdint>
#include <regex>
#include <vector>

#include "soc/aml-common/aml-cpu-metadata.h"

namespace performance_domain_visitor_dt {

PerformanceDomainVisitor::PerformanceDomainVisitor() {
  fdf_devicetree::Properties domain_properties = {};
  domain_properties.emplace_back(std::make_unique<fdf_devicetree::Uint32Property>(kDomainID, true));
  domain_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32ArrayProperty>(kCpus, true));
  domain_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32Property>(kRelativePerformance, true));
  domain_properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kOperatingPoints, 0u, true));

  performance_domain_parser_ =
      std::make_unique<fdf_devicetree::PropertyParser>(std::move(domain_properties));

  fdf_devicetree::Properties opp_properties = {};
  opp_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint64Property>(kOperatingFrequency, true));
  opp_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32Property>(kOperatingMicrovolt, true));

  opp_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(opp_properties));
}

zx::result<> PerformanceDomainVisitor::Visit(fdf_devicetree::Node& node,
                                             const devicetree::PropertyDecoder& decoder) {
  if (!IsMatch(node)) {
    return zx::ok();
  }

  auto device_node = node.parent().GetNode();

  std::vector<amlogic_cpu::perf_domain_t> performance_domains;
  std::vector<amlogic_cpu::operating_point_t> opp_tables;

  for (auto& child : node.children()) {
    auto performance_domain = ParsePerformanceDomain(*child.GetNode(), opp_tables);
    if (performance_domain.is_error()) {
      return performance_domain.take_error();
    }

    FDF_LOG(DEBUG, "Added performance domain '%s' to node '%s'.", performance_domain->name,
            device_node->name().c_str());
    performance_domains.push_back(*performance_domain);
  }

  if (!performance_domains.empty()) {
    fuchsia_hardware_platform_bus::Metadata perf_domains_metadata = {{
        .id = std::to_string(DEVICE_METADATA_AML_PERF_DOMAINS),
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(performance_domains.data()),
            reinterpret_cast<const uint8_t*>(performance_domains.data()) +
                (performance_domains.size() * sizeof(amlogic_cpu::perf_domain_t))),
    }};
    device_node->AddMetadata(std::move(perf_domains_metadata));

    fuchsia_hardware_platform_bus::Metadata opp_metadata = {{
        .id = std::to_string(DEVICE_METADATA_AML_OP_POINTS),
        .data =
            std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(opp_tables.data()),
                                 reinterpret_cast<const uint8_t*>(opp_tables.data()) +
                                     (opp_tables.size() * sizeof(amlogic_cpu::operating_point_t))),
    }};

    device_node->AddMetadata(std::move(opp_metadata));
  }

  return zx::ok();
}

bool PerformanceDomainVisitor::IsMatch(fdf_devicetree::Node& node) {
  return node.parent() && node.name() == "performance-domains";
}

std::optional<std::string> PerformanceDomainVisitor::GetDomainName(const std::string& node_name) {
  std::smatch match;
  std::regex name_regex("(^[a-zA-Z0-9-]*)-domain$");
  if (std::regex_search(node_name, match, name_regex) && match.size() == 2) {
    return match[1];
  }
  return std::nullopt;
}

zx::result<amlogic_cpu::perf_domain_t> PerformanceDomainVisitor::ParsePerformanceDomain(
    fdf_devicetree::Node& node, std::vector<amlogic_cpu::operating_point_t>& opp_tables) {
  auto parser_output = performance_domain_parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Performance domain visitor failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  amlogic_cpu::perf_domain_t performance_domain;
  performance_domain.id = parser_output->at(kDomainID)[0].AsUint32().value();
  performance_domain.relative_performance =
      static_cast<uint8_t>(parser_output->at(kRelativePerformance)[0].AsUint32().value());
  performance_domain.core_count = static_cast<uint32_t>(parser_output->at(kCpus).size());

  auto opp_table =
      ParseOppTable(*parser_output->at(kOperatingPoints)[0].AsReference()->first.GetNode(),
                    performance_domain.id);
  if (opp_table.is_error()) {
    return opp_table.take_error();
  }
  opp_tables.insert(opp_tables.end(), opp_table->begin(), opp_table->end());

  auto domain_name = GetDomainName(node.name());

  if (!domain_name) {
    FDF_LOG(ERROR, "Performance domain has invalid node name '%s'.", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  strncpy(performance_domain.name, domain_name->c_str(), sizeof(performance_domain.name));
  return zx::ok(performance_domain);
}

zx::result<std::vector<amlogic_cpu::operating_point_t>> PerformanceDomainVisitor::ParseOppTable(
    fdf_devicetree::Node& node, uint32_t domain_id) {
  std::vector<amlogic_cpu::operating_point_t> opp_table;
  for (auto& child : node.children()) {
    auto parser_output = opp_parser_->Parse(*child.GetNode());
    if (parser_output.is_error()) {
      FDF_LOG(ERROR, "Operating point visitor failed for node '%s' : %s", child.name().c_str(),
              parser_output.status_string());
      return parser_output.take_error();
    }

    opp_table.push_back({
        .freq_hz =
            static_cast<uint32_t>(parser_output->at(kOperatingFrequency)[0].AsUint64().value()),
        .volt_uv = parser_output->at(kOperatingMicrovolt)[0].AsUint32().value(),
        .pd_id = domain_id,
    });
  }
  return zx::ok(opp_table);
}

}  // namespace performance_domain_visitor_dt

REGISTER_DEVICETREE_VISITOR(performance_domain_visitor_dt::PerformanceDomainVisitor);
