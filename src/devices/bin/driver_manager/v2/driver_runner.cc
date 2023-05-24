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

#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/lib/storage/vfs/cpp/service.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace fdh = fuchsia_driver_host;
namespace fdd = fuchsia_driver_development;
namespace fdi = fuchsia_driver_index;
namespace fio = fuchsia_io;
namespace fprocess = fuchsia_process;
namespace frunner = fuchsia_component_runner;
namespace fcomponent = fuchsia_component;
namespace fdecl = fuchsia_component_decl;

using InspectStack = std::stack<std::pair<inspect::Node*, const dfv2::Node*>>;

namespace dfv2 {

namespace {

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
    case Collection::kBoot:
      return "boot-drivers";
    case Collection::kPackage:
      return "pkg-drivers";
    case Collection::kFullPackage:
      return "full-pkg-drivers";
  }
}
Collection ToCollection(fdi::DriverPackageType package) {
  switch (package) {
    case fdi::DriverPackageType::kBoot:
      return Collection::kBoot;
    case fdi::DriverPackageType::kBase:
      return Collection::kPackage;
    case fdi::DriverPackageType::kCached:
    case fdi::DriverPackageType::kUniverse:
      return Collection::kFullPackage;
    default:
      return Collection::kNone;
  }
}

// Perfrom a Breadth-First-Search (BFS) over the node topology, applying the visitor function on
// the node being visited.
// The return value of the visitor function is a boolean for whether the children of the node
// should be visited. If it returns false, the children will be skipped.
void PerformBFS(const std::shared_ptr<Node>& starting_node,
                fit::function<bool(const std::shared_ptr<dfv2::Node>&)> visitor) {
  std::unordered_set<std::shared_ptr<const Node>> visited;
  std::queue<std::shared_ptr<Node>> node_queue;
  visited.insert(starting_node);
  node_queue.push(starting_node);

  while (!node_queue.empty()) {
    auto current = node_queue.front();
    node_queue.pop();

    bool visit_children = visitor(current);
    if (!visit_children) {
      continue;
    }

    for (const auto& child : current->children()) {
      if (auto [_, inserted] = visited.insert(child); inserted) {
        node_queue.push(child);
      }
    }
  }
}

}  // namespace

Collection ToCollection(const Node& node, fdi::DriverPackageType package_type) {
  Collection collection = ToCollection(package_type);
  for (const auto& parent : node.parents()) {
    if (parent->collection() > collection) {
      collection = parent->collection();
    }
  }
  return collection;
}

DriverRunner::DriverRunner(fidl::ClientEnd<fcomponent::Realm> realm,
                           fidl::ClientEnd<fdi::DriverIndex> driver_index, InspectManager& inspect,
                           LoaderServiceFactory loader_service_factory,
                           async_dispatcher_t* dispatcher)
    : driver_index_(std::move(driver_index), dispatcher),
      loader_service_factory_(std::move(loader_service_factory)),
      dispatcher_(dispatcher),
      root_node_(
          std::make_shared<Node>(kRootDeviceName, std::vector<Node*>{}, this, dispatcher,
                                 inspect.CreateDevice(std::string(kRootDeviceName), zx::vmo(), 0))),
      composite_device_manager_(this, dispatcher, [this]() { this->TryBindAllAvailable(); }),
      composite_node_spec_manager_(this),
      runner_(dispatcher, fidl::WireClient(std::move(realm), dispatcher)) {
  inspect.inspector().GetRoot().CreateLazyNode(
      "driver_runner", [this] { return Inspect(); }, &inspect.inspector());

  // Pick a non-zero starting id so that folks cannot rely on the driver host process names being
  // stable.
  std::random_device rd;
  std::mt19937 gen(rd());
  std::uniform_int_distribution<> distrib(0, 1000);
  next_driver_host_id_ = distrib(gen);
}

