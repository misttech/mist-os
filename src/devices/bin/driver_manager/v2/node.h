// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_NODE_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_NODE_H_

#include <fidl/fuchsia.component.runner/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.driver.host/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <list>
#include <memory>
#include <stack>

#include "lib/fidl/cpp/wire/internal/transport_channel.h"
#include "src/devices/bin/driver_manager/devfs/devfs.h"
#include "src/devices/bin/driver_manager/inspect.h"
#include "src/devices/bin/driver_manager/v2/driver_host.h"

namespace dfv2 {

// This function creates a composite offer based on a service offer.
std::optional<fuchsia_component_decl::wire::Offer> CreateCompositeServiceOffer(
    fidl::AnyArena& arena, fuchsia_component_decl::wire::Offer& offer,
    std::string_view parents_name, bool primary_parent);

class Node;
class NodeRemovalTracker;
using NodeId = uint32_t;

using NodeBindingInfoResultCallback =
    fit::callback<void(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>)>;

using AddNodeResultCallback = fit::callback<void(
    fit::result<fuchsia_driver_framework::wire::NodeError, std::shared_ptr<Node>>)>;

using DestroyDriverComponentCallback =
    fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>;

// Used to track binding results. Once the tracker reaches the expected result count, it invokes the
// callback. The expected result count must be greater than 0.
class BindResultTracker {
 public:
  explicit BindResultTracker(size_t expected_result_count,
                             NodeBindingInfoResultCallback result_callback);

  void ReportSuccessfulBind(const std::string_view& node_name, const std::string_view& driver);
  void ReportNoBind();

 private:
  void Complete(size_t current);
  fidl::Arena<> arena_;
  size_t expected_result_count_;
  size_t currently_reported_ TA_GUARDED(lock_);
  std::mutex lock_;
  NodeBindingInfoResultCallback result_callback_;
  std::vector<fuchsia_driver_development::wire::NodeBindingInfo> results_;
};

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
                                   fuchsia_driver_index::DriverPackageType package_type) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  virtual zx::result<DriverHost*> CreateDriverHost() = 0;

  // Destroys the dynamic child component that runs the driver associated with
  // `node`.
  virtual void DestroyDriverComponent(Node& node, DestroyDriverComponentCallback callback) = 0;
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

enum class RemovalSet {
  kAll,      // Remove the boot drivers and the package drivers
  kPackage,  // Remove the package drivers
};

