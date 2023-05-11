// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/node.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/component/cpp/start_args.h>

#include <deque>
#include <unordered_set>

#include "src/devices/bin/driver_manager/v2/node_removal_tracker.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/join_strings.h"

const std::string kUnboundUrl = "unbound";

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
namespace fdecl = fuchsia_component_decl;
namespace fcomponent = fuchsia_component;

namespace dfv2 {

namespace {

const char* State2String(NodeState state) {
  switch (state) {
    case NodeState::kRunning:
      return "kRunning";
    case NodeState::kPrestop:
      return "kPrestop";
    case NodeState::kWaitingOnChildren:
      return "kWaitingOnChildren";
    case NodeState::kWaitingOnDriver:
      return "kWaitingOnDriver";
    case NodeState::kWaitingOnDriverComponent:
      return "kWaitingOnDriverComponent";
    case NodeState::kStopping:
      return "kStopping";
  }
}

template <typename R, typename F>
std::optional<R> VisitOffer(fdecl::Offer& offer, F apply) {
  // Note, we access each field of the union as mutable, so that `apply` can
  // modify the field if necessary.
  switch (offer.Which()) {
    case fdecl::Offer::Tag::kService:
      return apply(offer.service());
    case fdecl::Offer::Tag::kProtocol:
      return apply(offer.protocol());
    case fdecl::Offer::Tag::kDirectory:
      return apply(offer.directory());
    case fdecl::Offer::Tag::kStorage:
      return apply(offer.storage());
    case fdecl::Offer::Tag::kRunner:
      return apply(offer.runner());
    case fdecl::Offer::Tag::kResolver:
      return apply(offer.resolver());
    case fdecl::Offer::Tag::kEventStream:
      return apply(offer.event_stream());
    default:
      return {};
  }
}

const char* CollectionName(Collection collection) {
  switch (collection) {
    case Collection::kNone:
      return "";
    case Collection::kBoot:
      return "boot-drivers";
    case Collection::kPackage:
      return "pkg-drivers";
    case Collection::kFullPackage:
      return "full-pkg-drivers";
  }
}

bool NodeStateIsExiting(NodeState node_state) {
  switch (node_state) {
    case NodeState::kRunning:
      return false;
    case NodeState::kPrestop:
    case NodeState::kWaitingOnChildren:
    case NodeState::kWaitingOnDriver:
    case NodeState::kWaitingOnDriverComponent:
    case NodeState::kStopping:
      return true;
  }
}

uint32_t GetProtocolId(const std::vector<fdf::wire::NodeProperty>& properties) {
  uint32_t protocol_id = 0;
  for (const fdf::wire::NodeProperty& property : properties) {
    if (!property.key.is_int_value()) {
      continue;
    }
    if (property.key.int_value() != BIND_PROTOCOL) {
      continue;
    }
    protocol_id = property.value.int_value();
  }
  return protocol_id;
}

fit::result<fdf::wire::NodeError, fdecl::Offer> AddOfferToNodeOffer(fdecl::Offer add_offer,
                                                                    fdecl::Ref source) {
  auto has_source_name =
      VisitOffer<bool>(add_offer, [](const auto& decl) { return decl->source_name().has_value(); });
  if (!has_source_name.value_or(false)) {
    return fit::as_error(fdf::wire::NodeError::kOfferSourceNameMissing);
  }
  auto has_ref = VisitOffer<bool>(add_offer, [](const auto& decl) {
    return decl->source().has_value() || decl->target().has_value();
  });
  if (has_ref.value_or(false)) {
    return fit::as_error(fdf::wire::NodeError::kOfferRefExists);
  }

  // Assign the source of the offer.
  VisitOffer<bool>(add_offer, [source = std::move(source)](auto decl) mutable {
    decl->source(std::move(source));
    return true;
  });

  return fit::ok(std::move(add_offer));
}

bool IsDefaultOffer(std::string_view target_name) {
  return std::string_view("default").compare(target_name) == 0;
}

template <typename T>
void CloseIfExists(std::optional<fidl::ServerBinding<T>>& ref) {
  if (ref) {
    ref->Close(ZX_OK);
  }
}

fit::result<fdf::wire::NodeError> ValidateSymbols(std::vector<fdf::NodeSymbol>& symbols) {
  std::unordered_set<std::string_view> names;
  for (auto& symbol : symbols) {
    if (!symbol.name().has_value()) {
      LOGF(ERROR, "SymbolError: a symbol is missing a name");
      return fit::error(fdf::wire::NodeError::kSymbolNameMissing);
    }
    if (!symbol.address().has_value()) {
      LOGF(ERROR, "SymbolError: symbol '%s' is missing an address", symbol.name().value().c_str());
      return fit::error(fdf::wire::NodeError::kSymbolAddressMissing);
    }
    auto [_, inserted] = names.emplace(symbol.name().value());
    if (!inserted) {
      LOGF(ERROR, "SymbolError: symbol '%s' already exists", symbol.name().value().c_str());
      return fit::error(fdf::wire::NodeError::kSymbolAlreadyExists);
    }
  }
  return fit::ok();
}

}  // namespace

std::optional<fdecl::wire::Offer> CreateCompositeServiceOffer(fidl::AnyArena& arena,
                                                              fdecl::wire::Offer& offer,
                                                              std::string_view parents_name,
                                                              bool primary_parent) {
  if (!offer.is_service() || !offer.service().has_source_instance_filter() ||
      !offer.service().has_renamed_instances()) {
    return std::nullopt;
  }

  size_t new_instance_count = offer.service().renamed_instances().count();
  if (primary_parent) {
    for (auto& instance : offer.service().renamed_instances()) {
      if (IsDefaultOffer(instance.target_name.get())) {
        new_instance_count++;
      }
    }
  }

  size_t new_filter_count = offer.service().source_instance_filter().count();
  if (primary_parent) {
    for (auto& filter : offer.service().source_instance_filter()) {
      if (IsDefaultOffer(filter.get())) {
        new_filter_count++;
      }
    }
  }

  // We have to create a new offer so we aren't manipulating our parent's offer.
  auto service = fdecl::wire::OfferService::Builder(arena);
  if (offer.service().has_source_name()) {
    service.source_name(offer.service().source_name());
  }
  if (offer.service().has_target_name()) {
    service.target_name(offer.service().target_name());
  }
  if (offer.service().has_source()) {
    service.source(offer.service().source());
  }
  if (offer.service().has_target()) {
    service.target(offer.service().target());
  }

  size_t index = 0;
  fidl::VectorView<fdecl::wire::NameMapping> mappings(arena, new_instance_count);
  for (auto instance : offer.service().renamed_instances()) {
    // The instance is not "default", so copy it over.
    if (!IsDefaultOffer(instance.target_name.get())) {
      mappings[index].source_name = fidl::StringView(arena, instance.source_name.get());
      mappings[index].target_name = fidl::StringView(arena, instance.target_name.get());
      index++;
      continue;
    }

    // We are the primary parent, so add the "default" offer.
    if (primary_parent) {
      mappings[index].source_name = fidl::StringView(arena, instance.source_name.get());
      mappings[index].target_name = fidl::StringView(arena, instance.target_name.get());
      index++;
    }

    // Rename the instance to match the parent's name.
    mappings[index].source_name = fidl::StringView(arena, instance.source_name.get());
    mappings[index].target_name = fidl::StringView(arena, parents_name);
    index++;
  }
  ZX_ASSERT(index == new_instance_count);

  index = 0;
  fidl::VectorView<fidl::StringView> filters(arena, new_instance_count);
  for (auto filter : offer.service().source_instance_filter()) {
    // The filter is not "default", so copy it over.
    if (!IsDefaultOffer(filter.get())) {
      filters[index] = fidl::StringView(arena, filter.get());
      index++;
      continue;
    }

    // We are the primary parent, so add the "default" filter.
    if (primary_parent) {
      filters[index] = fidl::StringView(arena, "default");
      index++;
    }

    // Rename the filter to match the parent's name.
    filters[index] = fidl::StringView(arena, parents_name);
    index++;
  }
  ZX_ASSERT(index == new_filter_count);

  service.renamed_instances(mappings);
  service.source_instance_filter(filters);

  return fdecl::wire::Offer::WithService(arena, service.Build());
}

std::optional<fdecl::wire::Offer> CreateCompositeOffer(fidl::AnyArena& arena,
                                                       fdecl::wire::Offer& offer,
                                                       std::string_view parents_name,
                                                       bool primary_parent) {
  // We route 'service' capabilities based on the parent's name.
  if (offer.is_service()) {
    return CreateCompositeServiceOffer(arena, offer, parents_name, primary_parent);
  }

  // Other capabilities we can simply forward unchanged, but allocated on the new arena.
  return fidl::ToWire(arena, fidl::ToNatural(offer));
}

BindResultTracker::BindResultTracker(size_t expected_result_count,
                                     NodeBindingInfoResultCallback result_callback)
    : expected_result_count_(expected_result_count),
      currently_reported_(0),
      result_callback_(std::move(result_callback)) {
  ZX_ASSERT(expected_result_count > 0);
}

void BindResultTracker::ReportNoBind() {
  size_t current;
  {
    std::scoped_lock guard(lock_);
    currently_reported_++;
    current = currently_reported_;
  }

  Complete(current);
}

void BindResultTracker::ReportSuccessfulBind(const std::string_view& node_name,
                                             const std::string_view& driver) {
  size_t current;
  {
    std::scoped_lock guard(lock_);
    currently_reported_++;
    auto node_binding_info = fuchsia_driver_development::wire::NodeBindingInfo::Builder(arena_)
                                 .node_name(node_name)
                                 .driver_url(driver)
                                 .Build();
    results_.emplace_back(node_binding_info);
    current = currently_reported_;
  }

  Complete(current);
}

void BindResultTracker::Complete(size_t current) {
  if (current == expected_result_count_) {
    result_callback_(
        fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>(arena_, results_));
  }
}

Node::Node(std::string_view name, std::vector<Node*> parents, NodeManager* node_manager,
           async_dispatcher_t* dispatcher, DeviceInspect inspect, uint32_t primary_index)
    : name_(name),
      parents_(std::move(parents)),
      primary_index_(primary_index),
      node_manager_(node_manager),
      dispatcher_(dispatcher),
      inspect_(std::move(inspect)) {
  ZX_ASSERT(primary_index_ == 0 || primary_index_ < parents_.size());
  if (auto primary_parent = GetPrimaryParent()) {
    // By default, we set `driver_host_` to match the primary parent's
    // `driver_host_`. If the node is then subsequently bound to a driver in a
    // different driver host, this value will be updated to match.
    driver_host_ = primary_parent->driver_host_;
  }
}

Node::Node(std::string_view name, std::vector<Node*> parents, NodeManager* node_manager,
           async_dispatcher_t* dispatcher, DeviceInspect inspect, DriverHost* driver_host)
    : name_(name),
      parents_(std::move(parents)),
      node_manager_(node_manager),
      dispatcher_(dispatcher),
      driver_host_(driver_host),
      inspect_(std::move(inspect)) {}

zx::result<std::shared_ptr<Node>> Node::CreateCompositeNode(
    std::string_view node_name, std::vector<Node*> parents, std::vector<std::string> parents_names,
    std::vector<fuchsia_driver_framework::wire::NodeProperty> properties,
    NodeManager* driver_binder, async_dispatcher_t* dispatcher, uint32_t primary_index) {
  ZX_ASSERT(!parents.empty());
  if (primary_index >= parents.size()) {
    LOGF(ERROR, "Primary node index is out of bounds");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  DeviceInspect inspect =
      parents[primary_index]->inspect_.CreateChild(std::string(node_name), zx::vmo(), 0);
  std::shared_ptr composite = std::make_shared<Node>(node_name, std::move(parents), driver_binder,
                                                     dispatcher, std::move(inspect), primary_index);
  composite->parents_names_ = std::move(parents_names);

  for (auto& prop : properties) {
    auto natural = fidl::ToNatural(prop);
    auto new_prop = fidl::ToWire(composite->arena_, std::move(natural));
    composite->properties_.push_back(new_prop);
  }
  composite->SetAndPublishInspect();

  Node* primary = composite->GetPrimaryParent();
  // We know that our device has a parent because we're creating it.
  ZX_ASSERT(primary);

  // Copy the symbols from the primary parent.
  composite->symbols_.reserve(primary->symbols_.size());
  for (auto& symbol : primary->symbols_) {
    composite->symbols_.emplace_back(fdf::wire::NodeSymbol::Builder(composite->arena_)
                                         .name(composite->arena_, symbol.name().get())
                                         .address(symbol.address())
                                         .Build());
  }

  // Copy the offers from each parent.
  std::vector<fdecl::wire::Offer> node_offers;
  size_t parent_index = 0;
  for (const Node* parent : composite->parents_) {
    auto parent_offers = parent->offers();
    node_offers.reserve(node_offers.size() + parent_offers.count());

    for (auto& parent_offer : parent_offers) {
      auto offer = CreateCompositeOffer(composite->arena_, parent_offer,
                                        composite->parents_names_[parent_index],
                                        parent_index == primary_index);
      if (offer) {
        node_offers.push_back(*offer);
      }
    }
    parent_index++;
  }
  composite->offers_ = std::move(node_offers);

  composite->AddToParents();
  ZX_ASSERT_MSG(primary->devfs_device_.topological_node().has_value(), "%s",
                composite->MakeTopologicalPath().c_str());
  primary->devfs_device_.topological_node().value().add_child(
      composite->name_, std::nullopt, Devnode::NoRemote(), composite->devfs_device_);
  composite->devfs_device_.publish();
  return zx::ok(std::move(composite));
}

Node::~Node() {
  CloseIfExists(controller_ref_);
  if (request_bind_completer_.has_value()) {
    request_bind_completer_.value().ReplyError(ZX_ERR_CANCELED);
    request_bind_completer_.reset();
  }
}

const std::string& Node::driver_url() const {
  if (driver_component_) {
    return driver_component_->driver_url;
  }
  return kUnboundUrl;
}
const std::vector<Node*>& Node::parents() const { return parents_; }

const std::list<std::shared_ptr<Node>>& Node::children() const { return children_; }

fidl::VectorView<fuchsia_component_decl::wire::Offer> Node::offers() const {
  // TODO(fxbug.dev/66150): Once FIDL wire types support a Clone() method,
  // remove the const_cast.
  return fidl::VectorView<fdecl::wire::Offer>::FromExternal(
      const_cast<decltype(offers_)&>(offers_));
}

fidl::VectorView<fdf::wire::NodeSymbol> Node::symbols() const {
  // TODO(fxbug.dev/7999): Remove const_cast once VectorView supports const.
  return fidl::VectorView<fdf::wire::NodeSymbol>::FromExternal(
      const_cast<decltype(symbols_)&>(symbols_));
}

const std::vector<fdf::wire::NodeProperty>& Node::properties() const { return properties_; }

void Node::set_collection(Collection collection) { collection_ = collection; }

std::string Node::MakeTopologicalPath() const {
  std::deque<std::string_view> names;
  for (auto node = this; node != nullptr; node = node->GetPrimaryParent()) {
    names.push_front(node->name());
  }
  return fxl::JoinStrings(names, "/");
}

std::string Node::MakeComponentMoniker() const {
  std::string topo_path = MakeTopologicalPath();

  // The driver's component name is based on the node name, which means that the
  // node name cam only have [a-z0-9-_.] characters. DFv1 composites contain ':'
  // which is not allowed, so replace those characters.
  // TODO(fxbug.dev/111156): Migrate driver names to only use CF valid characters.
  std::replace(topo_path.begin(), topo_path.end(), ':', '_');
  std::replace(topo_path.begin(), topo_path.end(), '/', '.');
  return topo_path;
}

void Node::OnBind() const {
  if (controller_ref_) {
    fidl::Status result = fidl::WireSendEvent(*controller_ref_)->OnBind();
    if (!result.ok()) {
      LOGF(ERROR, "Failed to send OnBind event: %s", result.FormatDescription().data());
    }
  }
}

void Node::Stop(StopCompleter::Sync& completer) {
  LOGF(DEBUG, "Calling Remove on %s because of Stop() from component framework.", name().c_str());
  Remove(RemovalSet::kAll, nullptr);
}

void Node::Kill(KillCompleter::Sync& completer) {
  LOGF(DEBUG, "Calling Remove on %s because of Kill() from component framework.", name().c_str());
  Remove(RemovalSet::kAll, nullptr);
}

Node* Node::GetPrimaryParent() const {
  return parents_.empty() ? nullptr : parents_[primary_index_];
}

void Node::CompleteBind(zx::result<> result) {
  if (result.is_error()) {
    driver_component_.reset();
  }
  auto completer = std::move(request_bind_completer_);
  request_bind_completer_.reset();
  if (completer) {
    completer->Reply(result);
  }
}

void Node::AddToParents() {
  auto this_node = shared_from_this();
  for (auto parent : parents_) {
    parent->children_.push_back(this_node);
  }
}

void Node::RemoveChild(std::shared_ptr<Node> child) {
  LOGF(DEBUG, "RemoveChild %s from parent %s", child->name().c_str(), name().c_str());
  children_.erase(std::find(children_.begin(), children_.end(), child));
  // If we are waiting for children, see if that is done:
  if (node_state_ != NodeState::kPrestop && node_state_ != NodeState::kRunning) {
    CheckForRemoval();
  }
}

void Node::CheckForRemoval() {
  if (node_state_ != NodeState::kWaitingOnChildren) {
    LOGF(DEBUG, "Node: %s CheckForRemoval: not waiting on children.", name().c_str());
    return;
  }

  LOGF(DEBUG, "Node: %s Checking for removal", name().c_str());
  if (!children_.empty()) {
    return;
  }
  node_state_ = NodeState::kWaitingOnDriver;
  if (removal_tracker_ && removal_id_.has_value()) {
    removal_tracker_->Notify(removal_id_.value(), node_state_);
  }
  LOGF(DEBUG, "Node::Remove(): %s children are empty", name().c_str());
  if (driver_component_ && driver_component_->driver) {
    fidl::OneWayStatus result = driver_component_->driver->Stop();
    if (result.ok()) {
      return;  // We'll now wait for the channel to close
    }
    LOGF(ERROR, "Node: %s failed to stop driver: %s", name().c_str(),
         result.FormatDescription().data());
    // We'd better continue to close, since we can't talk to the driver.
  }

  ScheduleStopComponent();
}

void Node::FinishRemoval() {
  LOGF(DEBUG, "Node: %s Finishing removal", name().c_str());
  ZX_ASSERT_MSG(node_state_ == NodeState::kWaitingOnDriverComponent,
                "FinishRemoval called in invalid node state: %s", State2String(node_state_));
  if (node_restarting_) {
    FinishRestart();
    return;
  }

  // Get an extra shared_ptr to ourselves so we are not freed halfway through this function.
  std::shared_ptr this_node = shared_from_this();
  node_state_ = NodeState::kStopping;
  driver_component_.reset();
  for (auto& parent : parents()) {
    parent->RemoveChild(shared_from_this());
  }
  parents_.clear();

  LOGF(DEBUG, "Node: %s unbinding and resetting", name().c_str());
  CloseIfExists(controller_ref_);
  CloseIfExists(node_ref_);
  devfs_device_.unpublish();
  if (removal_tracker_ && removal_id_.has_value()) {
    removal_tracker_->Notify(removal_id_.value(), node_state_);
  }
  if (remove_complete_callback_) {
    remove_complete_callback_();
  }
}

void Node::FinishRestart() {
  ZX_ASSERT_MSG(node_restarting_, "FinishRestart called when node is not restarting.");
  node_restarting_ = false;
  node_state_ = NodeState::kRunning;

  if (restart_driver_url_suffix_.has_value()) {
    auto tracker = CreateBindResultTracker();
    node_manager_.value()->BindToUrl(*this, restart_driver_url_suffix_.value(), std::move(tracker));
    restart_driver_url_suffix_.reset();
    return;
  }

  fuchsia_driver_index::DriverPackageType pkg_type;
  switch (collection_) {
    case Collection::kNone:
      pkg_type = fuchsia_driver_index::DriverPackageType::Unknown();
      break;
    case Collection::kBoot:
      pkg_type = fuchsia_driver_index::DriverPackageType::kBoot;
      break;
    case Collection::kPackage:
      pkg_type = fuchsia_driver_index::DriverPackageType::kBase;
      break;
    case Collection::kFullPackage:
      pkg_type = fuchsia_driver_index::DriverPackageType::kUniverse;
      break;
  }
  std::string url = driver_url();
  zx::result start_result = node_manager_.value()->StartDriver(*this, url, pkg_type);
  if (start_result.is_error()) {
    LOGF(ERROR, "Failed to start driver '%s': %s", name().c_str(), start_result.status_string());
  }
}

// State table for package driver:
//                                   Initial States
//                 Running | Prestop|  WoC   | WoDriver | Stopping
// Remove(kPkg)      WoC   |  WoC   | Ignore |  Error!  |  Error!
// Remove(kAll)      WoC   |  WoC   |  WoC   |  Error!  |  Error!
// children empty    N/A   |  N/A   |WoDriver|  Error!  |  Error!
// Driver exit       WoC   |  WoC   |  WoC   | Stopping |  Error!
//
// State table for boot driver:
//                                   Initial States
//                  Running | Prestop |  WoC   | WoDriver | Stopping
// Remove(kPkg)     Prestop | Ignore  | Ignore |  Ignore  |  Ignore
// Remove(kAll)      WoC    |   WoC   | Ignore |  Ignore  |  Ignore
// children empty    N/A    |   N/A   |WoDriver|  Ignore  |  Ignore
// Driver exit       WoC    |   WoC   |  WoC   | Stopping |  Ignore
// Boot drivers go into the Prestop state when Remove(kPackage) is set, to signify that
// a removal is taking place, but this node will not be removed yet, even if all its children
// are removed.
void Node::Remove(RemovalSet removal_set, NodeRemovalTracker* removal_tracker) {
  if (!removal_tracker && removal_tracker_) {
    // TODO(fxbug.dev/115171): Change this to an error when we track shutdown steps better.
    LOGF(WARNING, "Untracked Node::Remove() called on %s, indicating an error during shutdown",
         MakeTopologicalPath().c_str());
  }

  if (removal_tracker) {
    if (removal_tracker_) {
      // We should never have two competing trackers
      ZX_ASSERT(removal_tracker_ == removal_tracker);
    } else {
      // We are getting a removal tracker for the first time so register ourselves.
      removal_tracker_ = removal_tracker;
      removal_id_ = removal_tracker_->RegisterNode(NodeRemovalTracker::Node{
          .name = MakeComponentMoniker(),
          .collection = collection_,
          .state = node_state_,
      });
    }
  }

  LOGF(DEBUG, "Remove called on Node: %s", name().c_str());
  // Two cases where we will transition state and take action:
  // Removing kAll, and state is Running or Prestop
  // Removing kPkg, and state is Running
  if ((node_state_ != NodeState::kPrestop && node_state_ != NodeState::kRunning) ||
      (node_state_ == NodeState::kPrestop && removal_set == RemovalSet::kPackage)) {
    if (parents_.size() <= 1) {
      LOGF(WARNING, "Node::Remove() %s called late, already in state %s",
           MakeComponentMoniker().c_str(), State2String(node_state_));
    }
    return;
  }

  // Now, the cases where we do something:
  // Set the new state
  if (removal_set == RemovalSet::kPackage &&
      (collection_ == Collection::kBoot || collection_ == Collection::kNone)) {
    node_state_ = NodeState::kPrestop;
  } else {
    // Either removing kAll, or is package driver and removing kPackage.
    node_state_ = NodeState::kWaitingOnChildren;
    // All children should be removed regardless as they block removal of this node.
    removal_set = RemovalSet::kAll;
  }

  // Propagate removal message to children
  if (removal_tracker_ && removal_id_.has_value()) {
    removal_tracker_->Notify(removal_id_.value(), node_state_);
  }

  // Get an extra shared_ptr to ourselves so we are not freed as we remove children.
  std::shared_ptr this_node = shared_from_this();

  // Ask each of our children to remove themselves.
  for (auto it = children_.begin(); it != children_.end();) {
    // We have to be careful here - Remove() could invalidate the iterator, so we increment the
    // iterator before we call Remove().
    LOGF(DEBUG, "Node: %s calling remove on child: %s", name().c_str(), (*it)->name().c_str());
    Node* child = it->get();
    ++it;
    child->Remove(removal_set, removal_tracker);
  }

  // In case we had no children, or they removed themselves synchronously:
  CheckForRemoval();
}

void Node::RestartNode() {
  node_restarting_ = true;
  Remove(RemovalSet::kAll, nullptr);
}

std::shared_ptr<BindResultTracker> Node::CreateBindResultTracker() {
  return std::make_shared<BindResultTracker>(
      1, [weak_self = weak_from_this()](
             fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> info) {
        std::shared_ptr self = weak_self.lock();
        if (!self) {
          return;
        }
        // We expect a single successful "bind". If we don't get it, we can assume the bind
        // request failed. If we do get it, we will continue to wait for the driver's start hook
        // to complete, which will only occur after the successful bind. The remaining flow will
        // be similar to the RestartNode flow.
        if (info.count() < 1) {
          self->CompleteBind(zx::error(ZX_ERR_NOT_FOUND));
        } else if (info.count() > 1) {
          LOGF(ERROR, "Unexpectedly bound multiple drivers to a single node");
          self->CompleteBind(zx::error(ZX_ERR_BAD_STATE));
        }
      });
}

fit::result<fuchsia_driver_framework::wire::NodeError, std::shared_ptr<Node>> Node::AddChildHelper(
    fuchsia_driver_framework::NodeAddArgs args,
    fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller,
    fidl::ServerEnd<fuchsia_driver_framework::Node> node) {
  if (node_manager_ == nullptr) {
    LOGF(WARNING, "Failed to add Node, as this Node '%s' was removed", name().data());
    return fit::as_error(fdf::wire::NodeError::kNodeRemoved);
  }
  if (NodeStateIsExiting(node_state_)) {
    LOGF(WARNING, "Failed to add Node, as this Node '%s' is being removed", name().c_str());
    return fit::as_error(fdf::wire::NodeError::kNodeRemoved);
  }
  if (!args.name().has_value()) {
    LOGF(ERROR, "Failed to add Node, a name must be provided");
    return fit::as_error(fdf::wire::NodeError::kNameMissing);
  }
  std::string_view name = args.name().value();
  for (auto& child : children_) {
    if (child->name() == name) {
      LOGF(ERROR, "Failed to add Node '%.*s', name already exists among siblings",
           static_cast<int>(name.size()), name.data());
      return fit::as_error(fdf::wire::NodeError::kNameAlreadyExists);
    }
  };
  zx::vmo inspect_vmo;
  if (args.devfs_args().has_value() && args.devfs_args()->inspect().has_value()) {
    inspect_vmo = std::move(args.devfs_args()->inspect().value());
  }
  DeviceInspect inspect = inspect_.CreateChild(std::string(name), std::move(inspect_vmo), 0);
  std::shared_ptr child = std::make_shared<Node>(name, std::vector<Node*>{this}, *node_manager_,
                                                 dispatcher_, std::move(inspect));

  if (args.offers().has_value()) {
    child->offers_.reserve(args.offers().value().size());

    // Find a parent node with a collection. This indicates that a driver has
    // been bound to the node, and the driver is running within the collection.
    Node* source_node = this;
    while (source_node && source_node->collection_ == Collection::kNone) {
      source_node = source_node->GetPrimaryParent();
    }
    fdecl::Ref source_ref =
        fdecl::Ref::WithChild(fdecl::ChildRef()
                                  .name(source_node->MakeComponentMoniker())
                                  .collection(CollectionName(source_node->collection_)));

    for (auto& offer : args.offers().value()) {
      fit::result new_offer = AddOfferToNodeOffer(offer, source_ref);
      if (new_offer.is_error()) {
        LOGF(ERROR, "Failed to add Node '%s': Bad add offer: %d",
             child->MakeTopologicalPath().c_str(), new_offer.error_value());
        return new_offer.take_error();
      }
      child->offers_.push_back(fidl::ToWire(child->arena_, new_offer.value()));
    }
  }

  if (args.properties().has_value()) {
    child->properties_.reserve(args.properties()->size() + 1);  // + 1 for DFv2 prop.
    for (auto& property : args.properties().value()) {
      child->properties_.emplace_back(fidl::ToWire(child->arena_, property));
    }
  }

  // We set a property for DFv2 devices.
  child->properties_.emplace_back(
      fdf::MakeProperty(child->arena_, "fuchsia.driver.framework.dfv2", true));

  child->SetAndPublishInspect();

  if (args.symbols().has_value()) {
    auto is_valid = ValidateSymbols(args.symbols().value());
    if (is_valid.is_error()) {
      LOGF(ERROR, "Failed to add Node '%.*s', bad symbols", static_cast<int>(name.size()),
           name.data());
      return fit::as_error(is_valid.error_value());
    }

    child->symbols_.reserve(args.symbols().value().size());
    for (auto& symbol : args.symbols().value()) {
      child->symbols_.emplace_back(fdf::wire::NodeSymbol::Builder(child->arena_)
                                       .name(child->arena_, symbol.name().value())
                                       .address(symbol.address().value())
                                       .Build());
    }
  }

  Devnode::Target devfs_target = Devnode::NoRemote();
  std::optional<std::string_view> devfs_class_path;
  if (args.devfs_args().has_value() && args.devfs_args()->connector().has_value()) {
    if (args.devfs_args()->class_name().has_value()) {
      devfs_class_path = args.devfs_args()->class_name();
    }
    devfs_target = Devnode::Connector{
        .connector = fidl::WireSharedClient<fuchsia_device_fs::Connector>(
            std::move(args.devfs_args().value().connector().value()), dispatcher_),
    };
  }
  ZX_ASSERT(devfs_device_.topological_node().has_value());
  zx_status_t status = devfs_device_.topological_node()->add_child(
      child->name_, std::move(devfs_class_path), std::move(devfs_target), child->devfs_device_);
  ZX_ASSERT_MSG(status == ZX_OK, "%s failed to export: %s", child->MakeTopologicalPath().c_str(),
                zx_status_get_string(status));
  ZX_ASSERT(child->devfs_device_.topological_node().has_value());
  child->devfs_device_.publish();

  if (controller.is_valid()) {
    child->controller_ref_.emplace(dispatcher_, std::move(controller), child.get(),
                                   fidl::kIgnoreBindingClosure);
  }
  if (node.is_valid()) {
    child->node_ref_.emplace(dispatcher_, std::move(node), child.get(), [](Node* node, auto) {
      node->node_ref_.reset();
      LOGF(WARNING, "Removing node %s because of binding closed", node->name().c_str());
      node->Remove(RemovalSet::kAll, nullptr);
    });
  } else {
    // We don't care about tracking binds here, sending nullptr is fine.
    (*node_manager_)->Bind(*child, nullptr);
  }

  child->AddToParents();
  return fit::ok(child);
}

void Node::WaitForChildToExit(
    std::string_view name,
    fit::callback<void(fit::result<fuchsia_driver_framework::wire::NodeError>)> callback) {
  for (auto& child : children_) {
    if (child->name() != name) {
      continue;
    }
    if (!NodeStateIsExiting(child->node_state_)) {
      LOGF(ERROR, "Failed to add Node '%.*s', name already exists among siblings",
           static_cast<int>(name.size()), name.data());
      callback(fit::as_error(fdf::wire::NodeError::kNameAlreadyExists));
      return;
    }
    if (child->remove_complete_callback_) {
      LOGF(ERROR,
           "Failed to add Node '%.*s': Node with name already exists and is marked to be replaced.",
           static_cast<int>(name.size()), name.data());
      callback(fit::as_error(fdf::wire::NodeError::kNameAlreadyExists));
      return;
    }
    child->remove_complete_callback_ = [callback = std::move(callback)]() mutable {
      callback(fit::success());
    };
    return;
  };
  callback(fit::success());
}

void Node::AddChild(fuchsia_driver_framework::NodeAddArgs args,
                    fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller,
                    fidl::ServerEnd<fuchsia_driver_framework::Node> node,
                    AddNodeResultCallback callback) {
  if (!args.name().has_value()) {
    LOGF(ERROR, "Failed to add Node, a name must be provided");
    callback(fit::as_error(fdf::wire::NodeError::kNameMissing));
    return;
  }
  std::string name = args.name().value();
  WaitForChildToExit(
      name, [self = shared_from_this(), args = std::move(args), controller = std::move(controller),
             node = std::move(node), callback = std::move(callback)](
                fit::result<fuchsia_driver_framework::wire::NodeError> result) mutable {
        if (result.is_error()) {
          callback(result.take_error());
          return;
        }
        callback(self->AddChildHelper(std::move(args), std::move(controller), std::move(node)));
      });
}

bool Node::IsComposite() const { return parents_.size() > 1; }

void Node::Remove(RemoveCompleter::Sync& completer) {
  LOGF(DEBUG, "Remove() Fidl call for %s", name().c_str());
  Remove(RemovalSet::kAll, nullptr);
}

void Node::RequestBind(RequestBindRequestView request, RequestBindCompleter::Sync& completer) {
  bool force_rebind = false;
  if (request->has_force_rebind()) {
    force_rebind = request->force_rebind();
  }
  if (driver_component_.has_value() && !force_rebind) {
    completer.ReplyError(ZX_ERR_ALREADY_BOUND);
    return;
  }

  if (request_bind_completer_) {
    completer.ReplyError(ZX_ERR_ALREADY_EXISTS);
    return;
  }

  std::string driver_url_suffix;
  if (request->has_driver_url_suffix()) {
    driver_url_suffix = std::string(request->driver_url_suffix().get());
  }

  request_bind_completer_ = completer.ToAsync();

  if (driver_component_.has_value()) {
    restart_driver_url_suffix_ = driver_url_suffix;
    RestartNode();
    return;
  }

  auto tracker = CreateBindResultTracker();
  node_manager_.value()->BindToUrl(*this, driver_url_suffix, std::move(tracker));
}

void Node::AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) {
  AddChild(fidl::ToNatural(request->args), std::move(request->controller), std::move(request->node),
           [completer = completer.ToAsync()](
               fit::result<fuchsia_driver_framework::wire::NodeError, std::shared_ptr<Node>>
                   result) mutable {
             if (result.is_error()) {
               completer.Reply(result.take_error());
             } else {
               completer.ReplySuccess();
             }
           });
}

void Node::StartDriver(fuchsia_component_runner::wire::ComponentStartInfo start_info,
                       fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
                       fit::callback<void(zx::result<>)> cb) {
  auto url = start_info.resolved_url().get();
  bool colocate = fdf::ProgramValue(start_info.program(), "colocate").value_or("") == "true";

  if (colocate && !driver_host_) {
    LOGF(ERROR,
         "Failed to start driver '%.*s', driver is colocated but does not have a prent with a "
         "driver host",
         static_cast<int>(url.size()), url.data());
    cb(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  auto symbols = fidl::VectorView<fdf::wire::NodeSymbol>();
  if (colocate) {
    symbols = this->symbols();
  }

  // Launch a driver host if we are not colocated.
  if (!colocate) {
    auto result = (*node_manager_)->CreateDriverHost();
    if (result.is_error()) {
      cb(result.take_error());
      return;
    }
    driver_host_ = result.value();
  }

  // Bind the Node associated with the driver.
  auto endpoints = fidl::CreateEndpoints<fdf::Node>();
  if (endpoints.is_error()) {
    cb(zx::error(endpoints.error_value()));
    return;
  }
  node_ref_.emplace(dispatcher_, std::move(endpoints->server), this,
                    [](Node* node, fidl::UnbindInfo info) {
                      node->node_ref_.reset();
                      if (!info.is_user_initiated()) {
                        LOGF(WARNING, "Removing node %s because of fdf::Node binding closed: %s",
                             node->name().c_str(), info.FormatDescription().c_str());
                        node->Remove(RemovalSet::kAll, nullptr);
                      }
                    });

  LOGF(INFO, "Binding %.*s to  %s", static_cast<int>(url.size()), url.data(), name().c_str());
  // Start the driver within the driver host.
  zx::result driver_endpoints = fidl::CreateEndpoints<fuchsia_driver_host::Driver>();
  if (driver_endpoints.is_error()) {
    cb(driver_endpoints.take_error());
    return;
  }
  driver_component_.emplace(*this, std::string(url), std::move(controller),
                            std::move(driver_endpoints->client));
  driver_host_.value()->Start(std::move(endpoints->client), name_, std::move(symbols),
                              std::move(start_info), std::move(driver_endpoints->server),
                              [this, cb = std::move(cb)](zx::result<> result) mutable {
                                if (result.is_error()) {
                                  driver_component_.reset();
                                }
                                cb(result);
                              });
}

void Node::ScheduleStopComponent() {
  ZX_ASSERT_MSG(node_state_ == NodeState::kWaitingOnDriver,
                "ScheduleStopComponent called in invalid node state: %s",
                State2String(node_state_));
  node_state_ = NodeState::kWaitingOnDriverComponent;
  if (!driver_component_) {
    FinishRemoval();
    return;
  }
  // Send an epitaph to the component manager and close the connection. The
  // server of a `ComponentController` protocol is expected to send an epitaph
  // before closing the associated connection.
  auto this_node = shared_from_this();
  driver_component_->component_controller_ref.Close(ZX_OK);
  if (!node_manager_.has_value()) {
    return;
  }
  node_manager_.value()->DestroyDriverComponent(
      *this_node,
      [self = this_node](fidl::WireUnownedResult<fcomponent::Realm::DestroyChild>& result) {
        if (!result.ok()) {
          auto error = result.error().FormatDescription();
          LOGF(ERROR, "Node: %s: Failed to send request to destroy component: %.*s",
               self->name_.c_str(), static_cast<int>(error.size()), error.data());
        }
        if (result->is_error() &&
            result->error_value() != fcomponent::wire::Error::kInstanceNotFound) {
          LOGF(ERROR, "Node: %.*s: Failed to destroy driver component: %u",
               static_cast<int>(self->name_.size()), self->name_.data(), result->error_value());
        }
        self->FinishRemoval();
      });
}

void Node::on_fidl_error(fidl::UnbindInfo info) {
  if (driver_component_) {
    driver_component_->driver = {};
  }
  // The only valid way a driver host should shut down the Driver channel
  // is with the ZX_OK epitaph.
  if (info.reason() != fidl::Reason::kPeerClosedWhileReading || info.status() != ZX_OK) {
    LOGF(ERROR, "Node: %s: driver channel shutdown with: %s", name().c_str(),
         info.FormatDescription().data());
  }
  if (node_state_ == NodeState::kWaitingOnDriver) {
    LOGF(DEBUG, "Node: %s: realm channel had expected shutdown.", name().c_str());
    ScheduleStopComponent();
  } else if (node_state_ == NodeState::kWaitingOnDriverComponent) {
    LOGF(DEBUG, "Node: %s: driver channel had expected shutdown.", name().c_str());
    FinishRemoval();
  } else {
    LOGF(WARNING, "Removing node %s because of unexpected driver channel shutdown.",
         name().c_str());
    Remove(RemovalSet::kAll, nullptr);
  }
}

Node::DriverComponent::DriverComponent(
    Node& node, std::string url,
    fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
    fidl::ClientEnd<fuchsia_driver_host::Driver> driver)
    : component_controller_ref(
          node.dispatcher_, std::move(controller), &node,
          [](Node* node, fidl::UnbindInfo info) {
            if (!info.is_user_initiated()) {
              LOGF(WARNING, "Removing node %s because of ComponentController binding closed: %s",
                   node->name().c_str(), info.FormatDescription().c_str());
              node->Remove(RemovalSet::kAll, nullptr);
            }
          }),
      driver(std::move(driver), node.dispatcher_, &node),
      driver_url(std::move(url)) {}

void Node::SetAndPublishInspect() {
  constexpr char kDeviceTypeString[] = "Device";
  constexpr char kCompositeDeviceTypeString[] = "Composite Device";

  std::vector<zx_device_prop_t> property_vector;
  for (auto& property : properties_) {
    if (property.key.is_int_value() && property.value.is_int_value()) {
      property_vector.push_back(zx_device_prop_t{
          .id = static_cast<uint16_t>(property.key.int_value()),
          .value = property.value.int_value(),
      });
    }
  }

  inspect_.SetStaticValues(MakeTopologicalPath(), GetProtocolId(properties_),
                           IsComposite() ? kCompositeDeviceTypeString : kDeviceTypeString, 0,
                           property_vector, "");
  if (zx::result result = inspect_.Publish(); result.is_error()) {
    LOGF(ERROR, "%s: Failed to publish inspect: %s", MakeTopologicalPath().c_str(),
         result.status_string());
  }
}

}  // namespace dfv2
