// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "mailbox-visitor.h"

#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/registration.h>

#include <bind/fuchsia/hardware/mailbox/cpp/bind.h>
#include <bind/fuchsia/mailbox/cpp/bind.h>

namespace {

constexpr char kMailboxesProperty[] = "mboxes";
constexpr char kMailboxNamesProperty[] = "mbox-names";
constexpr char kMailboxCellsProperty[] = "#mbox-cells";

zx::result<std::pair<fdf_devicetree::ReferenceNode, uint32_t>> ParseChannel(
    const fdf_devicetree::PropertyValue& property) {
  const auto reference = property.AsReference();
  if (!reference || !reference->first) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const std::optional<uint32_t> channel =
      fdf_devicetree::PropertyValue(reference->second).AsUint32();
  if (!channel) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(std::pair{reference->first, *channel});
}

}  // namespace

namespace mailbox_dt {

MailboxVisitor::MailboxVisitor() {
  fdf_devicetree::Properties mailbox_properties = {};
  mailbox_properties.emplace_back(std::make_unique<fdf_devicetree::ReferenceProperty>(
      kMailboxesProperty, 1u, /*required=*/false));
  mailbox_parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(mailbox_properties));
}

zx::result<> MailboxVisitor::Visit(fdf_devicetree::Node& node,
                                   const devicetree::PropertyDecoder& decoder) {
  zx::result<fdf_devicetree::PropertyValues> properties = mailbox_parser_->Parse(node);
  if (properties.is_error()) {
    FDF_LOG(ERROR, "Failed to parse node \"%s\"", node.name().c_str());
    return properties.take_error();
  }
  if (!properties->contains(kMailboxesProperty)) {
    return zx::ok();
  }

  std::vector<std::string_view> channel_names;
  if (auto channel_names_property = node.properties().find(kMailboxNamesProperty);
      channel_names_property != node.properties().end()) {
    std::optional channel_names_list = channel_names_property->second.AsStringList();
    if (!channel_names_list) {
      FDF_LOG(ERROR, "Failed to parse mbox-names property for node \"%s\"", node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    channel_names = {channel_names_list->begin(), channel_names_list->end()};
  }

  const std::vector<fdf_devicetree::PropertyValue>& channels = (*properties)[kMailboxesProperty];
  if (!channel_names.empty() && channels.size() != channel_names.size()) {
    FDF_LOG(ERROR, "mboxes and mbox-names mismatch for node \"%s\"", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto channel_name_it = channel_names.cbegin();

  std::map<uint32_t, uint32_t> controller_ids;
  uint32_t current_controller_id = 0;

  for (const auto& property : channels) {
    zx::result<std::pair<fdf_devicetree::ReferenceNode, uint32_t>> channel = ParseChannel(property);
    if (channel.is_error()) {
      FDF_LOG(ERROR, "Failed to parse mailbox channel reference for node \"%s\"",
              node.name().c_str());
      return channel.take_error();
    }

    // Map the node ID to a controller index to be used only in the node properties. This way
    // clients with multiple mailbox parents don't need to know the actual controller ID.
    if (!controller_ids.contains(channel->first.id())) {
      controller_ids[channel->first.id()] = current_controller_id++;
    }
    const uint32_t local_controller_id = controller_ids[channel->first.id()];

    controller_info_[channel->first.id()].push_back({{.channel = channel->second}});

    fuchsia_driver_framework::ParentSpec parent_spec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bind_fuchsia_hardware_mailbox::SERVICE,
                                        bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CONTROLLER_ID, channel->first.id()),
                fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CHANNEL, channel->second),
            },
        .properties =
            {
                fdf::MakeProperty(bind_fuchsia_hardware_mailbox::SERVICE,
                                  bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
                fdf::MakeProperty(bind_fuchsia_mailbox::CONTROLLER_ID, local_controller_id),
                fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL, channel->second),
            },
    }};

    if (channel_name_it != channel_names.cend()) {
      parent_spec.properties().push_back(
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL_NAME, *channel_name_it));
      *channel_name_it++;
    }

    node.AddNodeSpec(parent_spec);
  }

  return zx::ok();
}

zx::result<> MailboxVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  const uint32_t controller_id = node.id();

  const auto channels = controller_info_.find(controller_id);
  if (channels == controller_info_.end()) {
    return zx::ok();  // Not a mailbox controller or no channels -- ignore.
  }

  auto mbox_cells = node.properties().find(kMailboxCellsProperty);
  if (mbox_cells == node.properties().end() || !mbox_cells->second.AsUint32()) {
    FDF_LOG(ERROR, "Missing #mbox-cells property for node \"%s\"", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (*mbox_cells->second.AsUint32() != 1) {
    FDF_LOG(ERROR, "Invalid #mbox-cells property for node \"%s\"", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fuchsia_hardware_mailbox::ControllerInfo controller{{.id = controller_id}};
  if (channels != controller_info_.end()) {
    controller.channels() = channels->second;
  }

  fit::result<fidl::Error, std::vector<uint8_t>> metadata = fidl::Persist(controller);
  if (metadata.is_error()) {
    FDF_LOG(ERROR, "Failed to persist mailbox controller metadata: %s",
            metadata.error_value().FormatDescription().c_str());
    return zx::error(metadata.error_value().status());
  }

  fuchsia_hardware_platform_bus::Metadata pbus_metadata{{
      .id = std::to_string(fuchsia_hardware_mailbox::kControllerInfoMetadataType),
      .data = *std::move(metadata),
  }};
  node.AddMetadata(std::move(pbus_metadata));

  return zx::ok();
}

}  // namespace mailbox_dt

REGISTER_DEVICETREE_VISITOR(mailbox_dt::MailboxVisitor);
