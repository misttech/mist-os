// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdio/directory.h>

namespace compat {

namespace {

void CollectMetadataFrom(fidl::VectorView<fuchsia_driver_compat::wire::Metadata> metadata,
                         const ForwardMetadata& forward_metadata, DeviceServer* server) {
  for (auto& metadata : metadata) {
    auto should_forward = forward_metadata.should_forward(metadata.type);
    if (should_forward) {
      size_t size;
      zx_status_t status =
          metadata.data.get_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get metadata vmo size: %s", zx_status_get_string(status));
        continue;
      }

      Metadata data(size);
      status = metadata.data.read(data.data(), 0, data.size());
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read metadata vmo: %s", zx_status_get_string(status));
        continue;
      }

      server->AddMetadata(metadata.type, data.data(), data.size());
    }
  }
}

}  // namespace

namespace fcd = fuchsia_component_decl;

ForwardMetadata ForwardMetadata::All() { return ForwardMetadata(std::nullopt); }

ForwardMetadata ForwardMetadata::None() {
  return ForwardMetadata(std::make_optional(std::unordered_set<MetadataKey>{}));
}

ForwardMetadata ForwardMetadata::Some(std::unordered_set<MetadataKey> filter) {
  ZX_ASSERT_MSG(!filter.empty(), "ForwardMetadata::Some 'filter' cannot be empty.");
  return ForwardMetadata(std::make_optional(filter));
}

bool ForwardMetadata::empty() const { return filter_.has_value() && filter_->empty(); }

bool ForwardMetadata::should_forward(MetadataKey key) const {
  if (!filter_.has_value()) {
    return true;
  }

  return filter_->find(key) != filter_->end();
}

std::optional<zx::result<>> DeviceServer::InitResult() const {
  const Initialized* initialized = std::get_if<Initialized>(&state_);
  return (initialized != nullptr) ? std::make_optional(initialized->result) : std::nullopt;
}

void DeviceServer::Init(std::string name, std::string topological_path,
                        std::optional<ServiceOffersV1> service_offers,
                        std::optional<BanjoConfig> banjo_config) {
  state_.emplace<Initialized>();
  name_ = std::move(name);
  topological_path_ = std::move(topological_path);
  service_offers_ = std::move(service_offers);
  banjo_config_ = std::move(banjo_config);
}

void DeviceServer::OnInitialized(fit::callback<void(zx::result<>)> complete_callback) {
  Initialized* initialized = std::get_if<Initialized>(&state_);
  if (initialized != nullptr) {
    // We have already finished initialization (CompleteInitialization has already been called).
    // Post the callback to run in the dispatcher with the result.
    initialized->task.set_handler([this, cb = std::move(complete_callback)]() mutable {
      Initialized* initialized = std::get_if<Initialized>(&state_);
      ZX_ASSERT(initialized != nullptr);
      cb(initialized->result);
      initialized->task.set_handler(nullptr);
    });
    initialized->task.Post(initialized->dispatcher);
  } else {
    // The initialization has not finished yet. Store this callback so that CompleteInitialization
    // can run it when it is called.
    AsyncInit* init_state = std::get_if<AsyncInit>(&state_);
    ZX_ASSERT(init_state != nullptr);
    init_state->callback.emplace(std::move(complete_callback));
  }
}

zx_status_t DeviceServer::AddMetadata(uint32_t type, const void* data, size_t size) {
  // Constant taken from fuchsia.device.manager/METADATA_BYTES_MAX. We cannot depend on non-SDK
  // FIDL library here so we redefined the constant instead.
  constexpr size_t kMaxMetadataSize = 8192;
  if (size > kMaxMetadataSize) {
    return ZX_ERR_INVALID_ARGS;
  }
  Metadata metadata(size);
  auto begin = static_cast<const uint8_t*>(data);
  std::copy(begin, begin + size, metadata.begin());
  auto [_, inserted] = metadata_.emplace(type, std::move(metadata));
  if (!inserted) {
    // TODO(https://fxbug.dev/42063857): Return ZX_ERR_ALREADY_EXISTS instead once we do so in DFv1.
    return ZX_OK;
  }
  return ZX_OK;
}

