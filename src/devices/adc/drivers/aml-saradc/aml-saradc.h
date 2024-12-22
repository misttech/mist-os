// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_ADC_DRIVERS_AML_SARADC_AML_SARADC_H_
#define SRC_DEVICES_ADC_DRIVERS_AML_SARADC_AML_SARADC_H_

#include <fidl/fuchsia.hardware.adcimpl/cpp/driver/fidl.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include <fbl/mutex.h>
#include <sdk/lib/driver/metadata/cpp/metadata_server.h>

namespace aml_saradc {

class AmlSaradcDevice : public fdf::Server<fuchsia_hardware_adcimpl::Device> {
 public:
  AmlSaradcDevice(fdf::MmioBuffer adc_mmio, fdf::MmioBuffer ao_mmio, zx::interrupt irq)
      : adc_mmio_(std::move(adc_mmio)), ao_mmio_(std::move(ao_mmio)), irq_(std::move(irq)) {}

  void HwInit();
  void Shutdown();

  // fuchsia_hardware_adcimpl required methods.
  void GetResolution(GetResolutionCompleter::Sync& completer) override {
    completer.Reply(fit::ok(kSarAdcResolution));
  }
  void GetSample(GetSampleRequest& request, GetSampleCompleter::Sync& completer) override;

 private:
  enum ClkSrc { CLK_SRC_OSCIN, CLK_SRC_CLK81 };

  static constexpr uint8_t kMaxChannels = 8;
  static constexpr uint8_t kSarAdcResolution = 10;

  // TODO(b/314211318): Use clock driver instead of managing clocks.
  void ClkEna(bool ena);
  void InitClock(uint32_t src, uint32_t div);
  void Enable(bool ena);
  void SetClock(uint32_t src, uint32_t div);
  void Stop();

  const fdf::MmioBuffer adc_mmio_;
  const fdf::MmioBuffer ao_mmio_;
  const zx::interrupt irq_;
};

class AmlSaradc : public fdf::DriverBase {
 private:
  constexpr static const char kDeviceName[] = "aml-saradc";

 public:
  AmlSaradc(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDeviceName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

 private:
  zx::result<> CreateNode();

  std::unique_ptr<AmlSaradcDevice> device_;
  fdf::ServerBindingGroup<fuchsia_hardware_adcimpl::Device> bindings_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  compat::SyncInitializedDeviceServer compat_server_;
  fdf_metadata::MetadataServer<fuchsia_hardware_adcimpl::Metadata> metadata_server_;
};

}  // namespace aml_saradc

#endif  // SRC_DEVICES_ADC_DRIVERS_AML_SARADC_AML_SARADC_H_
