// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPAT_CPP_DEVICE_SERVER_H_
#define LIB_DRIVER_COMPAT_CPP_DEVICE_SERVER_H_

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/async-helpers/cpp/task_group.h>
#include <lib/driver/compat/cpp/connect.h>
#include <lib/driver/compat/cpp/service_offers.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/outgoing/cpp/handlers.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include <unordered_set>

namespace compat {

using Metadata = std::vector<uint8_t>;
using MetadataKey = uint32_t;
using MetadataMap = std::unordered_map<MetadataKey, const Metadata>;

using BanjoProtoId = uint32_t;

class ForwardMetadata {
 public:
  // Creates a ForwardMetadata object in which all of the available metadata from the parent(s)
  // are forwarded.
  static ForwardMetadata All();

  // Creates a ForwardMetadata object in which none of the available metadata from the parent(s)
  // are forwarded.
  static ForwardMetadata None();

  // Creates a ForwardMetadata object in which some of the available metadata from the parent(s)
  // are forwarded. The given filter set must not be empty.
  //
  // The given set is used as a filter when looking through all of the available metadata to the
  // driver. This means if a given metadata key is not found through the parent(s), it will be
  // ignored.
  static ForwardMetadata Some(std::unordered_set<MetadataKey> filter);

  // Deprecated constructor. Use All(), Some(), None() instead.
  // TODO(https://fxbug.dev/42086090): Remove once all usages are migrated
  explicit ForwardMetadata(std::unordered_set<MetadataKey> filter) : filter_(filter) {}

  // Returns true when there's nothing to forward.
  bool empty() const;

  // Returns true if the given key meets the requirements for forwarding.
  bool should_forward(MetadataKey key) const;

 private:
  explicit ForwardMetadata(std::optional<std::unordered_set<MetadataKey>> filter)
      : filter_(std::move(filter)) {}

  std::optional<std::unordered_set<MetadataKey>> filter_;
};

// The DeviceServer class vends the fuchsia_driver_compat::Device interface.
// It represents a single device.
class DeviceServer : public fidl::WireServer<fuchsia_driver_compat::Device> {
 public:
  struct GenericProtocol {
    void* ops;
    void* ctx;
  };

  struct AsyncInit {
    async_dispatcher_t* dispatcher;
    std::shared_ptr<fdf::Namespace> incoming;
    std::shared_ptr<fdf::OutgoingDirectory> outgoing;
    std::string node_name;
    std::string child_node_name;
    std::optional<std::string> child_additional_path;
    ForwardMetadata forward_metadata;
    uint32_t in_flight_metadata = 0;
    std::optional<fit::callback<void(zx::result<>)>> callback;
  };

  struct Initialized {
    Initialized()
        : dispatcher(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
          result(zx::ok()),
          task(nullptr) {}

    Initialized(async_dispatcher_t* dispatcher, zx::result<> result)
        : dispatcher(dispatcher), result(result), task(nullptr) {}

    async_dispatcher_t* dispatcher;
    zx::result<> result;
    async::TaskClosure task;
  };

  using SpecificGetBanjoProtoCb = fit::function<GenericProtocol()>;
  using GenericGetBanjoProtoCb = fit::function<zx::result<GenericProtocol>(BanjoProtoId)>;

  struct BanjoConfig {
    BanjoProtoId default_proto_id = 0;
    GenericGetBanjoProtoCb generic_callback = nullptr;
    std::unordered_map<BanjoProtoId, SpecificGetBanjoProtoCb> callbacks = {};
  };

  // Initialize empty. Can use |Init| to fill in information later.
  DeviceServer() : state_(std::in_place_type<Initialized>) {}

  // Deprecated constructor. Use empty constructor with |Init| call after to manually initialize,
  // or use the sync/async-initialization constructors.
  // TODO(https://fxbug.dev/42086090): Remove once all usages are migrated
  DeviceServer(std::string name, uint32_t proto_id, std::string topological_path)
      : state_(std::in_place_type<Initialized>) {
    ZX_ASSERT(proto_id == 0);
    BanjoConfig config{proto_id};
    Init(std::move(name), std::move(topological_path), {}, std::move(config));
  }

  // Async initialization. Will internally query the parent(s) for topological path and forwarded
  // metadata and serve on the outgoing directory when it is ready. Use the |OnInitialized| method
  // to register a callback to be called when the async initialization has been completed.
  //
  // Use the |OnInitialized| method to register a callback to be called when the async
  // initialization has been completed.
  //
  // |dispatcher|, |incoming|, |outgoing|, |node_name| can be grabbed through the
  // DriverBase methods of the same name.
  //
  // |child_node_name| is the name given to the |fdf::NodeAddArgs|'s name field.
  //
  // |child_additional_path| is used in the case that there are intermediary nodes that are
  // owned by this driver before the target child node. Each intermediate node should be separated
  // with a '/' and it should end with a trailing '/'. Eg: "node-a/node-b/"
  //
  // |forward_metadata| will contain information about the metadata to forward from the parent(s).
  //
  // |banjo_config| contains the banjo protocol information that this should serve.
  DeviceServer(async_dispatcher_t* dispatcher, const std::shared_ptr<fdf::Namespace>& incoming,
               const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
               const std::optional<std::string>& node_name, std::string_view child_node_name,
               const std::optional<std::string>& child_additional_path,
               const ForwardMetadata& forward_metadata = ForwardMetadata::None(),
               std::optional<BanjoConfig> banjo_config = std::nullopt)
      : state_(AsyncInit{dispatcher, incoming, outgoing, node_name.value_or("NA"),
                         std::string(child_node_name), child_additional_path, forward_metadata}),
        name_(child_node_name),
        banjo_config_(std::move(banjo_config)) {
    BeginAsyncInit();
  }

