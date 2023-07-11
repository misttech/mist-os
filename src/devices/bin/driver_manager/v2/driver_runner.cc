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
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/fit/defer.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/status.h>

#include <forward_list>
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
      composite_node_spec_manager_(this),
      bind_manager_(this, this, dispatcher),
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

  bind_manager_.RecordInspect(inspector);

  return fpromise::make_ok_promise(inspector);
}

std::vector<fdd::wire::CompositeInfo> DriverRunner::GetCompositeListInfo(
    fidl::AnyArena& arena) const {
  auto spec_composite_list = composite_node_spec_manager_.GetCompositeInfo(arena);
  auto list = bind_manager_.GetCompositeListInfo(arena);
  list.reserve(list.size() + spec_composite_list.size());
  list.insert(list.end(), std::make_move_iterator(spec_composite_list.begin()),
              std::make_move_iterator(spec_composite_list.end()));
  return list;
}

void DriverRunner::PublishComponentRunner(component::OutgoingDirectory& outgoing) {
  zx::result result = runner_.Publish(outgoing);
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());

  result = outgoing.AddUnmanagedProtocol<fdf::CompositeNodeManager>(
      manager_bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure));
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());

  bind_manager_.Publish(outgoing);
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
  bind_manager_.TryBindAllAvailable(std::move(result_callback));
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

void DriverRunner::Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) {
  BindToUrl(node, {}, std::move(result_tracker));
}

void DriverRunner::BindToUrl(Node& node, std::string_view driver_url_suffix,
                             std::shared_ptr<BindResultTracker> result_tracker) {
  bind_manager_.Bind(node, driver_url_suffix, std::move(result_tracker));
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

zx::result<std::vector<CompositeNodeAndDriver>> DriverRunner::BindToParentSpec(
    fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo match_info, std::weak_ptr<Node> node,
    bool enable_multibind) {
  return this->composite_node_spec_manager_.BindParentSpec(match_info, node, enable_multibind);
}

void DriverRunner::RequestMatchFromDriverIndex(
    fuchsia_driver_index::wire::MatchDriverArgs args,
    fit::callback<void(fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>&)> match_callback) {
  driver_index()->MatchDriver(args).Then(std::move(match_callback));
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

zx::result<uint32_t> DriverRunner::RestartNodesColocatedWithDriverUrl(
    std::string_view url, fdd::RematchFlags rematch_flags) {
  if (rematch_flags & (fdd::RematchFlags::kCompositeSpec | fdd::RematchFlags::kLegacyComposite)) {
    LOGF(WARNING, "Rematching composites are not supported currently.");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  auto driver_hosts = DriverHostsWithDriverUrl(url);

  // Perform a BFS over the node topology, if the current node's host is one of the driver_hosts
  // we collected, then restart that node and skip its children since they will go away
  // as part of it's restart.
  //
  // The BFS ensures that we always find the topmost node of a driver host.
  // This node will by definition have colocated set to false, so when we call StartDriver
  // on this node we will always create a new driver host. The old driver host will go away
  // on its own asynchronously since it is drained from all of its drivers.
  PerformBFS(
      root_node_, [&driver_hosts, rematch_flags, url](const std::shared_ptr<dfv2::Node>& current) {
        if (driver_hosts.find(current->driver_host()) != driver_hosts.end()) {
          // We want the node to re-match to a driver (rather than re-using its existing driver) if
          // it's not a composite and we have either the following:
          // - The node has the requested URL and we have a requested flag
          // - We have a non-requested flag
          if (!current->IsComposite() &&
              ((current->driver_url() == url && rematch_flags & fdd::RematchFlags::kRequested) ||
               rematch_flags & fdd::RematchFlags::kNonRequested)) {
            current->RestartNodeWithRematch();
          } else {
            current->RestartNode();
          }

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
