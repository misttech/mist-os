// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pwm.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>

#include <bind/fuchsia/cpp/bind.h>
#include <fbl/alloc_checker.h>

namespace pwm {

constexpr size_t kMaxConfigBufferSize = 256;

zx_status_t PwmDevice::Create(void* ctx, zx_device_t* parent) {
  pwm_impl_protocol_t pwm_proto;
  auto status = device_get_protocol(parent, ZX_PROTOCOL_PWM_IMPL, &pwm_proto);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: device_get_protocol failed %d", __FILE__, status);
    return status;
  }

  auto metadata = ddk::GetEncodedMetadata<fuchsia_hardware_pwm::wire::PwmChannelsMetadata>(
      parent, DEVICE_METADATA_PWM_CHANNELS);
  if (!metadata.is_ok()) {
    return metadata.error_value();
  }

  auto pwm_channels = fidl::ToNatural(*metadata.value());
  for (auto pwm_channel : *pwm_channels.channels()) {
    fbl::AllocChecker ac;
    std::unique_ptr<PwmDevice> dev(new (&ac) PwmDevice(parent, &pwm_proto, pwm_channel));
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    char name[20];
    snprintf(name, sizeof(name), "pwm-%u", *pwm_channel.id());
    zx_device_str_prop_t props[] = {
        ddk::MakeStrProperty(bind_fuchsia::PWM_ID, *pwm_channel.id()),
    };

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.status_value();
    }

    std::array offers = {
        fuchsia_hardware_pwm::Service::Name,
    };

    dev->outgoing_server_end_ = std::move(endpoints->server);

    status = dev->DdkAdd(ddk::DeviceAddArgs(name)
                             .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                             .set_str_props(props)
                             .set_fidl_service_offers(offers)
                             .set_outgoing_dir(endpoints->client.TakeChannel()));
    if (status != ZX_OK) {
      return status;
    }

    // dev is now owned by devmgr.
    [[maybe_unused]] auto ptr = dev.release();
  }

  return ZX_OK;
}

void PwmDevice::DdkInit(ddk::InitTxn txn) {
  fdf_dispatcher_t* fdf_dispatcher = fdf_dispatcher_get_current_dispatcher();
  ZX_ASSERT(fdf_dispatcher);
  async_dispatcher_t* dispatcher = fdf_dispatcher_get_async_dispatcher(fdf_dispatcher);
  outgoing_ = component::OutgoingDirectory(dispatcher);

  fuchsia_hardware_pwm::Service::InstanceHandler handler({
      .pwm = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
  });
  auto result = outgoing_->AddService<fuchsia_hardware_pwm::Service>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory");
    txn.Reply(result.status_value());
    return;
  }

  result = outgoing_->Serve(std::move(outgoing_server_end_));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve the outgoing directory");
    txn.Reply(result.status_value());
    return;
  }

  txn.Reply(ZX_OK);
}

zx_status_t PwmDevice::PwmGetConfig(pwm_config_t* out_config) {
  std::scoped_lock lock(lock_);
  return pwm_.GetConfig(*channel_.id(), out_config);
}

zx_status_t PwmDevice::PwmSetConfig(const pwm_config_t* config) {
  std::scoped_lock lock(lock_);
  return pwm_.SetConfig(*channel_.id(), config);
}

zx_status_t PwmDevice::PwmEnable() {
  std::scoped_lock lock(lock_);
  return pwm_.Enable(*channel_.id());
}

zx_status_t PwmDevice::PwmDisable() {
  std::scoped_lock lock(lock_);
  return pwm_.Disable(*channel_.id());
}

void PwmDevice::GetConfig(GetConfigCompleter::Sync& completer) {
  std::unique_ptr<uint8_t[]> buffer = std::make_unique<uint8_t[]>(kMaxConfigBufferSize);
  pwm_config_t config;
  config.mode_config_buffer = buffer.get();
  config.mode_config_size = kMaxConfigBufferSize;

  zx_status_t status = PwmGetConfig(&config);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  fuchsia_hardware_pwm::wire::PwmConfig result;
  result.polarity = config.polarity;
  result.period_ns = config.period_ns;
  result.duty_cycle = config.duty_cycle;
  result.mode_config =
      fidl::VectorView<uint8_t>::FromExternal(config.mode_config_buffer, config.mode_config_size);

  completer.ReplySuccess(result);
}

void PwmDevice::SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) {
  pwm_config_t new_config;

  new_config.polarity = request->config.polarity;
  new_config.period_ns = request->config.period_ns;
  new_config.duty_cycle = request->config.duty_cycle;
  new_config.mode_config_buffer = request->config.mode_config.data();
  new_config.mode_config_size = request->config.mode_config.count();

  zx_status_t result = PwmSetConfig(&new_config);

  if (result != ZX_OK) {
    completer.ReplyError(result);
  } else {
    completer.ReplySuccess();
  }
}

void PwmDevice::Enable(EnableCompleter::Sync& completer) {
  zx_status_t result = PwmEnable();

  if (result == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(result);
  }
}

void PwmDevice::Disable(DisableCompleter::Sync& completer) {
  zx_status_t result = PwmDisable();

  if (result == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(result);
  }
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = PwmDevice::Create;
  return ops;
}();

}  // namespace pwm

ZIRCON_DRIVER(pwm, pwm::driver_ops, "zircon", "0.1");
