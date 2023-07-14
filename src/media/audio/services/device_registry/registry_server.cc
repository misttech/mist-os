// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/registry_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/transaction.h>
#include <lib/fidl/cpp/wire/wire_messaging_declarations.h>
#include <lib/fit/internal/result.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include <memory>
#include <optional>
#include <vector>

#include "fidl/fuchsia.audio.device/cpp/common_types.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/observer_server.h"

namespace media_audio {

// static
std::shared_ptr<RegistryServer> RegistryServer::Create(
    std::shared_ptr<const FidlThread> thread,
    fidl::ServerEnd<fuchsia_audio_device::Registry> server_end,
    std::shared_ptr<AudioDeviceRegistry> parent) {
  ADR_LOG_CLASS(kLogRegistryServerMethods);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), parent);
}

RegistryServer::RegistryServer(std::shared_ptr<AudioDeviceRegistry> parent) : parent_(parent) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();
}

RegistryServer::~RegistryServer() {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

void RegistryServer::WatchDevicesAdded(WatchDevicesAddedCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogRegistryServerMethods);
  if (watch_devices_added_completer_) {
    ADR_WARN_OBJECT() << "previous `WatchDevicesAdded` request has not yet completed";
    completer.Reply(fit::error<fuchsia_audio_device::RegistryWatchDevicesAddedError>(
        fuchsia_audio_device::RegistryWatchDevicesAddedError::kWatchAlreadyPending));
    return;
  }

  watch_devices_added_completer_ = completer.ToAsync();
  ReplyWithAddedDevices();
}

void RegistryServer::DeviceWasAdded(std::shared_ptr<const Device> new_device) {
  ADR_LOG_OBJECT(kLogRegistryServerMethods);

  auto id = *new_device->info()->token_id();
  auto token_match = [id](fuchsia_audio_device::Info& info) { return info.token_id() == id; };
  if (std::find_if(devices_added_since_notify_.begin(), devices_added_since_notify_.end(),
                   token_match) != devices_added_since_notify_.end()) {
    FX_LOGS(ERROR) << "Device already added and not yet acknowledged, for this RegistryServer";
    return;
  }

  // Unlike remove-after-unack'ed-add (we delete both), don't coalesce add-after-unack'ed-remove.
  // Removed-then-added devices get a new token_id, so in practice this will never happen.

  devices_added_since_notify_.push_back(*new_device->info());
  ReplyWithAddedDevices();
}

// We just got either a completer, or a newly-added device. If now we have both, Reply.
void RegistryServer::ReplyWithAddedDevices() {
  if (!watch_devices_added_completer_) {
    ADR_LOG_OBJECT(kLogRegistryServerMethods) << "no pending completer; just adding to our list";
    return;
  }
  if (devices_added_since_notify_.empty()) {
    ADR_LOG_OBJECT(kLogRegistryServerMethods) << "devices_added_since_notify_ is empty";
    return;
  }

  auto completer = *std::move(watch_devices_added_completer_);
  watch_devices_added_completer_.reset();
  ADR_LOG_OBJECT(kLogRegistryServerResponses) << "responding to WatchDevicesAdded with "
                                              << devices_added_since_notify_.size() << " devices:";
  for (auto& info : devices_added_since_notify_) {
    ADR_LOG_OBJECT(kLogRegistryServerResponses) << "    token_id " << *info.token_id();
  }
  completer.Reply(fit::success(fuchsia_audio_device::RegistryWatchDevicesAddedResponse{{
      .devices = std::move(devices_added_since_notify_),
  }}));
}

// TODO(fxbug.dev/117166): consider WatchDevicesRemoved (plural: return vector) - more ergonomic?
void RegistryServer::WatchDeviceRemoved(WatchDeviceRemovedCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogRegistryServerMethods);
  if (watch_device_removed_completer_) {
    ADR_WARN_OBJECT() << "previous `WatchDeviceRemoved` request has not yet completed";
    completer.Reply(fit::error<fuchsia_audio_device::RegistryWatchDeviceRemovedError>(
        fuchsia_audio_device::RegistryWatchDeviceRemovedError::kWatchAlreadyPending));
    return;
  }

  watch_device_removed_completer_ = completer.ToAsync();
  ReplyWithNextRemovedDevice();
}