  // Sync initialization. Will immediately query the parent(s) for topological path and forwarded
  // metadata and serve on the outgoing directory synchronously before returning from the
  // constructor. Use the |InitResult| method to retrieve the initialization result.
  //
  // |incoming|, |outgoing|, |node_name| can be grabbed through the
  // DriverBase methods of the same name.
  //
  // |child_node_name| is the name given to the |fdf::NodeAddArgs|'s name field.
  //
  // |child_additional_path| is used in the case that there are intermediary nodes that are
  // owned by this driver before the target child node. Each intermediate node should be separated
  // with a '/' and it should end with a trailing '/'. Eg: "node-a/node-b/"
  //
  // |forward_metadata| will contain information about the metadata to forward from the parent(s).
  //
  // |banjo_config| contains the banjo protocol information that this should serve.
  DeviceServer(const std::shared_ptr<fdf::Namespace>& incoming,
               const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
               const std::optional<std::string>& node_name, std::string_view child_node_name,
               const std::optional<std::string>& child_additional_path,
               const ForwardMetadata& forward_metadata = ForwardMetadata::None(),
               std::optional<BanjoConfig> banjo_config = std::nullopt)
      : state_(std::in_place_type<Initialized>),
        name_(child_node_name),
        banjo_config_(std::move(banjo_config)) {
    SyncInit(incoming, outgoing, node_name, child_additional_path, forward_metadata);
  }

  // Gets the initialization result. This can be used in conjunction with the synchronous
  // initialization constructor to ensure that initialization succeeded.
  std::optional<zx::result<>> InitResult() const;

  // Initialize with known topological path. This is generally used in conjunction with the
  // empty constructor to avoid the self-initialization logic.
  void Init(std::string name, std::string topological_path,
            std::optional<ServiceOffersV1> service_offers = std::nullopt,
            std::optional<BanjoConfig> banjo_config = std::nullopt);

  // Set a callback to be called when the DeviceServer becomes initialized. This is generally used
  // in conjunction with the async initialization constructor, the one taking a dispatcher in,
  // so that the user can receive a callback when the asynchronous initialization is complete.
  void OnInitialized(fit::callback<void(zx::result<>)> complete_callback);

  // Functions to implement the DFv1 device API.
  zx_status_t AddMetadata(MetadataKey type, const void* data, size_t size);
  zx_status_t GetMetadata(MetadataKey type, void* buf, size_t buflen, size_t* actual);
  zx_status_t GetMetadataSize(MetadataKey type, size_t* out_size);
  zx_status_t GetProtocol(BanjoProtoId proto_id, GenericProtocol* out) const;

  // Serve this interface in an outgoing directory.
  zx_status_t Serve(async_dispatcher_t* dispatcher, component::OutgoingDirectory* outgoing);
  zx_status_t Serve(async_dispatcher_t* dispatcher, fdf::OutgoingDirectory* outgoing);

  // Create offers to offer this interface to another component.
  std::vector<fuchsia_component_decl::wire::Offer> CreateOffers(fidl::ArenaBase& arena);
  std::vector<fuchsia_component_decl::Offer> CreateOffers();

  std::string_view name() const { return name_; }
  std::string topological_path() const { return topological_path_.value_or(""); }
  BanjoProtoId proto_id() const {
    return banjo_config_.has_value() ? banjo_config_->default_proto_id : 0;
  }
  bool has_banjo_config() const { return banjo_config_.has_value(); }

 private:
  // fuchsia.driver.compat.Compat
  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override;
  void GetMetadata(GetMetadataCompleter::Sync& completer) override;
  void GetBanjoProtocol(GetBanjoProtocolRequestView request,
                        GetBanjoProtocolCompleter::Sync& completer) override;

  // Synchronous initialization.
  void SyncInit(const std::shared_ptr<fdf::Namespace>& incoming,
                const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                const std::optional<std::string>& node_name,
                const std::optional<std::string>& child_additional_path,
                const ForwardMetadata& forward_metadata);
  void ServeAndSetInitializeResult(async_dispatcher_t* dispatcher,
                                   const std::shared_ptr<fdf::OutgoingDirectory>& outgoing);

  // Helpers for async initialization.
  void BeginAsyncInit();
  void OnParentDevices(zx::result<std::vector<ParentDevice>> parent_devices);
  void OnTopologicalPathResult(
      fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetTopologicalPath>& result);
  void OnMetadataResult(
      fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& result);
  void CompleteInitialization(zx::result<> result);

  std::variant<AsyncInit, Initialized> state_;

  // Set in |OnParentDevices|.
  fidl::WireClient<fuchsia_driver_compat::Device> default_parent_client_ = {};
  std::unordered_map<std::string, fidl::WireClient<fuchsia_driver_compat::Device>> parent_clients_ =
      {};

  std::string name_;
  std::optional<std::string> topological_path_;
  MetadataMap metadata_;
  std::optional<ServiceOffersV1> service_offers_;
  std::optional<BanjoConfig> banjo_config_;

  fidl::ServerBindingGroup<fuchsia_driver_compat::Device> bindings_;

  // This callback is called when the class is destructed and it will stop serving the protocol.
  fit::deferred_callback stop_serving_;
  fdf::async_helpers::TaskGroup async_tasks_;
};

}  // namespace compat

#endif  // LIB_DRIVER_COMPAT_CPP_DEVICE_SERVER_H_
