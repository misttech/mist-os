// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "radar-reader-proxy.h"

namespace radar {

RadarReaderProxy::~RadarReaderProxy() {
  // Close out any outstanding requests before destruction to avoid triggerig an assert.
  for (auto& [server, completer] : connect_requests_) {
    completer.Close(ZX_ERR_PEER_CLOSED);
  }
}

void RadarReaderProxy::Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) {
  if (radar_client_) {
    // Our driver client is bound, so we must have already set these fields.
    ZX_DEBUG_ASSERT(burst_properties_);

    instances_.emplace_back(std::make_unique<ReaderInstance>(this, std::move(request.server())));
    completer.Reply(fit::ok());
  } else {
    connect_requests_.emplace_back(std::move(request.server()), completer.ToAsync());
  }
}

void RadarReaderProxy::DeviceAdded(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                                   const std::string& filename) {
  if (!radar_client_) {
    connector_->ConnectToRadarDevice(
        dir, filename, [&](auto client_end) { return ValidateDevice(std::move(client_end)); });
  }
}

void RadarReaderProxy::on_fidl_error(const fidl::UnbindInfo info) {
  FX_PLOGS(ERROR, info.status()) << "Connection to radar device closed, attempting to reconnect";
  HandleFatalError(info.status());
}

// TODO(fxbug.dev/99924): Remove this after all servers have switched to OnBurst2.
void RadarReaderProxy::OnBurst(
    fidl::Event<fuchsia_hardware_radar::RadarBurstReader::OnBurst>& event) {
  ZX_DEBUG_ASSERT(burst_properties_);

  const auto& response = event.result().response();
  if (!response || response->burst().vmo_id() >= vmo_pool_.size()) {
    for (auto& instance : instances_) {
      instance->SendError(
          event.result().err().value_or(fuchsia_hardware_radar::StatusCode::kVmoNotFound));
    }
    return;
  }

  const uint32_t vmo_id = response->burst().vmo_id();
  const cpp20::span<const uint8_t> burst_data{vmo_pool_[vmo_id].start(), burst_properties_->size()};
  for (auto& instance : instances_) {
    instance->SendBurst(burst_data, zx::time(response->burst().timestamp()));
  }

  if (radar_client_) {
    if (auto result = radar_client_->UnlockVmo(vmo_id); result.is_error()) {
      HandleFatalError(result.error_value().status());
    }
  }
}

void RadarReaderProxy::OnBurst2(
    fidl::Event<fuchsia_hardware_radar::RadarBurstReader::OnBurst2>& event) {
  ZX_DEBUG_ASSERT(burst_properties_);

  if (event.IsUnknown()) {
    HandleFatalError(ZX_ERR_BAD_STATE);
    return;
  }

  if (event.error() || event.burst()->vmo_id() >= vmo_pool_.size()) {
    const auto status = event.error().value_or(fuchsia_hardware_radar::StatusCode::kVmoNotFound);
    for (auto& instance : instances_) {
      instance->SendError(status);
    }
    return;
  }

  const uint32_t vmo_id = event.burst()->vmo_id();
  const cpp20::span<const uint8_t> burst_data{vmo_pool_[vmo_id].start(), burst_properties_->size()};
  for (auto& instance : instances_) {
    instance->SendBurst(burst_data, zx::time(event.burst()->timestamp()));
  }

  if (radar_client_) {
    if (auto result = radar_client_->UnlockVmo(vmo_id); result.is_error()) {
      HandleFatalError(result.error_value().status());
    }
  }
}