zx_status_t DeviceServer::GetMetadata(uint32_t type, void* buf, size_t buflen, size_t* actual) {
  ZX_ASSERT(std::holds_alternative<Initialized>(state_));

  auto it = metadata_.find(type);
  if (it == metadata_.end()) {
    return ZX_ERR_NOT_FOUND;
  }
  auto& [_, metadata] = *it;

  *actual = metadata.size();
  if (buflen < metadata.size()) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  auto size = std::min(buflen, metadata.size());
  auto begin = metadata.begin();
  std::copy(begin, begin + size, static_cast<uint8_t*>(buf));

  return ZX_OK;
}

zx_status_t DeviceServer::GetMetadataSize(uint32_t type, size_t* out_size) {
  ZX_ASSERT(std::holds_alternative<Initialized>(state_));

  auto it = metadata_.find(type);
  if (it == metadata_.end()) {
    return ZX_ERR_NOT_FOUND;
  }
  auto& [_, metadata] = *it;
  *out_size = metadata.size();
  return ZX_OK;
}

zx_status_t DeviceServer::GetProtocol(BanjoProtoId proto_id, GenericProtocol* out) const {
  ZX_ASSERT(std::holds_alternative<Initialized>(state_));

  if (!banjo_config_.has_value()) {
    return ZX_ERR_NOT_FOUND;
  }

  // If there is a specific entry for the proto_id, use it.
  auto specific_entry = banjo_config_->callbacks.find(proto_id);
  if (specific_entry != banjo_config_->callbacks.end()) {
    auto& get_banjo_protocol = specific_entry->second;
    if (out) {
      *out = get_banjo_protocol();
    }

    return ZX_OK;
  }

  // Otherwise use the generic one if one was provided.
  if (!banjo_config_->generic_callback) {
    return ZX_ERR_NOT_FOUND;
  }

  zx::result generic_result = banjo_config_->generic_callback(proto_id);
  if (generic_result.is_error()) {
    return ZX_ERR_NOT_FOUND;
  }

  if (out) {
    *out = generic_result.value();
  }

  return ZX_OK;
}

zx_status_t DeviceServer::Serve(async_dispatcher_t* dispatcher,
                                component::OutgoingDirectory* outgoing) {
  auto device = [this, dispatcher](
                    fidl::ServerEnd<fuchsia_driver_compat::Device> server_end) mutable -> void {
    bindings_.AddBinding(dispatcher, std::move(server_end), this, fidl::kIgnoreBindingClosure);
  };

  fuchsia_driver_compat::Service::InstanceHandler handler({.device = std::move(device)});
  zx::result<> status =
      outgoing->AddService<fuchsia_driver_compat::Service>(std::move(handler), name());
  if (status.is_error()) {
    return status.error_value();
  }
  stop_serving_ = [this, outgoing]() {
    (void)outgoing->RemoveService<fuchsia_driver_compat::Service>(name_);
  };

  if (service_offers_) {
    return service_offers_->Serve(dispatcher, outgoing);
  }
  return ZX_OK;
}

zx_status_t DeviceServer::Serve(async_dispatcher_t* dispatcher, fdf::OutgoingDirectory* outgoing) {
  auto device = [this, dispatcher](
                    fidl::ServerEnd<fuchsia_driver_compat::Device> server_end) mutable -> void {
    bindings_.AddBinding(dispatcher, std::move(server_end), this, fidl::kIgnoreBindingClosure);
  };
  fuchsia_driver_compat::Service::InstanceHandler handler({.device = device});
  zx::result<> status =
      outgoing->AddService<fuchsia_driver_compat::Service>(std::move(handler), name());
  if (status.is_error()) {
    return status.error_value();
  }
  stop_serving_ = [this, outgoing]() {
    (void)outgoing->RemoveService<fuchsia_driver_compat::Service>(name_);
  };

  if (service_offers_) {
    return service_offers_->Serve(dispatcher, outgoing);
  }
  return ZX_OK;
}

std::vector<fcd::wire::Offer> DeviceServer::CreateOffers(fidl::ArenaBase& arena) {
  std::vector<fcd::wire::Offer> offers;
  // Create the main fuchsia.driver.compat.Service offer.
  offers.push_back(fdf::MakeOffer<fuchsia_driver_compat::Service>(arena, name()));

  if (service_offers_) {
    auto service_offers = service_offers_->CreateOffers(arena);
    offers.reserve(offers.size() + service_offers.size());
    offers.insert(offers.end(), service_offers.begin(), service_offers.end());
  }
  return offers;
}

