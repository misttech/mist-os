// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pwm.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/metadata/cpp/metadata.h>

#include <bind/fuchsia/cpp/bind.h>

namespace pwm {

constexpr size_t kMaxConfigBufferSize = 256;

zx::result<> Pwm::Start() {
  zx::result pwm_impl = compat::ConnectBanjo<ddk::PwmImplProtocolClient>(incoming());
  if (pwm_impl.is_error()) {
    fdf::error("Failed to connect to pwm-impl: {}", pwm_impl);
    return pwm_impl.take_error();
  }

  zx::result metadata =
      fdf_metadata::GetMetadata<fuchsia_hardware_pwm::PwmChannelsMetadata>(*incoming());
  if (metadata.is_error()) {
    fdf::error("Failed to get metadata: {}", metadata);
    return metadata.take_error();
  }
  if (!metadata.value().channels().has_value()) {
    fdf::error("Metadata missing `channels` field");
    return zx::error(ZX_ERR_INTERNAL);
  }
  const auto& pwm_channels = metadata.value().channels().value();

  for (size_t i = 0; i < pwm_channels.size(); ++i) {
    const auto& pwm_channel_info = pwm_channels[i];
    if (!pwm_channel_info.id().has_value()) {
      fdf::error("PWM channel info {} missing `id` field", i);
      return zx::error(ZX_ERR_INTERNAL);
    }
    auto pwm_channel_id = pwm_channel_info.id().value();

    auto& pwm_channel = *pwm_channels_.emplace_back(
        std::make_unique<PwmChannel>(pwm_channel_id, dispatcher(), pwm_impl.value()));
    zx::result result = pwm_channel.Init(incoming(), outgoing(), node());
    if (result.is_error()) {
      fdf::error("Failed to initialize pwm channel {}: {}", i, result);
      return result.take_error();
    }
  }

  return zx::ok();
}

zx::result<> PwmChannel::Init(const std::shared_ptr<fdf::Namespace>& incoming,
                              std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                              fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent) {
  std::string child_node_name = std::format("pwm-{}", id_);

  {
    zx::result result = outgoing->AddService<fuchsia_hardware_pwm::Service>(
        fuchsia_hardware_pwm::Service::InstanceHandler{
            {.pwm = bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure)}},
        child_node_name);
    if (result.is_error()) {
      fdf::error("Failed to add pwm service to outgoing directory: {}", result);
      return result.take_error();
    }
  }

  {
    zx::result result =
        compat_server_.Initialize(incoming, outgoing, std::nullopt, child_node_name);
    if (result.is_error()) {
      fdf::error("Failed to initialize compat server: {}", result);
      return result.take_error();
    }
  }

  zx::result connector = devfs_connector_.Bind(dispatcher_);
  if (connector.is_error()) {
    fdf::error("Failed to bind devfs connector: {}", connector);
    return connector.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{
      {.connector = std::move(connector.value()),
       .class_name{kClassName},
       .connector_supports{fuchsia_device_fs::ConnectionType::kController}}};

  std::vector offers = compat_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_pwm::Service>(child_node_name));

  std::vector properties = {fdf::MakeProperty2(bind_fuchsia::PWM_ID, id_)};

  zx::result child = fdf::AddChild(parent, *fdf::Logger::GlobalInstance(), child_node_name,
                                   devfs_args, properties, offers);
  if (child.is_error()) {
    fdf::error("Failed to add child: {}", child);
    return child.take_error();
  }
  child_ = std::move(child.value());

  return zx::ok();
}

void PwmChannel::GetConfig(GetConfigCompleter::Sync& completer) {
  std::unique_ptr<uint8_t[]> buffer = std::make_unique<uint8_t[]>(kMaxConfigBufferSize);
  pwm_config_t config;
  config.mode_config_buffer = buffer.get();
  config.mode_config_size = kMaxConfigBufferSize;

  zx_status_t status = pwm_impl_.GetConfig(id_, &config);
  if (status != ZX_OK) {
    fdf::error("Failed to get config for pwm {}: {}", id_, zx_status_get_string(status));
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

void PwmChannel::SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) {
  pwm_config_t new_config;

  new_config.polarity = request->config.polarity;
  new_config.period_ns = request->config.period_ns;
  new_config.duty_cycle = request->config.duty_cycle;
  new_config.mode_config_buffer = request->config.mode_config.data();
  new_config.mode_config_size = request->config.mode_config.size();

  zx_status_t status = pwm_impl_.SetConfig(id_, &new_config);
  if (status != ZX_OK) {
    fdf::error("Failed to set config for pwm {}: {}", id_, zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }

  completer.ReplySuccess();
}

void PwmChannel::Enable(EnableCompleter::Sync& completer) {
  zx_status_t status = pwm_impl_.Enable(id_);
  if (status != ZX_OK) {
    fdf::error("Failed to enable pwm {}: {}", id_, zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }

  completer.ReplySuccess();
}

void PwmChannel::Disable(DisableCompleter::Sync& completer) {
  zx_status_t status = pwm_impl_.Disable(id_);
  if (status != ZX_OK) {
    fdf::error("Failed to disable pwm {}: {}", id_, zx_status_get_string(status));
    completer.ReplyError(status);
    return;
  }

  completer.ReplySuccess();
}

void PwmChannel::Connect(fidl::ServerEnd<fuchsia_hardware_pwm::Pwm> request) {
  bindings_.AddBinding(dispatcher_, std::move(request), this, fidl::kIgnoreBindingClosure);
}

}  // namespace pwm

FUCHSIA_DRIVER_EXPORT(pwm::Pwm);