void DriverRunner::BindNodesForCompositeNodeSpec() { TryBindAllAvailable(); }

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
  for (auto& [moniker, node] : orphaned_nodes_) {
    if (std::shared_ptr locked_node = node.lock()) {
      auto orphan = orphans.CreateChild(orphans.UniqueName("orphan-"));
      orphan.RecordString("moniker", moniker);
      orphans.Record(std::move(orphan));
    }
  }

  orphans.RecordBool("bind_all_ongoing", bind_all_ongoing_);
  orphans.RecordUint("pending_bind_requests", pending_bind_requests_.size());
  orphans.RecordUint("pending_orphan_rebind_callbacks", pending_orphan_rebind_callbacks_.size());

  inspector.GetRoot().Record(std::move(orphans));

  auto dfv1_composites = inspector.GetRoot().CreateChild("dfv1_composites");
  composite_device_manager_.Inspect(dfv1_composites);
  inspector.GetRoot().Record(std::move(dfv1_composites));

  return fpromise::make_ok_promise(inspector);
}

std::vector<fdd::wire::CompositeInfo> DriverRunner::GetCompositeListInfo(
    fidl::AnyArena& arena) const {
  // TODO(fxb/119947): Add composite node specs to the list.
  return composite_device_manager_.GetCompositeListInfo(arena);
}

void DriverRunner::PublishComponentRunner(component::OutgoingDirectory& outgoing) {
  zx::result result = runner_.Publish(outgoing);
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());

  composite_device_manager_.Publish(outgoing);
}

void DriverRunner::PublishCompositeNodeManager(component::OutgoingDirectory& outgoing) {
  zx::result result = outgoing.AddUnmanagedProtocol<fdf::CompositeNodeManager>(
      manager_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  ZX_ASSERT(result.is_ok());
}

zx::result<> DriverRunner::StartRootDriver(std::string_view url) {
  fdi::DriverPackageType package = cpp20::starts_with(url, kBootScheme)
                                       ? fdi::DriverPackageType::kBoot
                                       : fdi::DriverPackageType::kBase;
  return StartDriver(*root_node_, url, package);
}

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

        TryBindAllAvailable();
      });
}

void DriverRunner::TryBindAllAvailable(NodeBindingInfoResultCallback result_callback) {
  // If there's an ongoing process to bind all orphans, queue up this callback. Once
  // the process is complete, it'll make another attempt to bind all orphans and invoke
  // all callbacks in the list.
  if (bind_all_ongoing_) {
    pending_orphan_rebind_callbacks_.push_back(std::move(result_callback));
    return;
  }

  if (orphaned_nodes_.empty() && composite_parents_.empty()) {
    result_callback(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>());
    return;
  }

  bind_all_ongoing_ = true;

  // In case there is a pending call to TryBindAllAvailable() after this one, we automatically
  // restart the process and call all queued up callbacks upon completion.
  auto next_attempt =
      [this, result_callback = std::move(result_callback)](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) mutable {
        result_callback(results);
        ProcessPendingBindRequests();
      };
  std::shared_ptr<BindResultTracker> tracker =
      std::make_shared<BindResultTracker>(NumNodesAvailableForBind(), std::move(next_attempt));
  TryBindAllAvailableInternal(tracker);
}

zx::result<> DriverRunner::StartDriver(Node& node, std::string_view url,
                                       fdi::DriverPackageType package_type) {
  node.set_collection(ToCollection(node, package_type));

  std::weak_ptr node_weak = node.shared_from_this();
  runner_.StartDriverComponent(
      node.MakeComponentMoniker(), url, CollectionName(node.collection()).get(), node.offers(),
      [node_weak](zx::result<driver_manager::Runner::StartedComponent> component) {
        std::shared_ptr node = node_weak.lock();
        if (!node) {
          return;
        }

        if (component.is_error()) {
          node->CompleteBind(component.take_error());
          return;
        }
        fidl::Arena arena;
        node->StartDriver(fidl::ToWire(arena, std::move(component->info)),
                          std::move(component->controller), [node_weak](zx::result<> result) {
                            if (std::shared_ptr node = node_weak.lock(); node) {
                              node->CompleteBind(result);
                            }
                          });
      });
  return zx::ok();
}

