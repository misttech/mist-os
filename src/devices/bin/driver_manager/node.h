// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_NODE_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_NODE_H_

#include <fidl/fuchsia.component.runner/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.driver.host/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <lib/zx/event.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <list>
#include <memory>
#include <utility>

#include "lib/fidl/cpp/wire/client.h"
#include "lib/fidl/cpp/wire/internal/transport_channel.h"
#include "src/devices/bin/driver_manager/bind/bind_result_tracker.h"
#include "src/devices/bin/driver_manager/controller_allowlist_passthrough.h"
#include "src/devices/bin/driver_manager/devfs/devfs.h"
#include "src/devices/bin/driver_manager/driver_host.h"
#include "src/devices/bin/driver_manager/inspect.h"
#include "src/devices/bin/driver_manager/shutdown/shutdown_helper.h"

namespace driver_manager {

using NodeOffer = fuchsia_driver_framework::wire::Offer;

enum class OfferTransport : std::uint8_t {
  DriverTransport,
  ZirconTransport,
};

zx::result<std::pair<fuchsia_component_decl::wire::Offer, OfferTransport>> GetInnerOffer(
    const NodeOffer& offer);

// This function creates a composite offer based on a service offer.
std::optional<fuchsia_component_decl::wire::Offer> CreateCompositeServiceOffer(
    fidl::AnyArena& arena, fuchsia_component_decl::wire::Offer& offer,
    std::string_view parents_name, bool primary_parent);

class Node;
class NodeRemovalTracker;

using AddNodeResultCallback = fit::callback<void(
    fit::result<fuchsia_driver_framework::wire::NodeError, std::shared_ptr<Node>>)>;

using DestroyDriverComponentCallback =
    fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>;

class NodeManager {
 public:
  virtual ~NodeManager() = default;

  // Attempt to bind `node`.
  // A nullptr for result_tracker is acceptable if the caller doesn't intend to
  // track the results.
  virtual void Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) = 0;

  virtual void BindToUrl(Node& node, std::string_view driver_url_suffix,
                         std::shared_ptr<BindResultTracker> result_tracker) {
    ZX_PANIC("Unimplemented BindToUrl");
  }

  virtual zx::result<> StartDriver(Node& node, std::string_view url,
                                   fuchsia_driver_framework::DriverPackageType package_type) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  virtual zx::result<DriverHost*> CreateDriverHost(bool use_next_vdso) = 0;

