// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_PWM_DRIVERS_PWM_PWM_H_
#define SRC_DEVICES_PWM_DRIVERS_PWM_PWM_H_

#include <fidl/fuchsia.hardware.pwm/cpp/fidl.h>
#include <fidl/fuchsia.hardware.pwm/cpp/wire.h>
#include <fuchsia/hardware/pwm/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <mutex>

namespace pwm {

class PwmChannel : public fidl::WireServer<fuchsia_hardware_pwm::Pwm> {
 public:
  static constexpr std::string_view kClassName = "pwm";

  explicit PwmChannel(uint32_t id, async_dispatcher_t* dispatcher,
                      ddk::PwmImplProtocolClient pwm_impl)
      : id_(id), pwm_impl_(pwm_impl), dispatcher_(dispatcher) {}

  zx::result<> Init(const std::shared_ptr<fdf::Namespace>& incoming,
                    std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent);

  // fidl::WireServer<fuchsia_hardware_pwm::Pwm> implementation.
  void GetConfig(GetConfigCompleter::Sync& completer) override;
  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override;
  void Enable(EnableCompleter::Sync& completer) override;
  void Disable(DisableCompleter::Sync& completer) override;

 private:
  void Connect(fidl::ServerEnd<fuchsia_hardware_pwm::Pwm> request);

  // ID of the pwm channel.
  const uint32_t id_;

  ddk::PwmImplProtocolClient pwm_impl_;

  async_dispatcher_t* dispatcher_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::ServerBindingGroup<fuchsia_hardware_pwm::Pwm> bindings_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> child_;
  driver_devfs::Connector<fuchsia_hardware_pwm::Pwm> devfs_connector_{
      fit::bind_member<&PwmChannel::Connect>(this)};
};

class Pwm : public fdf::DriverBase {
 public:
  static constexpr std::string_view kDriverName = "pwm";

  Pwm(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  // fdf::DriverBase implementation.
  zx::result<> Start() override;

 private:
  std::vector<std::unique_ptr<PwmChannel>> pwm_channels_;
};

}  // namespace pwm

#endif  // SRC_DEVICES_PWM_DRIVERS_PWM_PWM_H_
