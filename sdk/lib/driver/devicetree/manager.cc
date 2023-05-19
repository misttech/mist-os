// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/manager.h"

#include <fidl/fuchsia.boot/cpp/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/result.h>

#include "sdk/lib/driver/devicetree/node.h"
#include "sdk/lib/driver/devicetree/visitors/default.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace fdf_devicetree {

constexpr const char kPhandleProp[] = "phandle";

zx::result<Manager> Manager::CreateFromNamespace(fdf::Namespace& ns, fdf::Logger* logger) {
  zx::result client_end = ns.Connect<fuchsia_boot::Items>();
  if (client_end.is_error()) {
    FDF_LOGL(ERROR, *logger, "Failed to connect to fuchsia.boot.Items: %s",
             client_end.status_string());
    return client_end.take_error();
  }

  fidl::WireSyncClient client(std::move(client_end.value()));
  fidl::WireResult result = client->Get2(ZBI_TYPE_DEVICETREE, {});
  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger, "Failed to send get2 request: %s", result.FormatDescription().data());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger, "Failed to get2: %s", zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }

  fidl::VectorView items = result->value()->retrieved_items;
  if (items.count() != 1) {
    FDF_LOGL(ERROR, *logger, "Found wrong number of devicetrees: wanted 1, got %zu", items.count());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fuchsia_boot::wire::RetrievedItems& dt = result->value()->retrieved_items[0];
  std::vector<uint8_t> data;
  data.resize(dt.length);

  zx_status_t status = dt.payload.read(data.data(), 0, dt.length);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, *logger, "Failed to read %u bytes from the devicetree: %s", dt.length,
             zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(Manager(std::move(data), logger));
}

zx::result<> Manager::Discover() {
  tree_.Walk(
      [&, this](const devicetree::NodePath& path, const devicetree::PropertyDecoder& decoder) {
        FDF_LOG(DEBUG, "Found node - %s", path.back().data());

        // Create a node.
        const devicetree::Properties& properties = decoder.properties();
        auto node = std::make_unique<Node>(path.back(), properties, node_id_++, logger_);
        Node* ptr = node.get();
        nodes_publish_order_.emplace_back(std::move(node));
        FDF_LOG(DEBUG, "Node[%d] - %s added for publishing", node_id_, path.back().data());

        // If the node has a phandle, record it.
        const auto phandle_prop = ptr->properties().find(kPhandleProp);
        if (phandle_prop != ptr->properties().end() &&
            phandle_prop->second.AsUint32() != std::nullopt) {
          nodes_by_phandle_.emplace(phandle_prop->second.AsUint32().value(), ptr);
        } else if (phandle_prop != ptr->properties().end()) {
          FDF_SLOG(WARNING, "Node has invalid phandle property", KV("node_name", node->name()),
                   KV("prop_len", phandle_prop->second.AsBytes().size()));
        }

        return true;
      });
  return zx::ok();
}

zx::result<> Manager::Walk(Visitor& visitor) {
  for (auto& node : nodes_publish_order_) {
    auto status = visitor.Visit(*node.get());
    if (status.is_error()) {
      return status.take_error();
    }
  }
  return zx::ok();
}

void Manager::DefaultVisit() {
  DefaultVisitor visitor(logger_);
  [[maybe_unused]] auto unused = Walk(visitor);
}

zx::result<> Manager::PublishDevices(
    fdf::ClientEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus,
    fidl::ClientEnd<fuchsia_driver_framework::CompositeNodeManager> mgr) {
  auto pbus_client = fdf::WireSyncClient(std::move(pbus));
  auto mgr_client = fidl::SyncClient(std::move(mgr));

  for (auto& node : nodes_publish_order_) {
    zx::result<> status = node->Publish(pbus_client, mgr_client);
    if (status.is_error()) {
      return status.take_error();
    }
  }

  return zx::ok();
}

}  // namespace fdf_devicetree
