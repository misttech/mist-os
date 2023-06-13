// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual_audio/virtual_audio_device_impl.h"

#include <lib/ddk/debug.h>
#include <lib/zx/clock.h>

#include <memory>

#include "src/media/audio/drivers/virtual_audio/virtual_audio_dai.h"
#include "src/media/audio/drivers/virtual_audio/virtual_audio_stream.h"

namespace virtual_audio {

// static
fit::result<fuchsia_virtualaudio::Error, std::shared_ptr<VirtualAudioDeviceImpl>>
VirtualAudioDeviceImpl::Create(const fuchsia_virtualaudio::Configuration& cfg,
                               fidl::ServerEnd<fuchsia_virtualaudio::Device> server,
                               zx_device_t* dev_node, async_dispatcher_t* fidl_dispatcher) {
  std::optional<bool> is_input;
  switch (cfg.device_specific()->Which()) {
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kStreamConfig:
      is_input = cfg.device_specific()->stream_config()->is_input();
      break;
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kDai:
      is_input = cfg.device_specific()->dai()->is_input();
      break;
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kComposite:
      [[fallthrough]];
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kCodec:
      [[fallthrough]];
    default:
      zxlogf(ERROR, "Device type creation not supported");
      return fit::error(fuchsia_virtualaudio::Error::kInternal);
  }
  auto device = std::make_shared<VirtualAudioDeviceImpl>(std::move(is_input), fidl_dispatcher);

  // The `device` shared_ptr is held until the server is unbound (i.e. until the channel is closed).
  device->binding_ = fidl::BindServer(
      fidl_dispatcher, std::move(server), device,
      [device](auto* server, fidl::UnbindInfo info, auto server_end) {
        zxlogf(INFO, "Device closed with reason '%s'", info.FormatDescription().c_str());
        device->is_bound_ = false;
        if (device->driver_) {
          device->driver_->ShutdownAndRemove();
          // Drop our driver reference.
          device->driver_ = nullptr;
        }
        // Run destroy callbacks.
        for (auto& cb : device->on_destroy_callbacks_) {
          cb();
        }
      });

  switch (cfg.device_specific()->Which()) {
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kStreamConfig:
      device->driver_ = std::make_unique<VirtualAudioStreamWrapper>(cfg, device, dev_node);
      break;
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kDai:
      device->driver_ = std::make_unique<VirtualAudioDai>(cfg, device, dev_node);
      break;
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kComposite:
      [[fallthrough]];
    case fuchsia_virtualaudio::DeviceSpecific::Tag::kCodec:
      [[fallthrough]];
    default:
      zxlogf(ERROR, "Device type creation not supported");
      return fit::error(fuchsia_virtualaudio::Error::kInternal);
  }
  // Ensure the driver was created successfully.
  if (!device->driver_) {
    zxlogf(ERROR, "Device creation failed with unspecified internal error");
    return fit::error(fuchsia_virtualaudio::Error::kInternal);
  }

  return fit::ok(device);
}

VirtualAudioDeviceImpl::VirtualAudioDeviceImpl(std::optional<bool> is_input,
                                               async_dispatcher_t* fidl_dispatcher)
    : is_input_(std::move(is_input)), fidl_dispatcher_(fidl_dispatcher) {}

VirtualAudioDeviceImpl::~VirtualAudioDeviceImpl() {
  // The driver should have been unbound by our on_unbound handler.
  ZX_ASSERT(driver_ == nullptr);
}

// Post the given task with automatic cancellation if the device is cancelled before the task fires.
void VirtualAudioDeviceImpl::PostToDispatcher(fit::closure task_to_post) {
  async::PostTask(fidl_dispatcher_,
                  [weak = weak_from_this(), task_to_post = std::move(task_to_post)]() {
                    if (weak.lock()) {
                      task_to_post();
                    }
                  });
}

void VirtualAudioDeviceImpl::ShutdownAsync(fit::closure cb) {
  if (is_bound_) {
    on_destroy_callbacks_.emplace_back(std::move(cb));
    binding_->Unbind();
  } else {
    cb();
  }
}

// This is called when the driver is destroyed by an external entity (perhaps the device host
// process is removing our driver driver). When this happens, drop the driver so we stop making
// requests to the driver.
void VirtualAudioDeviceImpl::DriverIsShuttingDown() { binding_->Unbind(); }

//
// virtualaudio::Device implementation
//

void VirtualAudioDeviceImpl::GetFormat(GetFormatCompleter::Sync& completer) {
  if (!driver_) {
    zxlogf(WARNING, "%s: %p has no driver for this request", __func__, this);
    return;
  }

  driver_->PostToDispatcher([driver = driver_.get(), completer = completer.ToAsync()]() mutable {
    VirtualAudioDriver::ScopedToken t(driver->domain_token());
    auto result = driver->GetFormatForVA();
    if (!result.is_ok()) {
      completer.ReplyError(result.error_value());
      return;
    }
    auto& v = result.value();
    completer.ReplySuccess(v.frames_per_second, v.sample_format, v.num_channels,
                           v.external_delay.get());
  });
}

// Deliver SetFormat notification on binding's thread, if binding is valid.
void VirtualAudioDeviceImpl::NotifySetFormat(uint32_t frames_per_second, uint32_t sample_format,
                                             uint32_t num_channels, zx_duration_t external_delay) {
  PostToDispatcher(
      [weak = weak_from_this(), frames_per_second, sample_format, num_channels, external_delay]() {
        auto self = weak.lock();
        if (!self) {
          return;
        }
        fidl::Status status =
            fidl::WireSendEvent(self->binding_.value())
                ->OnSetFormat(frames_per_second, sample_format, num_channels, external_delay);
        if (status.status() != ZX_OK) {
          zxlogf(WARNING, "OnSetFormat failed with status %s", status.status_string());
        }
      });
}

void VirtualAudioDeviceImpl::GetGain(GetGainCompleter::Sync& completer) {
  if (!driver_) {
    zxlogf(WARNING, "%s: %p has no driver for this request", __func__, this);
    return;
  }

  driver_->PostToDispatcher([driver = driver_.get(), completer = completer.ToAsync()]() mutable {
    VirtualAudioDriver::ScopedToken t(driver->domain_token());
    auto result = driver->GetGainForVA();
    if (!result.is_ok()) {
      completer.ReplyError(result.error_value());
      return;
    }
    auto& v = result.value();
    completer.ReplySuccess(v.mute, v.agc, v.gain_db);
  });
}

void VirtualAudioDeviceImpl::NotifySetGain(bool current_mute, bool current_agc,
                                           float current_gain_db) {
  PostToDispatcher([weak = weak_from_this(), current_mute, current_agc, current_gain_db]() {
    auto self = weak.lock();
    if (!self) {
      return;
    }
    fidl::Status status = fidl::WireSendEvent(self->binding_.value())
                              ->OnSetGain(current_mute, current_agc, current_gain_db);
    if (status.status() != ZX_OK) {
      zxlogf(WARNING, "OnSetGain failed with status %s", status.status_string());
    }
  });
}

void VirtualAudioDeviceImpl::GetBuffer(GetBufferCompleter::Sync& completer) {
  if (!driver_) {
    zxlogf(WARNING, "%s: %p has no driver for this request", __func__, this);
    return;
  }

  driver_->PostToDispatcher([driver = driver_.get(), completer = completer.ToAsync()]() mutable {
    VirtualAudioDriver::ScopedToken t(driver->domain_token());
    auto result = driver->GetBufferForVA();
    if (!result.is_ok()) {
      completer.ReplyError(result.error_value());
      return;
    }
    auto& v = result.value();
    completer.ReplySuccess(std::move(v.vmo), v.num_frames, v.notifications_per_ring);
  });
}

void VirtualAudioDeviceImpl::NotifyBufferCreated(zx::vmo ring_buffer_vmo,
                                                 uint32_t num_ring_buffer_frames,
                                                 uint32_t notifications_per_ring) {
  PostToDispatcher([weak = weak_from_this(), ring_buffer_vmo = std::move(ring_buffer_vmo),
                    num_ring_buffer_frames, notifications_per_ring]() mutable {
    auto self = weak.lock();
    if (!self) {
      return;
    }
    fidl::Status status = fidl::WireSendEvent(self->binding_.value())
                              ->OnBufferCreated(std::move(ring_buffer_vmo), num_ring_buffer_frames,
                                                notifications_per_ring);
    if (status.status() != ZX_OK) {
      zxlogf(WARNING, "OnBufferCreated failed with status %s", status.status_string());
    }
  });
}

void VirtualAudioDeviceImpl::SetNotificationFrequency(
    SetNotificationFrequencyRequestView request,
    SetNotificationFrequencyCompleter::Sync& completer) {
  if (!driver_) {
    zxlogf(WARNING, "%s: %p has no driver for this request", __func__, this);
    return;
  }

  driver_->PostToDispatcher([driver = driver_.get(),
                             notifications_per_ring = request->notifications_per_ring,
                             completer = completer.ToAsync()]() mutable {
    VirtualAudioDriver::ScopedToken t(driver->domain_token());
    auto result = driver->SetNotificationFrequencyFromVA(notifications_per_ring);
    if (!result.is_ok()) {
      completer.ReplyError(result.error_value());
      return;
    }
    completer.ReplySuccess();
  });
}

void VirtualAudioDeviceImpl::NotifyStart(zx_time_t start_time) {
  PostToDispatcher([weak = weak_from_this(), start_time]() {
    auto self = weak.lock();
    if (!self) {
      return;
    }
    fidl::Status status = fidl::WireSendEvent(self->binding_.value())->OnStart(start_time);
    if (status.status() != ZX_OK) {
      zxlogf(WARNING, "OnStart failed with status %s", status.status_string());
    }
  });
}

void VirtualAudioDeviceImpl::NotifyStop(zx_time_t stop_time, uint32_t ring_buffer_position) {
  PostToDispatcher([weak = weak_from_this(), stop_time, ring_buffer_position]() {
    auto self = weak.lock();
    if (!self) {
      return;
    }
    fidl::Status status =
        fidl::WireSendEvent(self->binding_.value())->OnStop(stop_time, ring_buffer_position);
    if (status.status() != ZX_OK) {
      zxlogf(WARNING, "OnStop failed with status %s", status.status_string());
    }
  });
}

void VirtualAudioDeviceImpl::GetPosition(GetPositionCompleter::Sync& completer) {
  if (!driver_) {
    zxlogf(WARNING, "%s: %p has no driver for this request", __func__, this);
    return;
  }

  driver_->PostToDispatcher([driver = driver_.get(), completer = completer.ToAsync()]() mutable {
    VirtualAudioDriver::ScopedToken t(driver->domain_token());
    auto result = driver->GetPositionForVA();
    if (!result.is_ok()) {
      completer.ReplyError(result.error_value());
      return;
    }
    auto& v = result.value();
    completer.ReplySuccess(v.monotonic_time.get(), v.ring_position);
  });
}

void VirtualAudioDeviceImpl::NotifyPosition(zx_time_t monotonic_time,
                                            uint32_t ring_buffer_position) {
  PostToDispatcher([weak = weak_from_this(), monotonic_time, ring_buffer_position]() {
    auto self = weak.lock();
    if (!self) {
      return;
    }
    fidl::Status status = fidl::WireSendEvent(self->binding_.value())
                              ->OnPositionNotify(monotonic_time, ring_buffer_position);
    if (status.status() != ZX_OK) {
      zxlogf(WARNING, "OnPositionNotify failed with status %s", status.status_string());
    }
  });
}

void VirtualAudioDeviceImpl::ChangePlugState(ChangePlugStateRequestView request,
                                             ChangePlugStateCompleter::Sync& completer) {
  if (!driver_) {
    zxlogf(WARNING, "%s: %p has no driver; cannot change dynamic plug state", __func__, this);
    return;
  }

  driver_->PostToDispatcher([driver = driver_.get(), plugged = request->plugged,
                             completer = completer.ToAsync()]() mutable {
    VirtualAudioDriver::ScopedToken t(driver->domain_token());
    auto result = driver->ChangePlugStateFromVA(plugged);
    if (!result.is_ok()) {
      completer.ReplyError(result.error_value());
      return;
    }
    completer.ReplySuccess();
  });
}

void VirtualAudioDeviceImpl::AdjustClockRate(AdjustClockRateRequestView request,
                                             AdjustClockRateCompleter::Sync& completer) {
  if (!driver_) {
    zxlogf(WARNING, "%s: %p has no driver; cannot change clock rate", __func__, this);
    return;
  }

  driver_->PostToDispatcher([driver = driver_.get(),
                             ppm_from_monotonic = request->ppm_from_monotonic,
                             completer = completer.ToAsync()]() mutable {
    VirtualAudioDriver::ScopedToken t(driver->domain_token());
    auto result = driver->AdjustClockRateFromVA(ppm_from_monotonic);
    if (!result.is_ok()) {
      completer.ReplyError(result.error_value());
      return;
    }
    completer.ReplySuccess();
  });
}

}  // namespace virtual_audio