void DriverRunner::DestroyDriverComponent(dfv2::Node& node,
                                          DestroyDriverComponentCallback callback) {
  auto name = node.MakeComponentMoniker();
  fdecl::wire::ChildRef child_ref{
      .name = fidl::StringView::FromExternal(name),
      .collection = CollectionName(node.collection()),
  };
  runner_.realm()->DestroyChild(child_ref).Then(std::move(callback));
}

void DriverRunner::Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) {
  BindToUrl(node, {}, std::move(result_tracker));
}

void DriverRunner::BindToUrl(Node& node, std::string_view driver_url_suffix,
                             std::shared_ptr<BindResultTracker> result_tracker) {
  BindRequest request = {
      .node = node.weak_from_this(),
      .driver_url_suffix = std::string(driver_url_suffix),
      .tracker = result_tracker,
      .composite_only = false,
  };
  if (bind_all_ongoing_) {
    pending_bind_requests_.push(std::move(request));
    return;
  }

  bind_all_ongoing_ = true;

  auto next_attempt = [this]() mutable { ProcessPendingBindRequests(); };
  BindInternal(std::move(request), next_attempt);
}

void DriverRunner::TryBindAllAvailableInternal(std::shared_ptr<BindResultTracker> tracker) {
  ZX_ASSERT(bind_all_ongoing_);

  if (orphaned_nodes_.empty() && composite_parents_.empty()) {
    return;
  }

  std::unordered_map<std::string, std::weak_ptr<Node>> cached_parents =
      std::move(composite_parents_);
  composite_parents_ = {};
  for (auto& [path, node_weak] : cached_parents) {
    std::shared_ptr node = node_weak.lock();
    if (!node) {
      tracker->ReportNoBind();
      continue;
    }

    BindInternal(BindRequest{
        .node = node_weak,
        .tracker = tracker,
        .composite_only = true,
    });

    composite_parents_.emplace(node->MakeComponentMoniker(), node_weak);
  }

  // Clear our stored map of orphaned nodes. It will be repopulated in Bind().
  std::unordered_map<std::string, std::weak_ptr<Node>> orphaned_nodes = std::move(orphaned_nodes_);
  orphaned_nodes_ = {};

  for (auto& [path, node] : orphaned_nodes) {
    BindInternal(BindRequest{
        .node = node,
        .tracker = tracker,
        .composite_only = false,
    });
  }
}

void DriverRunner::BindInternal(BindRequest request,
                                BindMatchCompleteCallback match_complete_callback) {
  ZX_ASSERT(bind_all_ongoing_);
  std::shared_ptr node = request.node.lock();
  if (!node) {
    LOGF(WARNING, "Node was freed before bind request is processed.");
    if (request.tracker) {
      request.tracker->ReportNoBind();
    }
    match_complete_callback();
    return;
  }

  // Bind to a DFv1 composite first. If it succeeds, return early to report a successful bind.
  // Add a pending bind request to check for other composites for multibind.
  if (composite_device_manager_.BindNode(node)) {
    composite_parents_.emplace(node->MakeComponentMoniker(), request.node);

    if (request.tracker) {
      request.tracker->ReportSuccessfulBind(node->MakeComponentMoniker(), "");
    }

    pending_bind_requests_.push(BindRequest{
        .node = request.node,
        .composite_only = true,
    });

    match_complete_callback();
    return;
  }

  std::string driver_url_suffix = request.driver_url_suffix;
  auto match_callback =
      [this, request = std::move(request),
       match_complete_callback = std::move(match_complete_callback)](
          fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result) mutable {
        OnMatchDriverCallback(std::move(request), result, std::move(match_complete_callback));
      };
  fidl::Arena arena;
  auto builder = fuchsia_driver_index::wire::MatchDriverArgs::Builder(arena)
                     .name(node->name())
                     .properties(node->properties());
  if (!driver_url_suffix.empty()) {
    builder.driver_url_suffix(driver_url_suffix);
  }
  driver_index_->MatchDriver(builder.Build()).Then(std::move(match_callback));
}