bool RadarReaderProxy::ValidateDevice(
    fidl::ClientEnd<fuchsia_hardware_radar::RadarBurstReaderProvider> client_end) {
  fidl::SyncClient provider(std::move(client_end));

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  if (endpoints.is_error() || provider->Connect(std::move(endpoints->server)).is_error()) {
    return false;
  }

  fidl::SyncClient reader(std::move(endpoints->client));

  // If this is our first connection, save the burst size. If this is not our first connection, make
  // sure that the burst size reported by this device matches what we have saved.
  const auto burst_properties = reader->GetBurstProperties();
  if (burst_properties.is_error() ||
      (burst_properties_ && burst_properties_->size() != burst_properties->size())) {
    return false;
  }
  if (!burst_properties_) {
    burst_properties_ = *burst_properties;
  }

  if (!vmo_pool_.empty()) {
    // Register all of the VMOs that are currently in the pool.
    std::vector<uint32_t> vmo_ids(vmo_pool_.size());
    std::vector<zx::vmo> vmos(vmo_pool_.size());

    for (uint32_t i = 0; i < vmo_pool_.size(); i++) {
      vmo_ids[i] = i;
      zx_status_t status = vmo_pool_[i].vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmos[i]);
      if (status != ZX_OK) {
        FX_PLOGS(ERROR, status) << "Failed to duplicate VMO";
        return false;
      }
    }

    const auto result = reader->RegisterVmos({{std::move(vmo_ids), std::move(vmos)}});
    if (result.is_error() && result.error_value().is_framework_error()) {
      FX_PLOGS(ERROR, result.error_value().framework_error().status())
          << "Failed to send register VMOs request";
      return false;
    }
    if (result.is_error() && result.error_value().is_domain_error()) {
      FX_LOGS(ERROR) << "Failed to register VMOs";
      return false;
    }
  }

  radar_client_ = fidl::Client(reader.TakeClientEnd(), dispatcher_, this);

  // Now that we are connected to a device, complete any connect requests that are outstanding.
  for (auto& [server, completer] : connect_requests_) {
    instances_.emplace_back(std::make_unique<ReaderInstance>(this, std::move(server)));
    completer.Reply(fit::ok());
  }
  connect_requests_.clear();

  return true;
}

void RadarReaderProxy::HandleFatalError(const zx_status_t status) {
  // Tear down the client so that we stop receiving bursts. We may be handling a FIDL error here, in
  // which case the client has already been unbound.
  radar_client_ = {};

  // Close all existing client connections and let them reconnect if needed.
  for (auto& instance : instances_) {
    instance->Close(status);
  }

  // Check for available devices now, just in case one was added before the connection closed. If
  // not, the DeviceWatcher will signal to connect when a new device becomes available.
  connector_->ConnectToFirstRadarDevice(
      [&](auto client_end) { return ValidateDevice(std::move(client_end)); });
}

void RadarReaderProxy::UpdateVmoCount(const size_t count) {
  ZX_DEBUG_ASSERT(burst_properties_);

  if (count <= vmo_pool_.size()) {
    return;
  }

  const size_t vmos_to_register = count - vmo_pool_.size();

  std::vector<zx::vmo> vmos(vmos_to_register);
  std::vector<uint32_t> vmo_ids(vmos_to_register);

  for (size_t i = 0; i < vmos.size(); i++) {
    vmo_ids[i] = static_cast<uint32_t>(vmo_pool_.size());

    MappedVmo vmo;

    // The radar driver writes to these VMOs, so we only need read access.
    zx_status_t status =
        vmo.mapped_vmo.CreateAndMap(burst_properties_->size(), ZX_VM_PERM_READ, nullptr, &vmo.vmo);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to create and map VMO";
      HandleFatalError(status);
      return;
    }

    if ((status = vmo.vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmos[i])) != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to duplicate VMO";
      HandleFatalError(status);
      return;
    }

    vmo_pool_.push_back(std::move(vmo));
  }

  radar_client_->RegisterVmos({std::move(vmo_ids), std::move(vmos)}).Then([&](const auto& result) {
    if (result.is_error()) {
      zx_status_t status = ZX_ERR_BAD_STATE;
      if (result.error_value().is_framework_error()) {
        status = result.error_value().is_framework_error();
        FX_PLOGS(ERROR, status) << "Failed to send register VMOs request";
      } else {
        FX_LOGS(ERROR) << "Failed to register VMOs";
      }
      HandleFatalError(status);
    }
  });
}

void RadarReaderProxy::StartBursts() {
  if (auto result = radar_client_->StartBursts(); result.is_error()) {
    FX_PLOGS(ERROR, result.error_value().status()) << "Failed to send start bursts request";
    HandleFatalError(result.error_value().status());
  }
}

void RadarReaderProxy::StopBursts() {
  for (const auto& instance : instances_) {
    // Don't stop bursts if any client still wants to receive them.
    if (instance->bursts_started()) {
      return;
    }
  }

  radar_client_->StopBursts().Then([&](const auto& result) {
    if (result.is_error()) {
      FX_PLOGS(ERROR, result.error_value().status()) << "Failed to send stop bursts request";
      HandleFatalError(result.error_value().status());
    }
  });
}