std::vector<fcd::Offer> DeviceServer::CreateOffers() {
  std::vector<fcd::Offer> offers;
  // Create the main fuchsia.driver.compat.Service offer.
  offers.push_back(fdf::MakeOffer<fuchsia_driver_compat::Service>(name()));

  if (service_offers_) {
    auto service_offers = service_offers_->CreateOffers();
    offers.reserve(offers.size() + service_offers.size());
    offers.insert(offers.end(), service_offers.begin(), service_offers.end());
  }
  return offers;
}

void DeviceServer::GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) {
  ZX_ASSERT(std::holds_alternative<Initialized>(state_));

  ZX_ASSERT(topological_path_.has_value());
  completer.Reply(fidl::StringView::FromExternal(topological_path_.value()));
}

void DeviceServer::GetMetadata(GetMetadataCompleter::Sync& completer) {
  ZX_ASSERT(std::holds_alternative<Initialized>(state_));

  std::vector<fuchsia_driver_compat::wire::Metadata> metadata;
  metadata.reserve(metadata_.size());
  for (auto& [type, data] : metadata_) {
    fuchsia_driver_compat::wire::Metadata new_metadata;
    new_metadata.type = type;
    zx::vmo vmo;

    zx_status_t status = zx::vmo::create(data.size(), 0, &new_metadata.data);
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    status = new_metadata.data.write(data.data(), 0, data.size());
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    size_t size = data.size();
    status = new_metadata.data.set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }

    metadata.push_back(std::move(new_metadata));
  }
  completer.ReplySuccess(fidl::VectorView<fuchsia_driver_compat::wire::Metadata>::FromExternal(
      metadata.data(), metadata.size()));
}

void DeviceServer::GetBanjoProtocol(GetBanjoProtocolRequestView request,
                                    GetBanjoProtocolCompleter::Sync& completer) {
  ZX_ASSERT(std::holds_alternative<Initialized>(state_));

  // First check that we are in the same driver host.
  static uint64_t process_koid = []() {
    zx_info_handle_basic_t basic;
    ZX_ASSERT(zx::process::self()->get_info(ZX_INFO_HANDLE_BASIC, &basic, sizeof(basic), nullptr,
                                            nullptr) == ZX_OK);
    return basic.koid;
  }();

  if (process_koid != request->process_koid) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  GenericProtocol result;
  zx_status_t status = GetProtocol(request->proto_id, &result);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  completer.ReplySuccess(reinterpret_cast<uint64_t>(result.ops),
                         reinterpret_cast<uint64_t>(result.ctx));
}