enum class NodeState {
  kRunning,                   // Normal running state.
  kPrestop,                   // Still running, but will remove soon. usually because
                              //  Received Remove(kPackage), but is a boot driver.
  kWaitingOnChildren,         // Received Remove, and waiting for children to be removed.
  kWaitingOnDriver,           // Waiting for driver to respond from Stop() command.
  kWaitingOnDriverComponent,  // Waiting driver component to be destroyed.
  kStopping,                  // finishing shutdown of node.
};
class Node : public fidl::WireServer<fuchsia_driver_framework::NodeController>,
             public fidl::WireServer<fuchsia_driver_framework::Node>,
             public fidl::WireServer<fuchsia_component_runner::ComponentController>,
             public fidl::WireServer<fuchsia_device::Controller>,
             public fidl::WireAsyncEventHandler<fuchsia_driver_host::Driver>,
             public std::enable_shared_from_this<Node> {
 public:
  Node(std::string_view name, std::vector<Node*> parents, NodeManager* node_manager,
       async_dispatcher_t* dispatcher, DeviceInspect inspect, uint32_t primary_index = 0);
  Node(std::string_view name, std::vector<Node*> parents, NodeManager* node_manager,
       async_dispatcher_t* dispatcher, DeviceInspect inspect, DriverHost* driver_host);

  ~Node() override;

  static zx::result<std::shared_ptr<Node>> CreateCompositeNode(
      std::string_view node_name, std::vector<Node*> parents,
      std::vector<std::string> parents_names,
      std::vector<fuchsia_driver_framework::wire::NodeProperty> properties,
      NodeManager* driver_binder, async_dispatcher_t* dispatcher, uint32_t primary_index = 0);

  void OnBind() const;

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
  static void Remove(std::stack<std::shared_ptr<Node>> nodes, RemovalSet removal_set,
                     NodeRemovalTracker* removal_tracker);

  // `callback` is invoked once the node has finished being added or an error
  // has occurred.
  void AddChild(fuchsia_driver_framework::NodeAddArgs args,
                fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller,
                fidl::ServerEnd<fuchsia_driver_framework::Node> node,
                AddNodeResultCallback callback);

  // Begins the process of restarting the node. Restarting a node includes stopping and removing
  // all children nodes, stopping the driver that is bound to the node, and asking the NodeManager
  // to bind the node again. The restart operation is very similar to the Remove operation, the
  // difference being once the children are removed, and the driver stopped, we don't remove the
  // node from the topology but instead bind the node again.
  void RestartNode();
  void RestartNode(std::optional<std::string> restart_driver_url_suffix,
                   fit::callback<void(zx::result<>)> completer);

  void StartDriver(fuchsia_component_runner::wire::ComponentStartInfo start_info,
                   fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
                   fit::callback<void(zx::result<>)> cb);

  bool IsComposite() const;

  // Creates the node's topological path by combining each primary parent's name together,
  // separated by '/'.
  // E.g: dev/sys/topo/path
  std::string MakeTopologicalPath() const;

  // Make the node's component moniker by making the topological path and then replacing
  // characters not allowed by the component framework.
  // E.g: dev.sys.topo.path
  std::string MakeComponentMoniker() const;

  // Exposed for testing.
  Node* GetPrimaryParent() const;

  // This should be used on the root node. Install the root node at the top of the devfs filesystem.
  void SetupDevfsForRootNode(
      std::optional<Devfs>& devfs,
      std::optional<fidl::ClientEnd<fuchsia_io::Directory>> diagnostics = {}) {
    devfs.emplace(devfs_device_.topological_node(), std::move(diagnostics));
  }

  // This is exposed for testing. Setup this node's devfs nodes.
  void AddToDevfsForTesting(Devnode& parent) {
    parent.add_child(name_, std::nullopt, Devnode::Target(), devfs_device_);
  }

  // Invoked when a bind sequence has been completed. It allows us to reply to outstanding bind
  // requests that may have originated from the node.
  void CompleteBind(zx::result<> result);

  const std::string& name() const { return name_; }
  const DriverHost* driver_host() const { return *driver_host_; }
  const std::string& driver_url() const;
  const std::vector<Node*>& parents() const;
  const std::list<std::shared_ptr<Node>>& children() const;
  fidl::ArenaBase& arena() { return arena_; }
  fidl::VectorView<fuchsia_component_decl::wire::Offer> offers() const;
  fidl::VectorView<fuchsia_driver_framework::wire::NodeSymbol> symbols() const;
  const std::vector<fuchsia_driver_framework::wire::NodeProperty>& properties() const;
  const Collection& collection() const { return collection_; }
  DevfsDevice& devfs_device() { return devfs_device_; }

  // Exposed for testing.
  bool has_driver_component() { return driver_component_.has_value(); }

  // Exposed for testing.
  NodeState node_state() const { return node_state_; }

  bool can_multibind_composites() const { return can_multibind_composites_; }

  void set_collection(Collection collection);
  void set_offers(std::vector<fuchsia_component_decl::wire::Offer> offers) {
    offers_ = std::move(offers);
  }
  void set_symbols(std::vector<fuchsia_driver_framework::wire::NodeSymbol> symbols) {
    symbols_ = std::move(symbols);
  }

  // Exposed for testing.
  void set_properties(std::vector<fuchsia_driver_framework::wire::NodeProperty> properties) {
    properties_ = std::move(properties);
  }

  void set_can_multibind_composites(bool can_multibind_composites) {
    can_multibind_composites_ = can_multibind_composites;
  }

 private:
  struct DriverComponent {
    DriverComponent(Node& node, std::string url,
                    fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller,
                    fidl::ClientEnd<fuchsia_driver_host::Driver> driver);

    // This represents the Driver Component within the Component Framework.
    // When this is closed with an epitaph it signals to the Component Framework
    // that this driver component has stopped.
    fidl::ServerBinding<fuchsia_component_runner::ComponentController> component_controller_ref;
    fidl::WireClient<fuchsia_driver_host::Driver> driver;
    std::string driver_url;
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
  void GetMinDriverLogSeverity(GetMinDriverLogSeverityCompleter::Sync& completer) override;
  void GetCurrentPerformanceState(GetCurrentPerformanceStateCompleter::Sync& completer) override;
  void SetMinDriverLogSeverity(SetMinDriverLogSeverityRequestView request,
                               SetMinDriverLogSeverityCompleter::Sync& completer) override;
  void SetPerformanceState(SetPerformanceStateRequestView request,
                           SetPerformanceStateCompleter::Sync& completer) override;

  // This is called when fuchsia_driver_framework::Driver is closed.
  void on_fidl_error(fidl::UnbindInfo info) override;
  // fidl::WireServer<fuchsia_component_runner::ComponentController>
  // We ignore these signals.
  void Stop(StopCompleter::Sync& completer) override;
  void Kill(KillCompleter::Sync& completer) override;
  // fidl::WireServer<fuchsia_driver_framework::NodeController>
  void Remove(RemoveCompleter::Sync& completer) override;
  void RequestBind(RequestBindRequestView request, RequestBindCompleter::Sync& completer) override;
  // fidl::WireServer<fuchsia_driver_framework::Node>
  void AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) override;

  // Add this Node to its parents. This should be called when the node is created.
  void AddToParents();

  // Shutdown helpers:
  // Remove a child from this parent
  void RemoveChild(const std::shared_ptr<Node>& child);
  // Check to see if all children have been removed.  If so, move on to stopping driver.
  void CheckForRemoval();
  // Close the component connection to signal to CF that the component has
  // stopped and ensure the component is destroyed.
  void ScheduleStopComponent();
  // Cleanup and remove node. Called by `ScheduleStopComponent` once the
  // associated component has been removed. If |node_restarting_|, it will start the driver
  // of the node again instead of removing it from the node topology.
  void FinishRemoval();
  // Start the node's driver back up.
  void FinishRestart();
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
  // the device controller of this node.
  Devnode::Target CreateDevfsPassthrough();

  std::string name_;

  // If this is a composite device, this stores the list of each parent's names.
  std::vector<std::string> parents_names_;
  std::vector<Node*> parents_;
  uint32_t primary_index_ = 0;
  std::list<std::shared_ptr<Node>> children_;
  fit::nullable<NodeManager*> node_manager_;
  async_dispatcher_t* const dispatcher_;

  // TODO(fxb/122531): Set this flag from NodeAddArgs.
  bool can_multibind_composites_ = true;

  fidl::Arena<128> arena_;
  std::vector<fuchsia_component_decl::wire::Offer> offers_;
  std::vector<fuchsia_driver_framework::wire::NodeSymbol> symbols_;
  std::vector<fuchsia_driver_framework::wire::NodeProperty> properties_;

  Collection collection_ = Collection::kNone;
  fit::nullable<DriverHost*> driver_host_;

  NodeState node_state_ = NodeState::kRunning;
  std::optional<NodeId> removal_id_;
  NodeRemovalTracker* removal_tracker_ = nullptr;
  bool node_restarting_ = false;
  // An outstanding rebind request.
  std::optional<fit::callback<void(zx::result<>)>> pending_bind_completer_;

  std::optional<std::string> restart_driver_url_suffix_;

  // Invoked when the node has been fully removed.
  fit::callback<void()> remove_complete_callback_;
  std::optional<DriverComponent> driver_component_;
  std::optional<fidl::ServerBinding<fuchsia_driver_framework::Node>> node_ref_;
  std::optional<fidl::ServerBinding<fuchsia_driver_framework::NodeController>> controller_ref_;

  // The device's inspect information.
  DeviceInspect inspect_;

  // This represents the node's presence in devfs, both it's topological path and it's class path.
  DevfsDevice devfs_device_;

  fidl::ServerBindingGroup<fuchsia_device::Controller> dev_controller_bindings_;
};

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_NODE_H_