void RadarReaderProxy::InstanceUnbound(ReaderInstance* const instance) {
  // The instance was unbound, remove it from our list and let it be deleted.
  for (auto it = instances_.begin(); it != instances_.end(); it++) {
    if (it->get() == instance) {
      instances_.erase(it);

      // Stop bursts if this was the last client that wanted to receive them. We may be handling a
      // fatal error in which case the client has already been unbound.
      if (radar_client_) {
        StopBursts();
      }
      return;
    }
  }
}

RadarReaderProxy::ReaderInstance::ReaderInstance(
    RadarReaderProxy* const parent,
    fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstReader> server_end)
    : parent_(parent),
      server_(fidl::BindServer(parent->dispatcher_, std::move(server_end), this,
                               [](ReaderInstance* instance, auto, auto) {
                                 instance->parent_->InstanceUnbound(instance);
                               })),
      manager_(parent->burst_properties_ ? parent->burst_properties_->size() : 0) {}

// TODO(fxbug.dev/100020): Remove this after all clients have switched to GetBurstProperties.
void RadarReaderProxy::ReaderInstance::GetBurstSize(GetBurstSizeCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(parent_->burst_properties_);
  completer.Reply(parent_->burst_properties_->size());
}

void RadarReaderProxy::ReaderInstance::GetBurstProperties(
    GetBurstPropertiesCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(parent_->burst_properties_);
  completer.Reply(*parent_->burst_properties_);
}

void RadarReaderProxy::ReaderInstance::RegisterVmos(RegisterVmosRequest& request,
                                                    RegisterVmosCompleter::Sync& completer) {
  const fit::result result = manager_.RegisterVmos(request.vmo_ids(), std::move(request.vmos()));
  completer.Reply(result);
  if (result.is_ok()) {
    parent_->UpdateVmoCount(vmo_count_ += request.vmo_ids().size());
  }
}

void RadarReaderProxy::ReaderInstance::UnregisterVmos(UnregisterVmosRequest& request,
                                                      UnregisterVmosCompleter::Sync& completer) {
  completer.Reply(manager_.UnregisterVmos(request.vmo_ids()));
}

void RadarReaderProxy::ReaderInstance::StartBursts(StartBurstsCompleter::Sync& completer) {
  bursts_started_ = true;
  sent_error_ = false;
  parent_->StartBursts();
}

void RadarReaderProxy::ReaderInstance::StopBursts(StopBurstsCompleter::Sync& completer) {
  bursts_started_ = false;
  // We know that no more bursts will be sent on the channel after setting this flag, so it is safe
  // to immediately reply.
  completer.Reply();
  parent_->StopBursts();
}

void RadarReaderProxy::ReaderInstance::UnlockVmo(UnlockVmoRequest& request,
                                                 UnlockVmoCompleter::Sync& completer) {
  manager_.UnlockVmo(request.vmo_id());
}

void RadarReaderProxy::ReaderInstance::SendBurst(const cpp20::span<const uint8_t> burst,
                                                 const zx::time timestamp) {
  if (!bursts_started_) {
    return;
  }

  fit::result vmo_id = manager_.WriteUnlockedVmoAndGetId(burst);
  if (vmo_id.is_error()) {
    SendError(vmo_id.error_value());
    return;
  }

  sent_error_ = false;

  const auto result = fuchsia_hardware_radar::RadarBurstReaderOnBurstResult::WithResponse(
      {{*vmo_id, timestamp.get()}});
  if (auto event_result = fidl::SendEvent(server_)->OnBurst(result); event_result.is_error()) {
    FX_PLOGS(ERROR, event_result.error_value().status()) << "Failed to send burst";
  }
}

void RadarReaderProxy::ReaderInstance::SendError(const fuchsia_hardware_radar::StatusCode error) {
  if (!bursts_started_ || sent_error_) {
    return;
  }

  sent_error_ = true;

  const auto result = fuchsia_hardware_radar::RadarBurstReaderOnBurstResult::WithErr(error);
  if (auto event_result = fidl::SendEvent(server_)->OnBurst(result); event_result.is_error()) {
    FX_PLOGS(ERROR, event_result.error_value().status()) << "Failed to send burst error";
  }
}

}  // namespace radar
