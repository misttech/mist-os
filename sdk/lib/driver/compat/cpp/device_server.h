// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPAT_CPP_DEVICE_SERVER_H_
#define LIB_DRIVER_COMPAT_CPP_DEVICE_SERVER_H_

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
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
    std::unordered_set<MetadataKey> forward_metadata;
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

  using GetBanjoProtoCb = fit::function<zx::result<GenericProtocol>(BanjoProtoId)>;

  struct BanjoConfig {
    BanjoProtoId default_proto_id = 0;
    std::unordered_map<BanjoProtoId, GetBanjoProtoCb> callbacks = {};
  };

  // Initialize empty. Can use |Init| to fill in information later.
  DeviceServer() : state_(std::in_place_type<Initialized>) {}

  // Remove when dependencies are removed.
  DeviceServer(std::string name, uint32_t proto_id, std::string topological_path)
      : state_(std::in_place_type<Initialized>) {
    ZX_ASSERT(proto_id == 0);
    BanjoConfig config{proto_id};
    Init(std::move(name), std::move(topological_path), {}, std::move(config));
  }

  // Async initialization. Will internally query the parent for topological path and forwarded
  // metadata and serve on the outgoing directory when it is ready.
  DeviceServer(async_dispatcher_t* dispatcher, const std::shared_ptr<fdf::Namespace>& incoming,
               const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
               const std::optional<std::string>& node_name, std::string_view child_node_name,
               const std::unordered_set<MetadataKey>& forward_metadata = {},
               std::optional<BanjoConfig> banjo_config = std::nullopt)
      : state_(AsyncInit{dispatcher, incoming, outgoing, node_name.value_or("NA"),
                         std::string(child_node_name), forward_metadata}),
        name_(child_node_name),
        banjo_config_(std::move(banjo_config)) {
    BeginAsyncInit();
  }

  // Initialize with known topological path.
  void Init(std::string name, std::string topological_path,
            std::optional<ServiceOffersV1> service_offers = std::nullopt,
            std::optional<BanjoConfig> banjo_config = std::nullopt);

  void OnInitialized(fit::callback<void(zx::result<>)> complete_callback);

  // Functions to implement the DFv1 device API.
  zx_status_t AddMetadata(MetadataKey type, const void* data, size_t size);
  zx_status_t GetMetadata(MetadataKey type, void* buf, size_t buflen, size_t* actual);
  zx_status_t GetMetadataSize(MetadataKey type, size_t* out_size);

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

 private:
  // fuchsia.driver.compat.Compat
  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override;
  void GetMetadata(GetMetadataCompleter::Sync& completer) override;
  void GetBanjoProtocol(GetBanjoProtocolRequestView request,
                        GetBanjoProtocolCompleter::Sync& completer) override;

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
};

}  // namespace compat

#endif  // LIB_DRIVER_COMPAT_CPP_DEVICE_SERVER_H_