  // Creates the driver host component, loads the driver host using dynamic linking,
  // and calls |cb| on completion or error.
  virtual void CreateDriverHostDynamicLinker(
      fit::callback<void(zx::result<DriverHost*>)> completion_cb) {
    completion_cb(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  // DriverHost lifetimes are managed through a linked list, and they will delete themselves
  // when the FIDL connection is closed. Currently in the Node class we store a raw pointer to the
  // DriverHost object, and do not have a way to remove it from the class when the underlying
  // DriverHost object is deallocated. This function will return true if the underlying DriverHost
  // object is still alive and in the linked list. Otherwise returns false.
  virtual bool IsDriverHostValid(DriverHost* driver_host) const { return true; }

  // Destroys the dynamic child component that runs the driver associated with
  // `node`.
  virtual void DestroyDriverComponent(Node& node, DestroyDriverComponentCallback callback) = 0;

  virtual void RebindComposite(std::string spec, std::optional<std::string> driver_url,
                               fit::callback<void(zx::result<>)> callback) {}

  virtual bool IsTestShutdownDelayEnabled() const { return false; }
  virtual std::weak_ptr<std::mt19937> GetShutdownTestRng() const {
    return std::weak_ptr<std::mt19937>();
  }
};

enum class Collection : uint8_t {
  kNone,
  // Collection for boot drivers.
  kBoot,
  // Collection for package drivers.
  kPackage,
  // Collection for universe package drivers.
  kFullPackage,
};

enum class NodeType {
  kNormal,     // Normal non-composite node.
  kComposite,  // Composite node created from composite node specs.
};

enum class DriverState : uint8_t {
  kStopped,  // Driver is not running.
  kBinding,  // Driver's bind hook is scheduled and running.
  kRunning,  // Driver finished binding and is running.
};

class Node : public fidl::WireServer<fuchsia_driver_framework::NodeController>,
             public fidl::WireServer<fuchsia_driver_framework::Node>,
             public fidl::WireServer<fuchsia_component_runner::ComponentController>,
             public fidl::WireServer<fuchsia_device::Controller>,
             public fidl::WireAsyncEventHandler<fuchsia_driver_host::Driver>,
             public std::enable_shared_from_this<Node>,
             public NodeShutdownBridge {
 public:
  Node(std::string_view name, std::vector<std::weak_ptr<Node>> parents, NodeManager* node_manager,
       async_dispatcher_t* dispatcher, DeviceInspect inspect, uint32_t primary_index = 0,
       NodeType type = NodeType::kNormal);

  ~Node() override;

  static zx::result<std::shared_ptr<Node>> CreateCompositeNode(
      std::string_view node_name, std::vector<std::weak_ptr<Node>> parents,
      std::vector<std::string> parents_names,
      const std::vector<fuchsia_driver_framework::NodePropertyEntry2>& parent_properties,
      NodeManager* driver_binder, async_dispatcher_t* dispatcher, uint32_t primary_index = 0);

  // This is called when |node_ref_| is unbound from the dispatcher.
  void OnNodeServerUnbound(fidl::UnbindInfo info);

  // NodeShutdownBridge
  // Exposed for testing.
  bool HasDriverComponent() const override {
    return driver_component_.has_value() && driver_component_->state != DriverState::kStopped;
  }

  void OnBind() const;

  bool is_bound() const { return driver_component_.has_value(); }

  // Begin the removal process for a Node. This function ensures that a Node is
  // only removed after all of its children are removed. It also ensures that
  // a Node is only removed after the driver that is bound to it has been stopped.
  // This is safe to call multiple times.
  // There are multiple reasons a Node's removal will be started:
  //   - The system is being stopped.
  //   - The Node had an unexpected error or disconnect
  // During a system stop, Remove is expected to be called twice:
  // once with |removal_set| == kPackage, and once with |removal_set| == kAll.
  // Errors and disconnects that are unrecoverable should call Remove(kAll, nullptr).
  void Remove(RemovalSet removal_set, NodeRemovalTracker* removal_tracker);

  // `callback` is invoked once the node has finished being added or an error
  // has occurred.
  void AddChild(fuchsia_driver_framework::NodeAddArgs args,
                fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller,
                fidl::ServerEnd<fuchsia_driver_framework::Node> node,
                AddNodeResultCallback callback);

  // Add this Node to its parents. This should be called when the node is created. Exposed for
  // testing.
  void AddToParents();

  // Begins the process of restarting the node. Restarting a node includes stopping and removing
  // all children nodes, stopping the driver that is bound to the node, and asking the NodeManager
  // to bind the node again. The restart operation is very similar to the Remove operation, the
  // difference being once the children are removed, and the driver stopped, we don't remove the
  // node from the topology but instead bind the node again.
  void RestartNode();

  // Begins the process of quarantining the node. This is basically performing a Remove,
  // but instead of removing the node from the topology, we keep it in a stopped state so that it
  // can be orphaned if its driver is ever disabled. That way new drivers can be bound to the node.
  void QuarantineNode();

  void RemoveCompositeNodeForRebind(fit::callback<void(zx::result<>)> completer);

  // Restarting a node WithRematch, means that instead of re-using the currently bound driver,
  // another MatchDriver call will be made into the driver index to find a new driver to bind.
  void RestartNodeWithRematch(std::optional<std::string> restart_driver_url_suffix,
                              fit::callback<void(zx::result<>)> completer);
  void RestartNodeWithRematch();

  void StartDriver(fuchsia_component_runner::wire::ComponentStartInfo start_info,
                   fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
                   fit::callback<void(zx::result<>)> cb);

  bool IsComposite() const { return type_ == NodeType::kComposite; }

  // Exposed for testing.
  // Set properties to non-composite node properties containing a clone of `properties` and
  // "DRIVER_FRAMEWORK_VERSION == 2".
  void SetNonCompositeProperties(
      cpp20::span<const fuchsia_driver_framework::NodeProperty2> properties);

  // Evaluates the given rematch_flags against the node. Returns true if rematch should take place,
  // false otherwise. Rematching is done based on the node type and url both matching:
  // For node type, if the node is a composite, the rematch flags must contain the flag
  // for the composite variant that the node is. No validation for non-composites.
  // For the url, rematch takes place if either:
  //  - the url matches the requested_url and the 'requested' flag is available.
  //  - the url does not match and the 'non_requested' flag is available.
  bool EvaluateRematchFlags(fuchsia_driver_development::RestartRematchFlags rematch_flags,
                            std::string_view requested_url);

  // Creates the node's topological path by combining each primary parent's name together,
  // separated by '/'.
  // E.g: dev/sys/topo/path
  std::string MakeTopologicalPath() const;

  // Make the node's component moniker by making the topological path and then replacing
  // characters not allowed by the component framework.
  // E.g: dev.sys.topo.path
  std::string MakeComponentMoniker() const;

  // Exposed for testing.
  Node* GetPrimaryParent() const {
    return parents_.empty() ? nullptr : parents_[primary_index_].lock().get();
  }

  // This should be used on the root node. Install the root node at the top of the devfs filesystem.
  void SetupDevfsForRootNode(std::optional<Devfs>& devfs) {
    devfs.emplace(devfs_device_.topological_node());
  }

  // This is exposed for testing. Setup this node's devfs nodes.
  void AddToDevfsForTesting(Devnode& parent) {
    parent.add_child(name_, std::nullopt, Devnode::Target(), devfs_device_);
  }

  // Invoked when a bind sequence has been completed. It allows us to reply to outstanding bind
  // requests that may have originated from the node.
  void CompleteBind(zx::result<> result);

  ShutdownHelper& GetShutdownHelper();

  NodeState GetNodeState() { return GetShutdownHelper().node_state(); }

  const std::string& name() const { return name_; }

  NodeType type() const { return type_; }

  const DriverHost* driver_host() const {
    if (node_manager_.has_value() && node_manager_.value()->IsDriverHostValid(*driver_host_)) {
      return *driver_host_;
    }

    return nullptr;
  }

  const std::string& driver_url() const;

  bool quarantined() const {
    return !driver_component_.has_value() && quarantine_driver_url_.has_value();
  }

  const std::vector<std::weak_ptr<Node>>& parents() const { return parents_; }

  const std::list<std::shared_ptr<Node>>& children() const { return children_; }

  fidl::ArenaBase& arena() { return arena_; }

  const std::vector<NodeOffer>& offers() const { return offers_; }

  // TODO(https://fxbug.dev/42160282): Remove const_cast once VectorView supports const.
  fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols() const {
    return fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol>::FromExternal(
        const_cast<decltype(symbols_) &>(symbols_));
  }

  // Returns the node properties of the node and its parents if the node is a composite node.
  // See `properties_` property for more info.
  const fuchsia_driver_framework::wire::NodePropertyDictionary2& properties() const {
    return properties_;
  }

  // Returns the node properties of the node or the node's parent if the node is a composite node.
  // Returns std::nullopt if the node is a non-composite and `parent_name` is not "default".
  // Returns std::nullopt if the parent node cannot be found.
  // See `properties_` property for more info.
  std::optional<cpp20::span<const fuchsia_driver_framework::wire::NodeProperty2>> GetNodeProperties(
      std::string_view parent_name = "default") const;

  std::optional<std::vector<fuchsia_driver_framework::NodeProperty>> GetDeprecatedNodeProperties(
      std::string_view parent_name = "default") const;

  const Collection& collection() const { return collection_; }

  const fuchsia_driver_framework::DriverPackageType& driver_package_type() const {
    return driver_package_type_;
  }

  DevfsDevice& devfs_device() { return devfs_device_; }

  bool can_multibind_composites() const { return can_multibind_composites_; }

  void set_collection(Collection collection) { collection_ = collection; }

  void set_driver_package_type(fuchsia_driver_framework::DriverPackageType driver_package_type) {
    driver_package_type_ = driver_package_type;
  }

  void set_symbols(std::vector<fuchsia_driver_framework::wire::NodeSymbol> symbols) {
    symbols_ = std::move(symbols);
  }

  void set_can_multibind_composites(bool can_multibind_composites) {
    can_multibind_composites_ = can_multibind_composites;
  }

  std::optional<zx_koid_t> token_koid() const {
    return driver_component_.has_value() ? std::optional(driver_component_->component_instance_koid)
                                         : std::nullopt;
  }
  std::vector<fuchsia_driver_framework::BusInfo> GetBusTopology() const;

  ShutdownIntent shutdown_intent() { return GetShutdownHelper().shutdown_intent(); }

 private:
  struct DriverComponent {
    DriverComponent(Node& node, std::string url,
                    fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
                    fidl::ClientEnd<fuchsia_driver_host::Driver> driver,
                    zx::event component_instance);

    // This represents the Driver Component within the Component Framework.
    // When this is closed with an epitaph it signals to the Component Framework
    // that this driver component has stopped.
    fidl::ServerBinding<fuchsia_component_runner::ComponentController> component_controller_ref;
    fidl::WireClient<fuchsia_driver_host::Driver> driver;
    std::string driver_url;
    zx::event component_instance;
    zx_koid_t component_instance_koid = 0;

    DriverState state = DriverState::kBinding;
  };

  // fidl::WireServer<fuchsia_device::Controller>
  void ConnectToDeviceFidl(ConnectToDeviceFidlRequestView request,
                           ConnectToDeviceFidlCompleter::Sync& completer) override;
  void ConnectToController(ConnectToControllerRequestView request,
                           ConnectToControllerCompleter::Sync& completer) override;
  void Bind(BindRequestView request, BindCompleter::Sync& completer) override;
  void Rebind(RebindRequestView request, RebindCompleter::Sync& completer) override;
  void UnbindChildren(UnbindChildrenCompleter::Sync& completer) override;
  void ScheduleUnbind(ScheduleUnbindCompleter::Sync& completer) override;
  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override;

  // This is called when fuchsia_driver_host::Driver is closed.
  void on_fidl_error(fidl::UnbindInfo info) override;

  // fidl::WireServer<fuchsia_component_runner::ComponentController>
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_component_runner::ComponentController> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;
  // We ignore these signals.
  void Stop(StopCompleter::Sync& completer) override;
  void Kill(KillCompleter::Sync& completer) override;

  // fidl::WireServer<fuchsia_driver_framework::NodeController>
  void Remove(RemoveCompleter::Sync& completer) override;
  void RequestBind(RequestBindRequestView request, RequestBindCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_driver_framework::NodeController> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fidl::WireServer<fuchsia_driver_framework::Node>
  void AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_driver_framework::Node> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  // NodeShutdownBridge
  std::pair<std::string, Collection> GetRemovalTrackerInfo() override;
  void StopDriver() override;
  void StopDriverComponent() override;
  void FinishShutdown(fit::callback<void()> shutdown_callback) override;
  bool HasChildren() const override { return !children_.empty(); }
  bool HasDriver() const override {
    return driver_component_.has_value() && driver_component_->driver;
  }

  bool IsPendingBind() const override {
    if (!driver_component_) {
      return false;
    }
    return driver_component_->driver && driver_component_->state == DriverState::kBinding;
  }

  void BindHelper(bool force_rebind, std::optional<std::string> driver_url_suffix,
                  fit::callback<void(zx_status_t)> on_bind_complete);

  // Shutdown helpers:
  // Remove a child from this parent
  void RemoveChild(const std::shared_ptr<Node>& child);

  // Start the node's driver back up.
  void FinishRestart();

  // Called to wrap things up before going to the quarantine state.
  void FinishQuarantine();

  // Clear out the values associated with the driver on the driver host.
  void ClearHostDriver();

  // Call `callback` once child node with the name `name` has been removed.
  // Returns an error if a child node with the name `name` exists and is not in
  // the process of being removed.
  void WaitForChildToExit(
      std::string_view name,
      fit::callback<void(fit::result<fuchsia_driver_framework::wire::NodeError>)> callback);

  std::shared_ptr<BindResultTracker> CreateBindResultTracker();

  fit::result<fuchsia_driver_framework::wire::NodeError, std::shared_ptr<Node>> AddChildHelper(
      fuchsia_driver_framework::NodeAddArgs args,
      fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller,
      fidl::ServerEnd<fuchsia_driver_framework::Node> node);

  // Set the inspect data and publish it. This should be called once as the node is being created.
  void SetAndPublishInspect();

  // Creates a passthrough for the associated devfs node that will connect to
  // the device controller of this node if no connector provided, or the connection
  // type is not supported.
  Devnode::Target CreateDevfsPassthrough(
      std::optional<fidl::ClientEnd<fuchsia_device_fs::Connector>> connector,
      std::optional<fidl::ClientEnd<fuchsia_device_fs::Connector>> controller_connector,
      bool allow_controller_connection, const std::string& class_name);

  zx_status_t ConnectControllerInterface(fidl::ServerEnd<fuchsia_device::Controller> server_end);
  zx_status_t ConnectDeviceInterface(zx::channel channel);

  // Set properties to composite node properties containing a clone of the node properties of
  // `parents_`.
  void SetCompositeParentProperties(
      const std::vector<fuchsia_driver_framework::NodePropertyEntry2>& parent_properties);

  // Update `properties_dict_` to identify the contents of `properties_`.
  void SynchronizePropertiesDict();

  void StartDriverWithDynamicLinker(
      DriverHost::DriverLoadArgs load_args, DriverHost::DriverStartArgs start_args,
      std::string_view url,
      fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
      fit::callback<void(zx::result<>)> cb);

  std::string name_;

  NodeType type_;

  // If this is a composite device, this stores the list of each parent's names.
  std::vector<std::string> parents_names_;
  std::vector<std::weak_ptr<Node>> parents_;
  uint32_t primary_index_ = 0;
  std::list<std::shared_ptr<Node>> children_;
  fit::nullable<NodeManager*> node_manager_;
  async_dispatcher_t* const dispatcher_;

  // TODO(https://fxbug.dev/42073492): Set this flag from NodeAddArgs.
  bool can_multibind_composites_ = true;

  bool host_restart_on_crash_ = false;

  fidl::Arena<128> arena_;
  std::vector<NodeOffer> offers_;
  std::vector<fuchsia_driver_framework::wire::NodeSymbol> symbols_;

  // Contains the properties of the node or its parents if the node is a composite or legacy
  // composite node. "default" entry refers to the node's properties if the node is a
  // non-composite. "default" entry refers to the primary parent node's properties if the node is a
  // composite. All referenced data is owned by `arena_`. Make sure to call
  // `Node::SynchronizePropertiesDict()` when modified.
  fuchsia_driver_framework::wire::NodePropertyDictionary2 properties_;

  // Maps the node properties of the entries of `properties_` by their name.
  std::unordered_map<std::string, cpp20::span<const fuchsia_driver_framework::wire::NodeProperty2>>
      properties_dict_;

  std::optional<fuchsia_driver_framework::BusInfo> bus_info_;

  Collection collection_ = Collection::kNone;
  fuchsia_driver_framework::DriverPackageType driver_package_type_;
  fit::nullable<DriverHost*> driver_host_;

  // An outstanding rebind request.
  std::optional<fit::callback<void(zx::result<>)>> pending_bind_completer_;

  // Set by RemoveCompositeNodeForRebind(). Invoked when the node finished shutting down.
  std::optional<fit::callback<void(zx::result<>)>> composite_rebind_completer_;

  std::optional<std::string> restart_driver_url_suffix_;

  // Invoked when the node has been fully removed.
  fit::callback<void()> remove_complete_callback_;

  std::optional<DriverComponent> driver_component_;
  std::optional<std::string> quarantine_driver_url_;
  std::optional<fidl::ServerBinding<fuchsia_driver_framework::Node>> node_ref_;
  std::optional<fidl::ServerBinding<fuchsia_driver_framework::NodeController>> controller_ref_;

  // The device's inspect information.
  DeviceInspect inspect_;

  std::unique_ptr<ShutdownHelper> shutdown_helper_;

  // This represents the node's presence in devfs, both it's topological path and it's class path.
  DevfsDevice devfs_device_;

  // Connector to service exported to devfs
  std::optional<fidl::ClientEnd<fuchsia_device_fs::Connector>> devfs_connector_;
  // Connector to fuchsia_device/Controller exported to devfs.
  // This is only used by the compat shim to override the node's own controller
  // implementation.
  fidl::ServerBindingGroup<fuchsia_device::Controller> dev_controller_bindings_;
  std::unique_ptr<ControllerAllowlistPassthrough> controller_allowlist_passthrough_;

  // Completers that are waiting for the node to unbind all of its children.
  std::vector<UnbindChildrenCompleter::Async> unbinding_children_completers_;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_NODE_H_