void RegistryServer::DeviceWasRemoved(uint64_t removed_id) {
  ADR_LOG_OBJECT(kLogRegistryServerMethods);
  auto already_in_queue = false;
  for (auto i = devices_removed_since_notify_.size(); i > 0; --i) {
    auto id = devices_removed_since_notify_.front();
    if (id == removed_id) {
      already_in_queue = true;  // rotate the entire queue even if we find it, to maintain order.
    }
    devices_removed_since_notify_.pop();
    devices_removed_since_notify_.push(id);
  }
  if (already_in_queue) {
    FX_LOGS(ERROR) << "Device (" << removed_id << ") already removed and not yet acknowledged";
    return;
  }
  auto match = std::find_if(
      devices_added_since_notify_.begin(), devices_added_since_notify_.end(),
      [removed_id](fuchsia_audio_device::Info& info) { return info.token_id() == removed_id; });
  if (match != devices_added_since_notify_.end()) {
    ADR_LOG_OBJECT(kLogRegistryServerResponses)
        << "Device (" << removed_id << ") added then removed before notified!";
    devices_added_since_notify_.erase(match);
    return;
  }

  devices_removed_since_notify_.push(removed_id);
  ReplyWithNextRemovedDevice();
}

// We just got either a completer, or a newly-removed device. If now we have both, Reply.
void RegistryServer::ReplyWithNextRemovedDevice() {
  if (devices_removed_since_notify_.empty()) {
    ADR_LOG_OBJECT(kLogRegistryServerMethods) << "devices_removed_since_notify_ is empty";
    return;
  }
  if (!watch_device_removed_completer_) {
    ADR_LOG_OBJECT(kLogRegistryServerMethods) << "no WatchDeviceRemoved completer";
    return;
  }
  auto next_removed_id = devices_removed_since_notify_.front();
  devices_removed_since_notify_.pop();
  ADR_LOG_OBJECT(kLogRegistryServerResponses) << "responding with token_id " << next_removed_id;
  auto completer = *std::move(watch_device_removed_completer_);
  watch_device_removed_completer_.reset();
  completer.Reply(fit::success(
      fuchsia_audio_device::RegistryWatchDeviceRemovedResponse{{.token_id = next_removed_id}}));
}

void RegistryServer::CreateObserver(CreateObserverRequest& request,
                                    CreateObserverCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogRegistryServerMethods);

  if (!request.token_id()) {
    ADR_WARN_OBJECT() << "required field 'id' is missing";
    completer.Reply(fit::error(fuchsia_audio_device::RegistryCreateObserverError::kInvalidTokenId));
    return;
  }
  if (!request.observer_server()) {
    ADR_WARN_OBJECT() << "required field 'observer_server' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::RegistryCreateObserverError::kInvalidObserver));
    return;
  }
  auto token_id = *request.token_id();
  auto [presence, matching_device] = parent_->FindDeviceByTokenId(token_id);
  switch (presence) {
    // We could break these out into separate error codes if needed.
    case AudioDeviceRegistry::DevicePresence::Unknown:
      ADR_WARN_OBJECT() << "no device found with 'id' " << token_id;
      completer.Reply(
          fit::error(fuchsia_audio_device::RegistryCreateObserverError::kDeviceNotFound));
      return;

    case AudioDeviceRegistry::DevicePresence::Error:
      ADR_WARN_OBJECT() << "device with 'id' " << token_id << " has an error";
      completer.Reply(fit::error(fuchsia_audio_device::RegistryCreateObserverError::kDeviceError));
      return;

    case AudioDeviceRegistry::DevicePresence::Active:
      break;
  }

  // TODO(fxbug.dev/117199): Decide when we proactively call GetHealthState, if at all.

  auto observer =
      parent_->CreateObserverServer(std::move(*request.observer_server()), matching_device);

  completer.Reply(fit::success(fuchsia_audio_device::RegistryCreateObserverResponse{}));
}

}  // namespace media_audio