void DeviceServer::SyncInit(const std::shared_ptr<fdf::Namespace>& incoming,
                            const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                            const std::optional<std::string>& node_name,
                            const std::optional<std::string>& child_additional_path,
                            const ForwardMetadata& forward_metadata) {
  auto node_name_val = node_name.value_or("NA");
  auto additional_path = child_additional_path.value_or("");

  // First connect to all the parents.
  auto parent_devices = ConnectToParentDevices(incoming.get());
  if (parent_devices.is_error()) {
    FDF_LOG(WARNING, "Failed to get parent devices: %s. Assuming root.",
            parent_devices.status_string());

    // In case that there are no parents, assume we are the root and create
    // the topological path from scratch.
    topological_path_ = "/" + node_name_val + "/" + additional_path + name_;
    ServeAndSetInitializeResult(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing);
    return;
  }

  // Now we have out parent clients, store them.
  fidl::WireSyncClient<fuchsia_driver_compat::Device> default_parent_client;
  std::unordered_map<std::string, fidl::WireSyncClient<fuchsia_driver_compat::Device>>
      parent_clients;
  for (auto& parent : parent_devices.value()) {
    if (parent.name == "default") {
      default_parent_client.Bind(std::move(parent.client));
      continue;
    }

    // TODO(https://fxbug.dev/42051759): When services stop adding extra instances
    // separated by ',' then remove this check.
    if (parent.name.find(',') != std::string::npos) {
      continue;
    }

    parent_clients[parent.name] =
        fidl::WireSyncClient<fuchsia_driver_compat::Device>(std::move(parent.client));
  }

  // No default parent found.
  if (!default_parent_client.is_valid()) {
    FDF_LOG(WARNING, "Failed to find the default parent.");

    // In case that there is no default parent, assume we are the root and create
    // the topological path from scratch.
    topological_path_ = "/" + node_name_val + "/" + additional_path + name_;
    ServeAndSetInitializeResult(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing);
    return;
  }

  // Get the topological path from the default parent.
  fidl::WireResult topological_path_result = default_parent_client->GetTopologicalPath();
  if (!topological_path_result.ok()) {
    FDF_LOG(WARNING, "Failed to get topological path from the parent: %s. Assuming root.",
            topological_path_result.status_string());

    // In case that getting the topological path fails, assume we are the root and create
    // the topological path from scratch.
    topological_path_ = "/" + node_name_val + "/" + additional_path + name_;
    ServeAndSetInitializeResult(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing);
    return;
  }

  // If we are a composite then we have to add the name of our composite device
  // to our primary parent. The composite device's name is the node_name.
  if (!parent_clients.empty()) {
    topological_path_ = std::string(topological_path_result->path.get()) + "/" + node_name_val +
                        "/" + additional_path + name_;
  } else {
    topological_path_ =
        std::string(topological_path_result->path.get()) + "/" + additional_path + name_;
  }

  // We can just serve and return if no metadata is needed.
  if (forward_metadata.empty()) {
    ServeAndSetInitializeResult(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing);
    return;
  }

  // Forward metadata
  if (parent_clients.empty()) {
    fidl::WireResult metadata_result = default_parent_client->GetMetadata();
    if (!metadata_result.ok()) {
      FDF_LOG(WARNING, "Failed to get metadata from default parent. %s",
              metadata_result.status_string());
      ServeAndSetInitializeResult(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing);
      return;
    }

    if (metadata_result.value().is_error()) {
      FDF_LOG(WARNING, "Failed to get metadata from default parent. %s",
              zx_status_get_string(metadata_result.value().error_value()));
      ServeAndSetInitializeResult(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing);
      return;
    }

    CollectMetadataFrom(metadata_result->value()->metadata, forward_metadata, this);
  } else {
    for (auto& [parent_name, parent_client] : parent_clients) {
      fidl::WireResult metadata_result = parent_client->GetMetadata();
      if (!metadata_result.ok()) {
        FDF_LOG(WARNING, "Failed to get metadata from parent %s. %s", parent_name.c_str(),
                metadata_result.status_string());
        continue;
      }

      if (metadata_result.value().is_error()) {
        FDF_LOG(WARNING, "Failed to get metadata from parent %s. %s", parent_name.c_str(),
                zx_status_get_string(metadata_result.value().error_value()));
        continue;
      }

      CollectMetadataFrom(metadata_result->value()->metadata, forward_metadata, this);
    }
  }

  ServeAndSetInitializeResult(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing);
}

void DeviceServer::ServeAndSetInitializeResult(
    async_dispatcher_t* dispatcher, const std::shared_ptr<fdf::OutgoingDirectory>& outgoing) {
  zx_status_t serve_result =
      Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), outgoing.get());
  if (serve_result != ZX_OK) {
    FDF_LOG(ERROR, "Failed to serve: %s", zx_status_get_string(serve_result));
    state_.emplace<Initialized>(dispatcher, zx::error(serve_result));
    return;
  }

  state_.emplace<Initialized>(dispatcher, zx::ok());
}

void DeviceServer::BeginAsyncInit() {
  AsyncInit* init_state = std::get_if<AsyncInit>(&state_);
  ZX_ASSERT(init_state != nullptr);
  auto task = ConnectToParentDevices(init_state->dispatcher, init_state->incoming.get(),
                                     [this](zx::result<std::vector<ParentDevice>> parents) {
                                       OnParentDevices(std::move(parents));
                                     });
  async_tasks_.AddTask(std::move(task));
}

void DeviceServer::OnParentDevices(zx::result<std::vector<ParentDevice>> parent_devices) {
  AsyncInit* init_state = std::get_if<AsyncInit>(&state_);
  ZX_ASSERT(init_state != nullptr);
  if (parent_devices.is_error()) {
    FDF_LOG(WARNING, "Failed to get parent devices: %s. Assuming root.",
            parent_devices.status_string());

    // In case that there are no parents, assume we are the root and create
    // the topological path from scratch.
    topological_path_ = "/" + init_state->node_name + "/" +
                        init_state->child_additional_path.value_or("") +
                        init_state->child_node_name;

    CompleteInitialization(zx::ok());
    return;
  }

  for (auto& parent : parent_devices.value()) {
    if (parent.name == "default") {
      default_parent_client_.Bind(std::move(parent.client), init_state->dispatcher);
      continue;
    }

    // TODO(https://fxbug.dev/42051759): When services stop adding extra instances
    // separated by ',' then remove this check.
    if (parent.name.find(',') != std::string::npos) {
      continue;
    }

    parent_clients_[parent.name] = fidl::WireClient<fuchsia_driver_compat::Device>(
        std::move(parent.client), init_state->dispatcher);
  }

  if (!default_parent_client_.is_valid()) {
    FDF_LOG(ERROR, "Failed to find the default parent.");
    CompleteInitialization(zx::error(ZX_ERR_NOT_FOUND));
    return;
  }

  default_parent_client_->GetTopologicalPath().Then(
      [this](fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetTopologicalPath>& result) {
        OnTopologicalPathResult(result);
      });
}

