// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adc-buttons-device.h"

#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/zx/clock.h>

namespace adc_buttons_device {

void AdcButtonsDevice::AdcButtonInputReport::ToFidlInputReport(
    fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
    fidl::AnyArena& allocator) {
  fidl::VectorView<fuchsia_input_report::wire::ConsumerControlButton> pressed_buttons_rpt(
      allocator, buttons.size());
  std::copy(buttons.begin(), buttons.end(), pressed_buttons_rpt.begin());

  auto consumer_control_report =
      fuchsia_input_report::wire::ConsumerControlInputReport::Builder(allocator).pressed_buttons(
          pressed_buttons_rpt);
  input_report.event_time(event_time.get()).consumer_control(consumer_control_report.Build());
}

void AdcButtonsDevice::PollingTask(async_dispatcher_t* dispatcher, async::TaskBase* task,
                                   zx_status_t status) {
  if (status != ZX_OK) {
    return;
  }

  polling_task_.PostDelayed(dispatcher_, zx::usec(polling_rate_usec_));

  std::set<fuchsia_input_report::ConsumerControlButton> buttons;
  for (const auto& [chan, configs] : configs_) {
    uint32_t val;
    auto status = saradc_->GetSample(chan, &val);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "%s: GetSample failed %d", __func__, status);
      return;
    }

    for (const auto& cfg : configs) {
      const auto& saradc = cfg.button_config()->adc();
      if (val >= saradc->press_threshold() && val < saradc->release_threshold()) {
        buttons.insert(cfg.types()->begin(), cfg.types()->end());
      }
    }
  }
  ZX_DEBUG_ASSERT_MSG(
      buttons.size() <= fuchsia_input_report::kConsumerControlMaxNumButtons,
      "More buttons than expected (max = %d). Please increase kConsumerControlMaxNumButtons ",
      fuchsia_input_report::kConsumerControlMaxNumButtons);
  if (rpt_.buttons == buttons) {
    return;
  }

  rpt_.buttons = std::move(buttons);
  rpt_.event_time = zx::clock::get_monotonic();
  readers_.SendReportToAllReaders(rpt_);
}

void AdcButtonsDevice::GetInputReportsReader(GetInputReportsReaderRequestView request,
                                             GetInputReportsReaderCompleter::Sync& completer) {
  auto status = readers_.CreateReader(dispatcher_, std::move(request->reader));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: CreateReader failed %d", __func__, status);
  }
}

void AdcButtonsDevice::GetDescriptor(GetDescriptorCompleter::Sync& completer) {
  fidl::Arena<kFeatureAndDescriptorBufferSize> allocator;

  fuchsia_input_report::wire::DeviceInfo device_info;
  device_info.vendor_id = static_cast<uint32_t>(fuchsia_input_report::wire::VendorId::kGoogle);
  device_info.product_id =
      static_cast<uint32_t>(fuchsia_input_report::wire::VendorGoogleProductId::kAdcButtons);
  device_info.polling_rate = polling_rate_usec_;

  fidl::VectorView<fuchsia_input_report::wire::ConsumerControlButton> buttons(allocator,
                                                                              buttons_.size());
  std::copy(buttons_.begin(), buttons_.end(), buttons.begin());

  const auto input = fuchsia_input_report::wire::ConsumerControlInputDescriptor::Builder(allocator)
                         .buttons(buttons)
                         .Build();

  const auto consumer_control =
      fuchsia_input_report::wire::ConsumerControlDescriptor::Builder(allocator)
          .input(input)
          .Build();

  const auto descriptor = fuchsia_input_report::wire::DeviceDescriptor::Builder(allocator)
                              .device_info(device_info)
                              .consumer_control(consumer_control)
                              .Build();

  completer.Reply(descriptor);
}

void AdcButtonsDevice::Shutdown() {
  polling_task_.Cancel();
  saradc_->Shutdown();
}

}  // namespace adc_buttons_device
