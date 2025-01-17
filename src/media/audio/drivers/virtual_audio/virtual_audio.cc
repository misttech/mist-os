// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual_audio/virtual_audio.h"

#include <lib/async/cpp/task.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <optional>

#include <ddktl/fidl.h>

#include "src/media/audio/drivers/virtual_audio/virtual_audio_codec.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_composite.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_dai.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_device_impl.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_stream.h"

namespace virtual_audio {

zx_status_t VirtualAudio::Bind(void* ctx, zx_device_t* parent) {
  auto device = std::make_unique<VirtualAudio>(parent);
  if (zx::result result = device->Init(); result.is_error()) {
    zxlogf(ERROR, "Failed to initialize device: %s", result.status_string());
    return result.status_value();
  }

  // On successful Add, Devmgr takes ownership (relinquished on DdkRelease), so transfer our
  // ownership to a local var, and let it go out of scope.
  [[maybe_unused]] auto _ = device.release();

  return ZX_OK;
}

zx::result<> VirtualAudio::Init() {
  zx_status_t status =
      DdkAdd(ddk::DeviceAddArgs("virtual_audio").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

void VirtualAudio::DdkUnbind(ddk::UnbindTxn txn) {
  if (devices_.empty()) {
    zxlogf(INFO, "%s with no devices; unbinding self", __func__);
    txn.Reply();
    return;
  }

  // Close any remaining device bindings, freeing those drivers.
  auto remaining = std::make_shared<size_t>(devices_.size());

  for (auto& d : devices_) {
    zxlogf(INFO, "%s with %lu devices; shutting one down", __func__, *remaining);
    d->ShutdownAsync([remaining, txn = std::move(txn)]() mutable {
      ZX_ASSERT(*remaining > 0);
      // After all devices are gone we can remove the control device itself.
      if (--(*remaining) == 0) {
        zxlogf(INFO, "DdkUnbind(lambda): after shutting down devices; unbinding self");
        txn.Reply();
      }
    });
  }
  devices_.clear();
}

void VirtualAudio::DdkRelease() {
  // By now, all our lists should be empty and we release.
  ZX_ASSERT(devices_.empty());
}

void VirtualAudio::GetDefaultConfiguration(GetDefaultConfigurationRequestView request,
                                           GetDefaultConfigurationCompleter::Sync& completer) {
  fidl::Arena arena;
  switch (request->type) {
    case fuchsia_virtualaudio::wire::DeviceType::kComposite:
      completer.ReplySuccess(fidl::ToWire(arena, VirtualAudioComposite::GetDefaultConfig()));
      break;
    case fuchsia_virtualaudio::wire::DeviceType::kDai:
      completer.ReplySuccess(
          fidl::ToWire(arena, VirtualAudioDai::GetDefaultConfig(request->direction.is_input())));
      break;
    case fuchsia_virtualaudio::wire::DeviceType::kStreamConfig:
      completer.ReplySuccess(
          fidl::ToWire(arena, VirtualAudioStream::GetDefaultConfig(request->direction.is_input())));
      break;
    case fuchsia_virtualaudio::wire::DeviceType::kCodec:
      completer.ReplySuccess(fidl::ToWire(
          arena, VirtualAudioCodec::GetDefaultConfig(
                     (request->direction.has_is_input()
                          ? static_cast<std::optional<bool>>(request->direction.is_input())
                          : std::nullopt))));
      break;
    default:
      ZX_ASSERT_MSG(0, "Unknown device type");
  }
}

void VirtualAudio::AddDevice(AddDeviceRequestView request, AddDeviceCompleter::Sync& completer) {
  auto config = fidl::ToNatural(request->config);
  ZX_ASSERT(config.device_specific().has_value());
  auto result = VirtualAudioDeviceImpl::Create(std::move(config), std::move(request->server),
                                               parent_, dispatcher_);
  if (!result.is_ok()) {
    zxlogf(ERROR, "Device creation failed with status %d",
           fidl::ToUnderlying(result.error_value()));
    completer.ReplyError(result.error_value());
    return;
  }
  devices_.insert(result.value());
  completer.ReplySuccess();
}

void VirtualAudio::GetNumDevices(GetNumDevicesCompleter::Sync& completer) {
  uint32_t num_inputs = 0;
  uint32_t num_outputs = 0;
  uint32_t num_unspecified_direction = 0;
  for (auto& d : devices_) {
    if (!d->is_bound()) {
      devices_.erase(d);
      continue;
    }
    if (d->is_input().has_value()) {
      if (d->is_input().value()) {
        num_inputs++;
      } else {
        num_outputs++;
      }
    } else {
      num_unspecified_direction++;
    }
  }
  completer.Reply(num_inputs, num_outputs, num_unspecified_direction);
}

void VirtualAudio::RemoveAll(RemoveAllCompleter::Sync& completer) {
  if (devices_.empty()) {
    completer.Reply();
    return;
  }

  // This callback waits until all devices have shut down. We wrap the async completer in a
  // shared_ptr so the callback can be copied into each ShutdownAsync call.
  struct ShutdownState {
    explicit ShutdownState(RemoveAllCompleter::Sync& sync) : completer(sync.ToAsync()) {}
    RemoveAllCompleter::Async completer;
    size_t remaining;
  };
  auto state = std::make_shared<ShutdownState>(completer);
  state->remaining = devices_.size();

  for (auto& d : devices_) {
    d->ShutdownAsync([state]() {
      ZX_ASSERT(state->remaining > 0);
      // After all devices are gone, notify the completer.
      if ((--state->remaining) == 0) {
        state->completer.Reply();
      }
    });
  }
  devices_.clear();
}

}  // namespace virtual_audio

static constexpr zx_driver_ops_t virtual_audio_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = &virtual_audio::VirtualAudio::Bind;
  return ops;
}();

ZIRCON_DRIVER(virtual_audio, virtual_audio_driver_ops, "fuchsia", "0.1");