void DeviceServer::OnTopologicalPathResult(
    fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetTopologicalPath>& result) {
  AsyncInit* init_state = std::get_if<AsyncInit>(&state_);
  ZX_ASSERT(init_state != nullptr);
  if (!result.ok()) {
    FDF_LOG(WARNING, "Failed to get topological path from the parent: %s. Assuming root.",
            result.status_string());

    // In case that getting the topological path fails, assume we are the root and create
    // the topological path from scratch.
    topological_path_ = "/" + init_state->node_name + "/" +
                        init_state->child_additional_path.value_or("") +
                        init_state->child_node_name;
    CompleteInitialization(zx::ok());
    return;
  }

  // If we are a composite then we have to add the name of our composite device
  // to our primary parent. The composite device's name is the node_name.
  if (!parent_clients_.empty()) {
    topological_path_ = std::string(result->path.get()) + "/" + init_state->node_name + "/" +
                        init_state->child_additional_path.value_or("") +
                        init_state->child_node_name;
  } else {
    topological_path_ = std::string(result->path.get()) + "/" +
                        init_state->child_additional_path.value_or("") +
                        init_state->child_node_name;
  }

  if (init_state->forward_metadata.empty()) {
    CompleteInitialization(zx::ok());
    return;
  }

  if (parent_clients_.empty()) {
    init_state->in_flight_metadata++;
    default_parent_client_->GetMetadata().Then(
        [this](fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& result) {
          OnMetadataResult(result);
        });
  } else {
    for (auto& [parent_name, parent_client] : parent_clients_) {
      init_state->in_flight_metadata++;
      parent_client->GetMetadata().Then(
          [this](fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& result) {
            OnMetadataResult(result);
          });
    }
  }
}

void DeviceServer::OnMetadataResult(
    fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& result) {
  AsyncInit* init_state = std::get_if<AsyncInit>(&state_);
  ZX_ASSERT(init_state != nullptr);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to get metadata: %s", result.status_string());
    CompleteInitialization(zx::error(result.status()));
    return;
  }

  if (result.value().is_error()) {
    FDF_LOG(ERROR, "Failed to get metadata: %s",
            zx_status_get_string(result.value().error_value()));
    CompleteInitialization(zx::error(result.value().error_value()));
    return;
  }

  CollectMetadataFrom(result.value().value()->metadata, init_state->forward_metadata, this);
  init_state->in_flight_metadata--;
  if (init_state->in_flight_metadata == 0) {
    CompleteInitialization(zx::ok());
  }
}

void DeviceServer::CompleteInitialization(zx::result<> result) {
  AsyncInit* init_state = std::get_if<AsyncInit>(&state_);
  ZX_ASSERT(init_state != nullptr);

  if (result.is_error()) {
    FDF_LOG(ERROR, "Initialize failed: %s", result.status_string());

    auto callback = std::move(init_state->callback);
    state_.emplace<Initialized>(init_state->dispatcher, result);
    if (callback.has_value()) {
      callback.value()(result);
    }

    return;
  }

  zx_status_t serve_result = Serve(init_state->dispatcher, init_state->outgoing.get());
  if (serve_result != ZX_OK) {
    FDF_LOG(ERROR, "Failed to serve: %s", zx_status_get_string(serve_result));

    auto callback = std::move(init_state->callback);
    state_.emplace<Initialized>(init_state->dispatcher, zx::error(serve_result));
    if (callback.has_value()) {
      callback.value()(result);
    }

    return;
  }

  auto callback = std::move(init_state->callback);
  state_.emplace<Initialized>(init_state->dispatcher, zx::ok());
  if (callback.has_value()) {
    callback.value()(result);
  }
}

}  // namespace compat