void DriverRunner::OnMatchDriverCallback(
    BindRequest request, fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result,
    BindMatchCompleteCallback match_complete_callback) {
  auto report_no_bind = fit::defer([&request, &match_complete_callback]() mutable {
    if (request.tracker) {
      request.tracker->ReportNoBind();
    }
    match_complete_callback();
  });

  std::shared_ptr node = request.node.lock();

  // TODO(fxb/125100): Add an additional guard to ensure that the node is still available for
  // binding when the match callback is fired. Currently, there are no issues from it, but it
  // is something we should address.
  if (!node) {
    LOGF(WARNING, "Node was freed before it could be bound");
    report_no_bind.call();
    return;
  }

  auto driver_url =
      BindNodeToResult(*node, result, request.composite_only, request.tracker != nullptr);
  if (driver_url == std::nullopt) {
    orphaned_nodes_.emplace(node->MakeComponentMoniker(), node);
    report_no_bind.call();
    return;
  }

  orphaned_nodes_.erase(node->MakeComponentMoniker());
  report_no_bind.cancel();
  if (request.tracker) {
    request.tracker->ReportSuccessfulBind(node->MakeComponentMoniker(), driver_url.value());
  }
  match_complete_callback();
}

std::optional<std::string> DriverRunner::BindNodeToResult(
    Node& node, fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result, bool composite_only,
    bool has_tracker) {
  if (!result.ok()) {
    LOGF(ERROR, "Failed to call match Node '%s': %s", node.name().c_str(),
         result.error().FormatDescription().data());
    return std::nullopt;
  }

  if (result->is_error()) {
    // Log the failed MatchDriver only if we are not tracking the results with a tracker
    // or if the error is not a ZX_ERR_NOT_FOUND error (meaning it could not find a driver).
    // When we have a tracker, the bind is happening for all the orphan nodes and the
    // not found errors get very noisy.
    zx_status_t match_error = result->error_value();
    if (match_error != ZX_ERR_NOT_FOUND && !has_tracker) {
      LOGF(WARNING, "Failed to match Node '%s': %s", node.MakeTopologicalPath().c_str(),
           zx_status_get_string(match_error));
    }

    return std::nullopt;
  }

  auto& matched_driver = result->value()->driver;

  if (composite_only && !matched_driver.is_parent_spec()) {
    return std::nullopt;
  }

  if (!matched_driver.is_driver() && !matched_driver.is_parent_spec()) {
    LOGF(WARNING,
         "Failed to match Node '%s', the MatchedDriver is not a normal driver or a "
         "parent spec.",
         node.name().c_str());
    return std::nullopt;
  }

  if (matched_driver.is_parent_spec() && !matched_driver.parent_spec().has_specs()) {
    LOGF(WARNING,
         "Failed to match Node '%s', the MatchedDriver is missing the composite node specs in the "
         "parent spec.",
         node.name().c_str());
    return std::nullopt;
  }

  // If this is a composite node spec match, bind the node into the spec. If the spec is
  // completed, use the returned node and driver to start the driver.
  if (matched_driver.is_parent_spec()) {
    auto result = BindNodeToSpec(node, matched_driver.parent_spec());
    if (!result.is_ok()) {
      return std::nullopt;
    }
    return "";
  }

  ZX_ASSERT(matched_driver.is_driver());

  // If the node is already part of a composite, it should not bind to a driver.
  if (composite_parents_.find(node.MakeComponentMoniker()) != composite_parents_.end()) {
    return std::nullopt;
  }

  zx::result start_result = StartDriver(node, matched_driver.driver());

  if (start_result.is_error()) {
    LOGF(ERROR, "Failed to start driver '%s': %s", node.name().c_str(),
         zx_status_get_string(start_result.error_value()));
    return std::nullopt;
  }

  node.OnBind();
  return start_result.value();
}

