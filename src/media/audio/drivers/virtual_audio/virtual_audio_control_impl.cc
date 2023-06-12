// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual_audio/virtual_audio_control_impl.h"

#include <lib/async/cpp/task.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/zx/result.h>

#include <ddktl/fidl.h>

#include "src/media/audio/drivers/virtual_audio/virtual_audio_composite.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_dai.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_device_impl.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_stream.h"

namespace virtual_audio {

// static
zx_status_t VirtualAudioControlImpl::DdkBind(void* ctx, zx_device_t* parent_bus) {
  std::unique_ptr<VirtualAudioControlImpl> control(new VirtualAudioControlImpl);

  // Define entry-point operations for this control device.
  static zx_protocol_device_t device_ops = {
      .version = DEVICE_OPS_VERSION,
      .unbind = &DdkUnbind,
      .release = &DdkRelease,
      .message = &DdkMessage,
  };

  // Define other metadata, incl. "control" as our entry-point context.
  device_add_args_t args = {};
  args.version = DEVICE_ADD_ARGS_VERSION;
  args.name = "virtual_audio";
  args.ctx = control.get();
  args.ops = &device_ops;
  args.flags = DEVICE_ADD_NON_BINDABLE;

  // Add the virtual_audio device node under the given parent.
  zx_status_t status = device_add(parent_bus, &args, &control->dev_node_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s could not add device '%s': %d", __func__, args.name, status);
    return status;
  }

  zxlogf(INFO, "%s added device '%s': %d", __func__, args.name, status);

  // Use the dispatcher supplied by the driver runtime.
  control->dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();

  // On successful Add, Devmgr takes ownership (relinquished on DdkRelease), so transfer our
  // ownership to a local var, and let it go out of scope.
  [[maybe_unused]] auto temp_ref = control.release();

  return ZX_OK;
}

// static
void VirtualAudioControlImpl::DdkUnbind(void* ctx) {
  ZX_ASSERT(ctx);

  auto self = static_cast<VirtualAudioControlImpl*>(ctx);
  if (self->devices_.empty()) {
    zxlogf(INFO, "%s with no devices; unbinding self", __func__);
    device_unbind_reply(self->dev_node_);
    return;
  }

  // Close any remaining device bindings, freeing those drivers.
  auto remaining = std::make_shared<size_t>(self->devices_.size());

  for (auto& d : self->devices_) {
    zxlogf(INFO, "%s with %lu devices; shutting one down", __func__, *remaining);
    d->ShutdownAsync([remaining, self]() {
      ZX_ASSERT(*remaining > 0);
      // After all devices are gone we can remove the control device itself.
      if (--(*remaining) == 0) {
        zxlogf(INFO, "DdkUnbind(lambda): after shutting down devices; unbinding self");
        device_unbind_reply(self->dev_node_);
      }
    });
  }
  self->devices_.clear();
}

// static
void VirtualAudioControlImpl::DdkRelease(void* ctx) {
  ZX_ASSERT(ctx);

  // Always called after DdkUnbind.
  // By now, all our lists should be empty and we can destroy the ctx.
  std::unique_ptr<VirtualAudioControlImpl> control_ptr(static_cast<VirtualAudioControlImpl*>(ctx));
  ZX_ASSERT(control_ptr->devices_.empty());
}

// static
void VirtualAudioControlImpl::DdkMessage(void* ctx, fidl_incoming_msg_t msg,
                                         device_fidl_txn_t txn) {
  VirtualAudioControlImpl* self = static_cast<VirtualAudioControlImpl*>(ctx);
  fidl::WireDispatch<fuchsia_virtualaudio::Control>(
      self, fidl::IncomingHeaderAndMessage::FromEncodedCMessage(msg),
      ddk::FromDeviceFIDLTransaction(txn));
}

void VirtualAudioControlImpl::GetDefaultConfiguration(
    GetDefaultConfigurationRequestView request, GetDefaultConfigurationCompleter::Sync& completer) {
  fidl::Arena arena;
  switch (request->type) {
    case fuchsia_virtualaudio::wire::DeviceType::kStreamConfig:
      completer.ReplySuccess(
          fidl::ToWire(arena, VirtualAudioStream::GetDefaultConfig(request->direction.is_input())));
      break;
    case fuchsia_virtualaudio::wire::DeviceType::kDai:
      completer.ReplySuccess(
          fidl::ToWire(arena, VirtualAudioDai::GetDefaultConfig(request->direction.is_input())));
      break;
    case fuchsia_virtualaudio::wire::DeviceType::kComposite:
      completer.ReplySuccess(fidl::ToWire(arena, VirtualAudioComposite::GetDefaultConfig()));
      break;
    default:
      ZX_ASSERT_MSG(0, "Unknown device type");
  }
}

void VirtualAudioControlImpl::AddDevice(AddDeviceRequestView request,
                                        AddDeviceCompleter::Sync& completer) {
  auto config = fidl::ToNatural(request->config);
  ZX_ASSERT(config.device_specific().has_value());
  auto result = VirtualAudioDeviceImpl::Create(std::move(config), std::move(request->server),
                                               dev_node_, dispatcher_);
  if (!result.is_ok()) {
    zxlogf(ERROR, "Device creation failed with status %d",
           fidl::ToUnderlying(result.error_value()));
    completer.ReplyError(result.error_value());
    return;
  }
  devices_.insert(result.value());
  completer.ReplySuccess();
}

void VirtualAudioControlImpl::GetNumDevices(GetNumDevicesCompleter::Sync& completer) {
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

void VirtualAudioControlImpl::RemoveAll(RemoveAllCompleter::Sync& completer) {
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
