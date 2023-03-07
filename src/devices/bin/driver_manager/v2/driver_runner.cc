// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/driver_runner.h"

#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.host/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <fidl/fuchsia.process/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/driver/component/cpp/start_args.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/fit/defer.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/status.h>

#include <forward_list>
#include <queue>
#include <random>
#include <stack>
#include <unordered_set>

#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/lib/storage/vfs/cpp/service.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace fdh = fuchsia_driver_host;
namespace fdi = fuchsia_driver_index;
namespace fio = fuchsia_io;
namespace fprocess = fuchsia_process;
namespace frunner = fuchsia_component_runner;
namespace fcomponent = fuchsia_component;
namespace fdecl = fuchsia_component_decl;

using InspectStack = std::stack<std::pair<inspect::Node*, const dfv2::Node*>>;

namespace dfv2 {

namespace {

constexpr uint32_t kTokenId = PA_HND(PA_USER0, 0);
constexpr auto kBootScheme = "fuchsia-boot://";
constexpr std::string_view kRootDeviceName = "dev";

template <typename R, typename F>
std::optional<R> VisitOffer(fdecl::wire::Offer& offer, F apply) {
  // Note, we access each field of the union as mutable, so that `apply` can
  // modify the field if necessary.
  switch (offer.Which()) {
    case fdecl::wire::Offer::Tag::kService:
      return apply(offer.service());
    case fdecl::wire::Offer::Tag::kProtocol:
      return apply(offer.protocol());
    case fdecl::wire::Offer::Tag::kDirectory:
      return apply(offer.directory());
    case fdecl::wire::Offer::Tag::kStorage:
      return apply(offer.storage());
    case fdecl::wire::Offer::Tag::kRunner:
      return apply(offer.runner());
    case fdecl::wire::Offer::Tag::kResolver:
      return apply(offer.resolver());
    case fdecl::wire::Offer::Tag::kEvent:
      return apply(offer.event());
    case fdecl::wire::Offer::Tag::kEventStream:
      return apply(offer.event_stream());
    default:
      return {};
  }
}

void InspectNode(inspect::Inspector& inspector, InspectStack& stack) {
  const auto inspect_decl = [](auto& decl) -> std::string_view {
    if (decl.has_target_name()) {
      return decl.target_name().get();
    }
    if (decl.has_source_name()) {
      return decl.source_name().get();
    }
    return "<missing>";
  };

  std::forward_list<inspect::Node> roots;
  std::unordered_set<const Node*> unique_nodes;
  while (!stack.empty()) {
    // Pop the current root and node to operate on.
    auto [root, node] = stack.top();
    stack.pop();

    auto [_, inserted] = unique_nodes.insert(node);
    if (!inserted) {
      // Only insert unique nodes from the DAG.
      continue;
    }

    // Populate root with data from node.
    if (auto offers = node->offers(); !offers.empty()) {
      std::vector<std::string_view> strings;
      for (auto& offer : offers) {
        auto string = VisitOffer<std::string_view>(offer, inspect_decl);
        strings.push_back(string.value_or("unknown"));
      }
      root->RecordString("offers", fxl::JoinStrings(strings, ", "));
    }
    if (auto symbols = node->symbols(); !symbols.empty()) {
      std::vector<std::string_view> strings;
      for (auto& symbol : symbols) {
        strings.push_back(symbol.name().get());
      }
      root->RecordString("symbols", fxl::JoinStrings(strings, ", "));
    }
    std::string driver_string = node->driver_url();
    root->RecordString("driver", driver_string);

    // Push children of this node onto the stack. We do this in reverse order to
    // ensure the children are handled in order, from first to last.
    auto& children = node->children();
    for (auto child = children.rbegin(), end = children.rend(); child != end; ++child) {
      auto& name = (*child)->name();
      auto& root_for_child = roots.emplace_front(root->CreateChild(name));
      stack.emplace(&root_for_child, child->get());
    }
  }

  // Store all of the roots in the inspector.
  for (auto& root : roots) {
    inspector.GetRoot().Record(std::move(root));
  }
}

fidl::StringView CollectionName(Collection collection) {
  switch (collection) {
    case Collection::kNone:
      return {};
    case Collection::kHost:
      return "driver-hosts";
    case Collection::kBoot:
      return "boot-drivers";
    case Collection::kPackage:
      return "pkg-drivers";
    case Collection::kUniversePackage:
      return "universe-pkg-drivers";
  }
}

}  // namespace

DriverRunner::DriverRunner(fidl::ClientEnd<fcomponent::Realm> realm,
                           fidl::ClientEnd<fdi::DriverIndex> driver_index,
                           inspect::Inspector& inspector,
                           LoaderServiceFactory loader_service_factory,
                           async_dispatcher_t* dispatcher)
    : realm_(std::move(realm), dispatcher),
      driver_index_(std::move(driver_index), dispatcher),
      loader_service_factory_(std::move(loader_service_factory)),
      dispatcher_(dispatcher),
      root_node_(std::make_shared<Node>(kRootDeviceName, std::vector<Node*>{}, this, dispatcher)),
      composite_device_manager_(this, dispatcher, [this]() { this->TryBindAllOrphansUntracked(); }),
      composite_node_spec_manager_(this) {
  inspector.GetRoot().CreateLazyNode(
      "driver_runner", [this] { return Inspect(); }, &inspector);

  // Pick a non-zero starting id so that folks cannot rely on the driver host process names being
  // stable.
  std::random_device rd;
  std::mt19937 gen(rd());
  std::uniform_int_distribution<> distrib(0, 1000);
  next_driver_host_id_ = distrib(gen);
}

void DriverRunner::BindNodesForCompositeNodeSpec() { TryBindAllOrphansUntracked(); }

void DriverRunner::AddSpec(AddSpecRequestView request, AddSpecCompleter::Sync& completer) {
  if (!request->has_name() || !request->has_parents()) {
    completer.Reply(fit::error(fdf::CompositeNodeSpecError::kMissingArgs));
    return;
  }

  if (request->parents().empty()) {
    completer.Reply(fit::error(fdf::CompositeNodeSpecError::kEmptyNodes));
    return;
  }

  auto spec = std::make_unique<CompositeNodeSpecV2>(
      CompositeNodeSpecCreateInfo{
          .name = std::string(request->name().get()),
          .size = request->parents().count(),
      },
      dispatcher_, this);
  completer.Reply(composite_node_spec_manager_.AddSpec(*request, std::move(spec)));
}

void DriverRunner::AddSpecToDriverIndex(fuchsia_driver_framework::wire::CompositeNodeSpec group,
                                        AddToIndexCallback callback) {
  driver_index_->AddCompositeNodeSpec(group).Then(
      [callback = std::move(callback)](
          fidl::WireUnownedResult<fdi::DriverIndex::AddCompositeNodeSpec>& result) mutable {
        if (!result.ok()) {
          LOGF(ERROR, "DriverIndex::AddCompositeNodeSpec failed %d", result.status());
          callback(zx::error(result.status()));
          return;
        }

        if (result->is_error()) {
          callback(result->take_error());
          return;
        }

        callback(zx::ok(fidl::ToNatural(*result->value())));
      });
}

// TODO(fxb/121999): Add information for composite node specs.
fpromise::promise<inspect::Inspector> DriverRunner::Inspect() const {
  // Create our inspector.
  // The default maximum size was too small, and so this is double the default size.
  // If a device loads too much inspect data, this can be increased in the future.
  inspect::Inspector inspector(inspect::InspectSettings{.maximum_size = 2 * 256 * 1024});

  // Make the device tree inspect nodes.
  auto device_tree = inspector.GetRoot().CreateChild("node_topology");
  auto root = device_tree.CreateChild(root_node_->name());
  InspectStack stack{{std::make_pair(&root, root_node_.get())}};
  InspectNode(inspector, stack);
  device_tree.Record(std::move(root));
  inspector.GetRoot().Record(std::move(device_tree));

  // Make the orphaned devices inspect nodes.
  auto orphans = inspector.GetRoot().CreateChild("orphan_nodes");
  for (size_t i = 0; i < orphaned_nodes_.size(); i++) {
    if (auto node = orphaned_nodes_[i].lock()) {
      orphans.RecordString(std::to_string(i), node->MakeComponentMoniker());
    }
  }

  inspector.GetRoot().Record(std::move(orphans));

  auto dfv1_composites = inspector.GetRoot().CreateChild("dfv1_composites");
  composite_device_manager_.Inspect(dfv1_composites);
  inspector.GetRoot().Record(std::move(dfv1_composites));

  return fpromise::make_ok_promise(inspector);
}

size_t DriverRunner::NumOrphanedNodes() const { return orphaned_nodes_.size(); }

void DriverRunner::PublishComponentRunner(component::OutgoingDirectory& outgoing) {
  auto result = outgoing.AddUnmanagedProtocol<frunner::ComponentRunner>(
      runner_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  ZX_ASSERT(result.is_ok());

  composite_device_manager_.Publish(outgoing);
}

void DriverRunner::PublishCompositeNodeManager(component::OutgoingDirectory& outgoing) {
  auto result = outgoing.AddUnmanagedProtocol<fdf::CompositeNodeManager>(
      manager_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  ZX_ASSERT(result.is_ok());
}

zx::result<> DriverRunner::StartRootDriver(std::string_view url) {
  return StartDriver(*root_node_, url, fdi::DriverPackageType::kBase);
}

std::shared_ptr<Node> DriverRunner::root_node() { return root_node_; }

void DriverRunner::ScheduleBaseDriversBinding() {
  driver_index_->WaitForBaseDrivers().Then(
      [this](fidl::WireUnownedResult<fdi::DriverIndex::WaitForBaseDrivers>& result) mutable {
        if (!result.ok()) {
          // It's possible in tests that the test can finish before WaitForBaseDrivers
          // finishes.
          if (result.status() == ZX_ERR_PEER_CLOSED) {
            LOGF(WARNING, "Connection to DriverIndex closed during WaitForBaseDrivers.");
          } else {
            LOGF(ERROR, "DriverIndex::WaitForBaseDrivers failed with: %s",
                 result.error().FormatDescription().c_str());
          }
          return;
        }

        TryBindAllOrphansUntracked();
      });
}

void DriverRunner::TryBindAllOrphans(NodeBindingInfoResultCallback result_callback) {
  // Clear our stored vector of orphaned nodes, we will repopulate it with the
  // new orphans.
  std::vector<std::weak_ptr<Node>> orphaned_nodes = std::move(orphaned_nodes_);
  orphaned_nodes_ = {};

  std::shared_ptr<BindResultTracker> tracker =
      std::make_shared<BindResultTracker>(orphaned_nodes.size(), std::move(result_callback));

  for (auto& weak_node : orphaned_nodes) {
    auto node = weak_node.lock();
    if (!node) {
      tracker->ReportNoBind();
      continue;
    }

    Bind(*node, tracker);
  }
}

void DriverRunner::TryBindAllOrphansUntracked() {
  NodeBindingInfoResultCallback empty_callback =
      [](fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>) {};
  TryBindAllOrphans(std::move(empty_callback));
}

zx::result<> DriverRunner::StartDriver(Node& node, std::string_view url,
                                       fdi::DriverPackageType package_type) {
  zx::event token;
  zx_status_t status = zx::event::create(0, &token);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  zx_info_handle_basic_t info{};
  status = token.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  // TODO(fxb/98474) Stop doing the url prefix check and just rely on the package_type.
  auto collection = cpp20::starts_with(url, kBootScheme) ? Collection::kBoot : Collection::kPackage;
  if (package_type == fdi::DriverPackageType::kUniverse) {
    collection = Collection::kUniversePackage;
  }
  node.set_collection(collection);
  auto create = CreateComponent(node.MakeComponentMoniker(), collection, std::string(url),
                                {.node = &node, .token = std::move(token)});
  if (create.is_error()) {
    return create.take_error();
  }
  driver_args_.emplace(info.koid, node);
  return zx::ok();
}

void DriverRunner::DestroyDriverComponent(dfv2::Node& node,
                                          DestroyDriverComponentCallback callback) {
  auto name = node.MakeComponentMoniker();
  fdecl::wire::ChildRef child_ref{
      .name = fidl::StringView::FromExternal(name),
      .collection = CollectionName(node.collection()),
  };
  realm_->DestroyChild(child_ref).Then(std::move(callback));
}

void DriverRunner::Start(StartRequestView request, StartCompleter::Sync& completer) {
  auto url = request->start_info.resolved_url().get();

  // When we start a driver, we associate an unforgeable token (the KOID of a
  // zx::event) with the start request, through the use of the numbered_handles
  // field. We do this so:
  //  1. We can securely validate the origin of the request
  //  2. We avoid collisions that can occur when relying on the package URL
  //  3. We avoid relying on the resolved URL matching the package URL
  if (!request->start_info.has_numbered_handles()) {
    LOGF(ERROR, "Failed to start driver '%.*s', invalid request for driver",
         static_cast<int>(url.size()), url.data());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& handles = request->start_info.numbered_handles();
  if (handles.count() != 1 || !handles[0].handle || handles[0].id != kTokenId) {
    LOGF(ERROR, "Failed to start driver '%.*s', invalid request for driver",
         static_cast<int>(url.size()), url.data());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  zx_info_handle_basic_t info{};
  zx_status_t status =
      handles[0].handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto it = driver_args_.find(info.koid);
  if (it == driver_args_.end()) {
    LOGF(ERROR, "Failed to start driver '%.*s', unknown request for driver",
         static_cast<int>(url.size()), url.data());
    completer.Close(ZX_ERR_UNAVAILABLE);
    return;
  }
  auto& [_, node] = *it;
  driver_args_.erase(it);

  if (zx::result status = node.StartDriver(request->start_info, std::move(request->controller));
      status.is_error()) {
    completer.Close(status.error_value());
  }
}

void DriverRunner::Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) {
  // Check the DFv1 composites first, and don't bind to others if they match.
  if (composite_device_manager_.BindNode(node.shared_from_this())) {
    return;
  }

  auto match_callback = [this, weak_node = node.weak_from_this(), result_tracker](
                            fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result) {
    auto shared_node = weak_node.lock();

    auto report_no_bind = fit::defer([&result_tracker]() {
      if (result_tracker) {
        result_tracker->ReportNoBind();
      }
    });

    if (!shared_node) {
      LOGF(WARNING, "Node was freed before it could be bound");
      return;
    }

    Node& node = *shared_node;
    auto driver_node = &node;
    auto orphaned = [this, &driver_node] {
      orphaned_nodes_.push_back(driver_node->weak_from_this());
    };

    if (!result.ok()) {
      orphaned();
      LOGF(ERROR, "Failed to call match Node '%s': %s", node.name().c_str(),
           result.error().FormatDescription().data());
      return;
    }

    if (result->is_error()) {
      orphaned();
      // Log the failed MatchDriver only if we are not tracking the results with a tracker
      // or if the error is not a ZX_ERR_NOT_FOUND error (meaning it could not find a driver).
      // When we have a tracker, the bind is happening for all the orphan nodes and the
      // not found errors get very noisy.
      zx_status_t match_error = result->error_value();
      if (!result_tracker || match_error != ZX_ERR_NOT_FOUND) {
        LOGF(WARNING, "Failed to match Node '%s': %s", driver_node->name().c_str(),
             zx_status_get_string(match_error));
      }

      return;
    }

    auto& matched_driver = result->value()->driver;
    if (!matched_driver.is_driver() && !matched_driver.is_parent_spec()) {
      orphaned();
      LOGF(WARNING,
           "Failed to match Node '%s', the MatchedDriver is not a normal driver or a "
           "parent spec.",
           driver_node->name().c_str());
      return;
    }

    if (matched_driver.is_parent_spec() && !matched_driver.parent_spec().has_specs()) {
      orphaned();
      LOGF(
          WARNING,
          "Failed to match Node '%s', the MatchedDriver is missing the composite node specs in the "
          "parent spec.",
          driver_node->name().c_str());
      return;
    }

    // If this is a composite node spec match, bind the node into the spec. If the spec is
    // completed, use the returned node and driver to start the driver.
    fdi::wire::MatchedDriverInfo driver_info;
    if (matched_driver.is_parent_spec()) {
      auto node_groups = matched_driver.parent_spec();
      auto result =
          composite_node_spec_manager_.BindParentSpec(node_groups, driver_node->weak_from_this());
      if (result.is_error()) {
        orphaned();
        LOGF(ERROR, "Failed to bind node '%s' to any of the matched parent specs.",
             driver_node->name().c_str());
        return;
      }

      auto composite_node_and_driver = result.value();

      // If it doesn't have a value but there was no error it just means the node was added
      // to a composite node spec but the spec is still incomplete.
      if (!composite_node_and_driver.has_value()) {
        return;
      }

      auto composite_node = std::get<std::weak_ptr<dfv2::Node>>(composite_node_and_driver->node);
      auto locked_composite_node = composite_node.lock();
      ZX_ASSERT(locked_composite_node);
      driver_node = locked_composite_node.get();
      driver_info = composite_node_and_driver->driver;
    } else {
      driver_info = matched_driver.driver();
    }

    if (!driver_info.has_url()) {
      orphaned();
      LOGF(ERROR, "Failed to match Node '%s', the driver URL is missing",
           driver_node->name().c_str());
      return;
    }

    auto pkg_type =
        driver_info.has_package_type() ? driver_info.package_type() : fdi::DriverPackageType::kBase;
    auto start_result = StartDriver(*driver_node, driver_info.url().get(), pkg_type);
    if (start_result.is_error()) {
      orphaned();
      LOGF(ERROR, "Failed to start driver '%s': %s", driver_node->name().c_str(),
           zx_status_get_string(start_result.error_value()));
      return;
    }

    driver_node->OnBind();
    report_no_bind.cancel();
    if (result_tracker) {
      result_tracker->ReportSuccessfulBind(driver_node->MakeComponentMoniker(),
                                           driver_info.url().get());
    }
  };
  fidl::Arena<> arena;
  driver_index_->MatchDriver(node.CreateMatchArgs(arena)).Then(std::move(match_callback));
}

zx::result<DriverHost*> DriverRunner::CreateDriverHost() {
  zx::result endpoints = fidl::CreateEndpoints<fio::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto name = "driver-host-" + std::to_string(next_driver_host_id_++);
  auto create = CreateComponent(name, Collection::kHost, "#meta/driver_host2.cm",
                                {.exposed_dir = std::move(endpoints->server)});
  if (create.is_error()) {
    return create.take_error();
  }

  auto client_end = component::ConnectAt<fdh::DriverHost>(endpoints->client);
  if (client_end.is_error()) {
    LOGF(ERROR, "Failed to connect to service '%s': %s",
         fidl::DiscoverableProtocolName<fdh::DriverHost>, client_end.status_string());
    return client_end.take_error();
  }

  auto loader_service_client = loader_service_factory_();
  if (loader_service_client.is_error()) {
    LOGF(ERROR, "Failed to connect to service fuchsia.ldsvc/Loader: %s",
         loader_service_client.status_string());
    return loader_service_client.take_error();
  }

  auto driver_host =
      std::make_unique<DriverHostComponent>(std::move(*client_end), dispatcher_, &driver_hosts_);
  auto result = driver_host->InstallLoader(std::move(*loader_service_client));
  if (result.is_error()) {
    LOGF(ERROR, "Failed to install loader service: %s", result.status_string());
    return result.take_error();
  }

  auto driver_host_ptr = driver_host.get();
  driver_hosts_.push_back(std::move(driver_host));

  return zx::ok(driver_host_ptr);
}

zx::result<> DriverRunner::CreateComponent(std::string name, Collection collection, std::string url,
                                           CreateComponentOpts opts) {
  fidl::Arena arena;
  auto child_decl_builder = fdecl::wire::Child::Builder(arena);
  child_decl_builder.name(fidl::StringView::FromExternal(name))
      .url(fidl::StringView::FromExternal(url))
      .startup(fdecl::wire::StartupMode::kLazy);
  auto child_args_builder = fcomponent::wire::CreateChildArgs::Builder(arena);
  if (opts.node != nullptr) {
    child_args_builder.dynamic_offers(opts.node->offers());
  }
  fprocess::wire::HandleInfo handle_info;
  if (opts.token) {
    handle_info = {
        .handle = std::move(opts.token),
        .id = kTokenId,
    };
    child_args_builder.numbered_handles(
        fidl::VectorView<fprocess::wire::HandleInfo>::FromExternal(&handle_info, 1));
  }
  auto open_callback = [name,
                        url](fidl::WireUnownedResult<fcomponent::Realm::OpenExposedDir>& result) {
    if (!result.ok()) {
      LOGF(ERROR, "Failed to open exposed directory for component '%s' (%s): %s", name.data(),
           url.data(), result.FormatDescription().data());
      return;
    }
    if (result->is_error()) {
      LOGF(ERROR, "Failed to open exposed directory for component '%s' (%s): %u", name.data(),
           url.data(), result->error_value());
    }
  };
  auto create_callback =
      [this, name, url, collection, exposed_dir = std::move(opts.exposed_dir),
       open_callback = std::move(open_callback)](
          fidl::WireUnownedResult<fcomponent::Realm::CreateChild>& result) mutable {
        if (!result.ok()) {
          LOGF(ERROR, "Failed to create component '%s' (%s): %s", name.data(), url.data(),
               result.error().FormatDescription().data());
          return;
        }
        if (result->is_error()) {
          LOGF(ERROR, "Failed to create component '%s' (%s): %u", name.data(), url.data(),
               result->error_value());
          return;
        }
        if (exposed_dir) {
          fdecl::wire::ChildRef child_ref{
              .name = fidl::StringView::FromExternal(name),
              .collection = CollectionName(collection),
          };
          realm_->OpenExposedDir(child_ref, std::move(exposed_dir))
              .ThenExactlyOnce(std::move(open_callback));
        }
      };
  realm_
      ->CreateChild(fdecl::wire::CollectionRef{.name = CollectionName(collection)},
                    child_decl_builder.Build(), child_args_builder.Build())
      .Then(std::move(create_callback));
  return zx::ok();
}

}  // namespace dfv2