zx::result<> DriverRunner::BindNodeToSpec(
    Node& node, fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo parents) {
  // TODO(fxb/122531): Re-enable multibind and add the node to |composite_parents_|.
  auto result = composite_node_spec_manager_.BindParentSpec(parents, node.weak_from_this(),
                                                            /* enable_multibind */ false);
  if (result.is_error()) {
    if (result.error_value() != ZX_ERR_NOT_FOUND) {
      LOGF(ERROR, "Failed to bind node '%s' to any of the matched parent specs.",
           node.name().c_str());
    }
    return result.take_error();
  }

  auto composite_list = result.value();
  if (composite_list.empty()) {
    return zx::ok();
  }

  // Start the driver for each completed composite.
  for (auto& composite : composite_list) {
    auto weak_composite_node = std::get<std::weak_ptr<dfv2::Node>>(composite.node);
    std::shared_ptr composite_node = weak_composite_node.lock();
    ZX_ASSERT(composite_node);

    auto driver_info = composite.driver;
    if (!driver_info.has_url()) {
      LOGF(ERROR, "Failed to match Node '%s', the driver URL is missing", node.name().c_str());
      continue;
    }

    auto pkg_type =
        driver_info.has_package_type() ? driver_info.package_type() : fdi::DriverPackageType::kBase;
    auto start_result = StartDriver(*composite_node, driver_info.url().get(), pkg_type);
    if (start_result.is_error()) {
      LOGF(ERROR, "Failed to start driver '%s': %s", node.name().c_str(),
           zx_status_get_string(start_result.error_value()));
      continue;
    }

    composite_node->OnBind();
  }
  return zx::ok();
}

void DriverRunner::ProcessPendingBindRequests() {
  ZX_ASSERT(bind_all_ongoing_);
  if (pending_bind_requests_.empty() && pending_orphan_rebind_callbacks_.empty()) {
    bind_all_ongoing_ = false;
    return;
  }

  bool have_bind_all_request = !pending_orphan_rebind_callbacks_.empty();
  size_t bind_tracker_size = have_bind_all_request
                                 ? pending_bind_requests_.size() + NumNodesAvailableForBind()
                                 : pending_bind_requests_.size();

  // If there are no nodes to bind, then we'll run through all the callbacks and end the bind
  // process.
  if (have_bind_all_request && bind_tracker_size == 0) {
    for (auto& callback : pending_orphan_rebind_callbacks_) {
      fidl::Arena arena;
      callback(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>(arena, 0));
    }
    pending_orphan_rebind_callbacks_.clear();
    bind_all_ongoing_ = false;
    return;
  }

  // Follow up with another ProcessPendingBindRequests() after all the pending bind calls are
  // complete. If there are no more accumulated bind calls, then the bind process ends.
  auto next_attempt =
      [this, callbacks = std::move(pending_orphan_rebind_callbacks_)](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) mutable {
        for (auto& callback : callbacks) {
          callback(results);
        }
        ProcessPendingBindRequests();
      };

  std::shared_ptr<BindResultTracker> tracker =
      std::make_shared<BindResultTracker>(bind_tracker_size, std::move(next_attempt));

  // Go through all the pending bind requests.
  while (!pending_bind_requests_.empty()) {
    BindRequest request = std::move(pending_bind_requests_.front());
    pending_bind_requests_.pop();
    auto match_complete_callback = [tracker]() mutable {
      // The bind status doesn't matter for this tracker.
      tracker->ReportNoBind();
    };
    BindInternal(std::move(request), std::move(match_complete_callback));
  }

  // If there are any pending callbacks for TryBindAllAvailable(), begin a new attempt.
  if (have_bind_all_request) {
    TryBindAllAvailableInternal(tracker);
  }
}

zx::result<DriverHost*> DriverRunner::CreateDriverHost() {
  zx::result endpoints = fidl::CreateEndpoints<fio::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  std::string name = "driver-host-" + std::to_string(next_driver_host_id_++);
  auto create = CreateDriverHostComponent(name, std::move(endpoints->server));
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

zx::result<std::string> DriverRunner::StartDriver(
    Node& node, fuchsia_driver_index::wire::MatchedDriverInfo driver_info) {
  if (!driver_info.has_url()) {
    LOGF(ERROR, "Failed to start driver for node '%s', the driver URL is missing",
         node.name().c_str());
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto pkg_type =
      driver_info.has_package_type() ? driver_info.package_type() : fdi::DriverPackageType::kBase;
  auto result = StartDriver(node, driver_info.url().get(), pkg_type);
  if (result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::string(driver_info.url().get()));
}

zx::result<> DriverRunner::CreateDriverHostComponent(
    std::string moniker, fidl::ServerEnd<fuchsia_io::Directory> exposed_dir) {
  constexpr std::string_view kUrl = "fuchsia-boot:///driver_host2#meta/driver_host2.cm";
  fidl::Arena arena;
  auto child_decl_builder = fdecl::wire::Child::Builder(arena).name(moniker).url(kUrl).startup(
      fdecl::wire::StartupMode::kLazy);
  auto child_args_builder = fcomponent::wire::CreateChildArgs::Builder(arena);
  auto open_callback =
      [moniker](fidl::WireUnownedResult<fcomponent::Realm::OpenExposedDir>& result) {
        if (!result.ok()) {
          LOGF(ERROR, "Failed to open exposed directory for driver host: '%s': %s", moniker.c_str(),
               result.FormatDescription().data());
          return;
        }
        if (result->is_error()) {
          LOGF(ERROR, "Failed to open exposed directory for driver host: '%s': %u", moniker.c_str(),
               result->error_value());
        }
      };
  auto create_callback =
      [this, moniker, exposed_dir = std::move(exposed_dir),
       open_callback = std::move(open_callback)](
          fidl::WireUnownedResult<fcomponent::Realm::CreateChild>& result) mutable {
        if (!result.ok()) {
          LOGF(ERROR, "Failed to create driver host '%s': %s", moniker.c_str(),
               result.error().FormatDescription().data());
          return;
        }
        if (result->is_error()) {
          LOGF(ERROR, "Failed to create driver host '%s': %u", moniker.c_str(),
               result->error_value());
          return;
        }
        fdecl::wire::ChildRef child_ref{
            .name = fidl::StringView::FromExternal(moniker),
            .collection = "driver-hosts",
        };
        runner_.realm()
            ->OpenExposedDir(child_ref, std::move(exposed_dir))
            .ThenExactlyOnce(std::move(open_callback));
      };
  runner_.realm()
      ->CreateChild(
          fdecl::wire::CollectionRef{
              .name = "driver-hosts",
          },
          child_decl_builder.Build(), child_args_builder.Build())
      .Then(std::move(create_callback));
  return zx::ok();
}

zx::result<uint32_t> DriverRunner::RestartNodesColocatedWithDriverUrl(std::string_view url) {
  auto driver_hosts = DriverHostsWithDriverUrl(url);

  // Perform a BFS over the node topology, if the current node's host is one of the driver_hosts
  // we collected, then restart that node and skip its children since they will go away
  // as part of it's restart.
  //
  // The BFS ensures that we always find the topmost node of a driver host.
  // This node will by definition have colocated set to false, so when we call StartDriver
  // on this node we will always create a new driver host. The old driver host will go away
  // on its own asynchronously since it is drained from all of its drivers.
  PerformBFS(root_node_, [&driver_hosts](const std::shared_ptr<dfv2::Node>& current) {
    if (driver_hosts.find(current->driver_host()) != driver_hosts.end()) {
      current->RestartNode();
      // Don't visit children of this node since we restarted it.
      return false;
    }

    return true;
  });

  return zx::ok(static_cast<uint32_t>(driver_hosts.size()));
}

std::unordered_set<const DriverHost*> DriverRunner::DriverHostsWithDriverUrl(std::string_view url) {
  std::unordered_set<const DriverHost*> result_hosts;

  // Perform a BFS over the node topology, if the current node's driver url is the url we are
  // interested in, add the driver host it is in to the result set.
  PerformBFS(root_node_, [&result_hosts, url](const std::shared_ptr<dfv2::Node>& current) {
    if (current->driver_url() == url) {
      result_hosts.insert(current->driver_host());
    }
    return true;
  });

  return result_hosts;
}

}  // namespace dfv2
