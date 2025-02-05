// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tcs3400.h"

#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/zx/clock.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/port.h>

#include <ddktl/fidl.h>
#include <ddktl/metadata/light-sensor.h>
#include <fbl/auto_lock.h>

#include "lib/inspect/cpp/vmo/types.h"
#include "tcs3400-regs.h"

namespace {
constexpr zx_duration_t INTERRUPTS_HYSTERESIS = ZX_MSEC(100);
constexpr uint8_t SAMPLES_TO_TRIGGER = 0x01;

// Repeat saturated log line every two minutes
constexpr zx::duration kSaturatedLogTimeSecs = zx::sec(120);
// Bright, not saturated values to return when saturated
constexpr uint16_t kMaxSaturationRed = 21'067;
constexpr uint16_t kMaxSaturationGreen = 20'395;
constexpr uint16_t kMaxSaturationBlue = 20'939;
constexpr uint16_t kMaxSaturationClear = 65'085;

constexpr int64_t kIntegrationTimeStepSizeMicroseconds = 2780;
constexpr int64_t kMinIntegrationTimeStep = 1;
constexpr int64_t kMaxIntegrationTimeStep = 256;

#define GET_BYTE(val, shift) static_cast<uint8_t>(((val) >> (shift)) & 0xFF)

constexpr fuchsia_input_report::wire::Axis kLightSensorAxis = {
    .range = {.min = 0, .max = UINT16_MAX},
    .unit =
        {
            .type = fuchsia_input_report::wire::UnitType::kOther,
            .exponent = 0,
        },
};

constexpr fuchsia_input_report::wire::Axis kReportIntervalAxis = {
    .range = {.min = 0, .max = INT64_MAX},
    .unit =
        {
            .type = fuchsia_input_report::wire::UnitType::kSeconds,
            .exponent = -6,
        },
};

constexpr fuchsia_input_report::wire::Axis kSensitivityAxis = {
    .range = {.min = 1, .max = 64},
    .unit =
        {
            .type = fuchsia_input_report::wire::UnitType::kOther,
            .exponent = 0,
        },
};

constexpr fuchsia_input_report::wire::Axis kSamplingRateAxis = {
    .range = {.min = kIntegrationTimeStepSizeMicroseconds,
              .max = kIntegrationTimeStepSizeMicroseconds * kMaxIntegrationTimeStep},
    .unit =
        {
            .type = fuchsia_input_report::wire::UnitType::kSeconds,
            .exponent = -6,
        },
};

constexpr fuchsia_input_report::wire::SensorAxis MakeLightSensorAxis(
    fuchsia_input_report::wire::SensorType type) {
  return {.axis = kLightSensorAxis, .type = type};
}

template <typename T>
bool FeatureValueValid(int64_t value, const T& axis) {
  return value >= axis.range.min && value <= axis.range.max;
}

constexpr size_t kMaxFeatureReports = 10;
constexpr std::string_view kEventTime("event_time");
constexpr std::string_view kReportingIntervalUs("report_interval_us");
constexpr std::string_view kReportingState("reporting_state");
constexpr std::string_view kSensitivity("sensitivity");
constexpr std::string_view kThresholdHigh("threshold_high");
constexpr std::string_view kThresholdLow("threshold_low");
constexpr std::string_view kIntegrationTimeUs("integration_time_us");

void RecordReport(inspect::Node& n, tcs::Tcs3400FeatureReport report) {
  n.RecordUint(kEventTime, (report.event_time - zx::time()).to_nsecs());
  n.RecordUint(kReportingIntervalUs, report.report_interval_us);
  n.RecordUint(kSensitivity, report.sensitivity);
  n.RecordUint(kThresholdHigh, report.threshold_high);
  n.RecordUint(kThresholdLow, report.threshold_low);
  n.RecordUint(kIntegrationTimeUs, report.integration_time_us);
  switch (report.reporting_state) {
    case fuchsia_input_report::SensorReportingState::kReportNoEvents:
      n.RecordString(kReportingState, "NoEvents");
      break;
    case fuchsia_input_report::SensorReportingState::kReportAllEvents:
      n.RecordString(kReportingState, "AllEvents");
      break;
    case fuchsia_input_report::SensorReportingState::kReportThresholdEvents:
      n.RecordString(kReportingState, "ThresholdEvents");
      break;
    default:
      n.RecordString(kReportingState, "Unknown");
      break;
  }
}

}  // namespace

namespace tcs {

void Tcs3400InputReport::ToFidlInputReport(
    fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
    fidl::AnyArena& allocator) const {
  fidl::VectorView<int64_t> values(allocator, 4);
  values[0] = illuminance;
  values[1] = red;
  values[2] = green;
  values[3] = blue;

  auto sensor_report =
      fuchsia_input_report::wire::SensorInputReport::Builder(allocator).values(values);
  input_report.event_time(event_time.get()).sensor(sensor_report.Build());
}

fuchsia_input_report::wire::FeatureReport Tcs3400FeatureReport::ToFidlFeatureReport(
    fidl::AnyArena& allocator) const {
  fidl::VectorView<int64_t> sens(allocator, 1);
  sens[0] = sensitivity;

  fidl::VectorView<int64_t> thresh_high(allocator, 1);
  thresh_high[0] = threshold_high;

  fidl::VectorView<int64_t> thresh_low(allocator, 1);
  thresh_low[0] = threshold_low;

  const auto sensor_report = fuchsia_input_report::wire::SensorFeatureReport::Builder(allocator)
                                 .report_interval(report_interval_us)
                                 .reporting_state(reporting_state)
                                 .sensitivity(sens)
                                 .threshold_high(thresh_high)
                                 .threshold_low(thresh_low)
                                 .sampling_rate(integration_time_us)
                                 .Build();

  return fuchsia_input_report::wire::FeatureReport::Builder(allocator)
      .sensor(sensor_report)
      .Build();
}

InspectTcs3400FeatureReport::InspectTcs3400FeatureReport(inspect::Node n,
                                                         const Tcs3400FeatureReport& report)
    : node(std::move(n)),
      event_time(node.CreateUint(kEventTime, 0)),
      report_interval_us(node.CreateUint(kReportingIntervalUs, 0)),
      reporting_state(node.CreateString(kReportingState, "Unknown")),
      sensitivity(node.CreateUint(kSensitivity, 0)),
      threshold_high(node.CreateUint(kThresholdHigh, 0)),
      threshold_low(node.CreateUint(kThresholdLow, 0)),
      integration_time_us(node.CreateUint(kIntegrationTimeUs, 0)) {
  event_time.Set((zx::clock::get_monotonic() - zx::time()).to_nsecs());
  threshold_low.Set(report.threshold_low);
  threshold_high.Set(report.threshold_high);
  sensitivity.Set(report.sensitivity);
  report_interval_us.Set(report.report_interval_us);
  switch (report.reporting_state) {
    case fuchsia_input_report::SensorReportingState::kReportNoEvents:
      reporting_state.Set("NoEvents");
      break;
    case fuchsia_input_report::SensorReportingState::kReportAllEvents:
      reporting_state.Set("AllEvents");
      break;
    case fuchsia_input_report::SensorReportingState::kReportThresholdEvents:
      reporting_state.Set("ThresholdEvents");
      break;
    default:
      reporting_state.Set("Unknown");
      break;
  }
  integration_time_us.Set(report.integration_time_us);
}

Tcs3400Device::Tcs3400Device(zx_device_t* device, async_dispatcher_t* dispatcher,
                             ddk::I2cChannel i2c, fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> gpio)
    : DeviceType(device),
      dispatcher_(dispatcher),
      i2c_(std::move(i2c)),
      gpio_(std::move(gpio)),
      inspect_reports_(inspect::contrib::BoundedListNode(
          inspect_.GetRoot().CreateChild("feature_reports"), kMaxFeatureReports)) {}

zx::result<Tcs3400InputReport> Tcs3400Device::ReadInputRpt() {
  Tcs3400InputReport report{.event_time = zx::clock::get_monotonic()};

  bool saturatedReading = false;
  struct Regs {
    int64_t* out;
    uint8_t reg_h;
    uint8_t reg_l;
  } regs[] = {
      {&report.illuminance, TCS_I2C_CDATAH, TCS_I2C_CDATAL},
      {&report.red, TCS_I2C_RDATAH, TCS_I2C_RDATAL},
      {&report.green, TCS_I2C_GDATAH, TCS_I2C_GDATAL},
      {&report.blue, TCS_I2C_BDATAH, TCS_I2C_BDATAL},
  };

  for (const auto& i : regs) {
    uint8_t buf_h, buf_l;
    zx_status_t status;
    // Read lower byte first, the device holds upper byte of a sample in a shadow register after
    // a lower byte read
    status = ReadReg(i.reg_l, buf_l);
    if (status != ZX_OK) {
      zxlogf(ERROR, "i2c_write_read_sync failed: %d", status);
      return zx::error(status);
    }
    status = ReadReg(i.reg_h, buf_h);
    if (status != ZX_OK) {
      zxlogf(ERROR, "i2c_write_read_sync failed: %d", status);
      return zx::error(status);
    }
    auto out = static_cast<uint16_t>(static_cast<float>(((buf_h & 0xFF) << 8) | (buf_l & 0xFF)));

    // Use memcpy here because i.out is a misaligned pointer and dereferencing a
    // misaligned pointer is UB. This ends up getting lowered to a 16-bit store.
    memcpy(i.out, &out, sizeof(out));

    zxlogf(DEBUG, "raw: 0x%04X  again: %u  atime: %u", out, again_, atime_);
  }

  uint8_t status_val;
  zx_status_t status;
  status = ReadReg(TCS_I2C_STATUS, status_val);
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c_write_read_sync failed: %d", status);
    return zx::error(status);
  }
  if ((status_val & TCS_I2C_STATUS_ASAT) == TCS_I2C_STATUS_ASAT) {
    status = WriteReg(TCS_I2C_CICLEAR, 0x00);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Unable to clear saturation status");
    }

    report.red = kMaxSaturationRed;
    report.green = kMaxSaturationGreen;
    report.blue = kMaxSaturationBlue;
    report.illuminance = kMaxSaturationClear;
    saturatedReading = true;
    if (!isSaturated_ || zx::clock::get_monotonic() - lastSaturatedLog_ >= kSaturatedLogTimeSecs) {
      zxlogf(INFO, "sensor is saturated via status register");
      lastSaturatedLog_ = zx::clock::get_monotonic();
    }
  } else if (isSaturated_) {
    zxlogf(INFO, "sensor is no longer saturated");
  }
  isSaturated_ = saturatedReading;

  return zx::ok(report);
}

void Tcs3400Device::Configure() {
  Tcs3400FeatureReport feature_report;
  {
    fbl::AutoLock lock(&feature_lock_);
    feature_report = feature_rpt_;
  }

  uint8_t control_reg = 0;
  // clang-format off
  if (feature_report.sensitivity == 4)  control_reg = 1;
  if (feature_report.sensitivity == 16) control_reg = 2;
  if (feature_report.sensitivity == 64) control_reg = 3;
  // clang-format on

  again_ = static_cast<uint8_t>(feature_report.sensitivity);

  const int64_t atime = feature_report.integration_time_us / kIntegrationTimeStepSizeMicroseconds;
  atime_ = static_cast<uint8_t>(kMaxIntegrationTimeStep - atime);

  struct Setup {
    uint8_t cmd;
    uint8_t val;
  } __PACKED setup[] = {
      // First we don't set TCS_I2C_ENABLE_ADC_ENABLE to disable the sensor.
      {TCS_I2C_ENABLE, TCS_I2C_ENABLE_POWER_ON | TCS_I2C_ENABLE_INT_ENABLE},
      {TCS_I2C_AILTL, GET_BYTE(feature_report.threshold_low, 0)},
      {TCS_I2C_AILTH, GET_BYTE(feature_report.threshold_low, 8)},
      {TCS_I2C_AIHTL, GET_BYTE(feature_report.threshold_high, 0)},
      {TCS_I2C_AIHTH, GET_BYTE(feature_report.threshold_high, 8)},
      {TCS_I2C_PERS, SAMPLES_TO_TRIGGER},
      {TCS_I2C_CONTROL, control_reg},
      {TCS_I2C_ATIME, atime_},
      // We now do set TCS_I2C_ENABLE_ADC_ENABLE to re-enable the sensor.
      {TCS_I2C_ENABLE,
       TCS_I2C_ENABLE_POWER_ON | TCS_I2C_ENABLE_ADC_ENABLE | TCS_I2C_ENABLE_INT_ENABLE},
  };
  for (const auto& i : setup) {
    auto status = WriteReg(i.cmd, i.val);
    if (status != ZX_OK) {
      zxlogf(ERROR, "i2c_write_sync failed: %d", status);
      break;  // do not exit thread, future transactions may succeed
    }
  }

  // per spec 0 is device's default. we define the default as no polling.
  polling_handler_.Cancel();
  if (feature_report.report_interval_us != 0) {
    polling_handler_.PostDelayed(dispatcher_, zx::usec(feature_report.report_interval_us));
  }
}

void Tcs3400Device::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq,
                              zx_status_t status, const zx_packet_interrupt_t* interrupt) {
  Tcs3400FeatureReport feature_report;
  {
    fbl::AutoLock lock(&feature_lock_);
    feature_report = feature_rpt_;
  }

  zx_interrupt_ack(irq_.get());  // rearm interrupt at the IRQ level

  const zx::result<Tcs3400InputReport> report = ReadInputRpt();
  if (report.is_error()) {
    rearm_irq_handler_.PostDelayed(dispatcher_, zx::duration(INTERRUPTS_HYSTERESIS));
    return;
  }
  if (feature_report.reporting_state ==
      fuchsia_input_report::wire::SensorReportingState::kReportNoEvents) {
    rearm_irq_handler_.PostDelayed(dispatcher_, zx::duration(INTERRUPTS_HYSTERESIS));
    return;
  }

  if (report->illuminance > feature_report.threshold_high ||
      report->illuminance < feature_report.threshold_low) {
    readers_.SendReportToAllReaders(std::move(*report));
  }

  fbl::AutoLock lock(&input_lock_);
  input_rpt_ = *report;

  rearm_irq_handler_.PostDelayed(dispatcher_, zx::duration(INTERRUPTS_HYSTERESIS));
}

void Tcs3400Device::RearmIrq() {
  // rearm interrupt at the device level
  auto status = WriteReg(TCS_I2C_AICLEAR, 0x00);
  if (status != ZX_OK) {
    zxlogf(ERROR, "i2c_write_sync failed: %d", status);
    // Continue on error, future transactions may succeed
  }
}

void Tcs3400Device::HandlePoll() {
  Tcs3400FeatureReport feature_report;
  {
    fbl::AutoLock lock(&feature_lock_);
    feature_report = feature_rpt_;
  }

  if (feature_report.reporting_state ==
      fuchsia_input_report::wire::SensorReportingState::kReportAllEvents) {
    const zx::result<Tcs3400InputReport> report = ReadInputRpt();
    if (report.is_ok()) {
      readers_.SendReportToAllReaders(std::move(*report));
      fbl::AutoLock lock(&input_lock_);
      input_rpt_ = *report;
    }
  }

  polling_handler_.PostDelayed(dispatcher_, zx::usec(feature_report.report_interval_us));
}

void Tcs3400Device::GetInputReportsReader(GetInputReportsReaderRequestView request,
                                          GetInputReportsReaderCompleter::Sync& completer) {
  readers_.CreateReader(dispatcher_, std::move(request->reader));
  sync_completion_signal(&next_reader_wait_);  // Only for tests.
}

void Tcs3400Device::GetDescriptor(GetDescriptorCompleter::Sync& completer) {
  using SensorAxisVector = fidl::VectorView<fuchsia_input_report::wire::SensorAxis>;

  fidl::Arena<kFeatureAndDescriptorBufferSize> allocator;

  auto device_info = fuchsia_input_report::wire::DeviceInformation::Builder(allocator);
  device_info.vendor_id(static_cast<uint32_t>(fuchsia_input_report::wire::VendorId::kGoogle));
  device_info.product_id(
      static_cast<uint32_t>(fuchsia_input_report::wire::VendorGoogleProductId::kAmsLightSensor));

  auto sensor_axes = SensorAxisVector(allocator, 4);
  sensor_axes[0] = MakeLightSensorAxis(fuchsia_input_report::wire::SensorType::kLightIlluminance);
  sensor_axes[1] = MakeLightSensorAxis(fuchsia_input_report::wire::SensorType::kLightRed);
  sensor_axes[2] = MakeLightSensorAxis(fuchsia_input_report::wire::SensorType::kLightGreen);
  sensor_axes[3] = MakeLightSensorAxis(fuchsia_input_report::wire::SensorType::kLightBlue);

  fidl::VectorView<fuchsia_input_report::wire::SensorInputDescriptor> input_descriptor(allocator,
                                                                                       1);
  input_descriptor[0] = fuchsia_input_report::wire::SensorInputDescriptor::Builder(allocator)
                            .values(sensor_axes)
                            .Build();

  auto sensitivity_axes = SensorAxisVector(allocator, 1);
  sensitivity_axes[0] = {
      .axis = kSensitivityAxis,
      .type = fuchsia_input_report::wire::SensorType::kLightIlluminance,
  };

  auto threshold_high_axes = SensorAxisVector(allocator, 1);
  threshold_high_axes[0] =
      MakeLightSensorAxis(fuchsia_input_report::wire::SensorType::kLightIlluminance);

  auto threshold_low_axes = SensorAxisVector(allocator, 1);
  threshold_low_axes[0] =
      MakeLightSensorAxis(fuchsia_input_report::wire::SensorType::kLightIlluminance);

  fidl::VectorView<fuchsia_input_report::wire::SensorFeatureDescriptor> feature_descriptor(
      allocator, 1);
  feature_descriptor[0] = fuchsia_input_report::wire::SensorFeatureDescriptor::Builder(allocator)
                              .report_interval(kReportIntervalAxis)
                              .supports_reporting_state(true)
                              .sensitivity(sensitivity_axes)
                              .threshold_high(threshold_high_axes)
                              .threshold_low(threshold_low_axes)
                              .sampling_rate(kSamplingRateAxis)
                              .Build();

  const auto sensor_descriptor = fuchsia_input_report::wire::SensorDescriptor::Builder(allocator)
                                     .input(input_descriptor)
                                     .feature(feature_descriptor)
                                     .Build();

  const auto descriptor = fuchsia_input_report::wire::DeviceDescriptor::Builder(allocator)
                              .device_information(device_info.Build())
                              .sensor(sensor_descriptor)
                              .Build();

  completer.Reply(descriptor);
}

void Tcs3400Device::SendOutputReport(SendOutputReportRequestView request,
                                     SendOutputReportCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Tcs3400Device::GetFeatureReport(GetFeatureReportCompleter::Sync& completer) {
  fbl::AutoLock lock(&feature_lock_);
  fidl::Arena<kFeatureAndDescriptorBufferSize> allocator;
  completer.ReplySuccess(feature_rpt_.ToFidlFeatureReport(allocator));
}

void Tcs3400Device::SetFeatureReport(SetFeatureReportRequestView request,
                                     SetFeatureReportCompleter::Sync& completer) {
  const auto& report = request->report;
  if (!report.has_sensor()) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (!report.sensor().has_report_interval() || report.sensor().report_interval() < 0) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (!report.sensor().has_sensitivity() || report.sensor().sensitivity().count() != 1 ||
      !FeatureValueValid(report.sensor().sensitivity()[0], kSensitivityAxis)) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  const int64_t gain = report.sensor().sensitivity()[0];
  if (!(gain == 1 || gain == 4 || gain == 16 || gain == 64)) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (!report.sensor().has_threshold_high() || report.sensor().threshold_high().count() != 1 ||
      !FeatureValueValid(report.sensor().threshold_high()[0], kLightSensorAxis)) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (!report.sensor().has_threshold_low() || report.sensor().threshold_low().count() != 1 ||
      !FeatureValueValid(report.sensor().threshold_low()[0], kLightSensorAxis)) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (!report.sensor().has_sampling_rate()) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  const int64_t atime = report.sensor().sampling_rate() / kIntegrationTimeStepSizeMicroseconds;
  if (atime < 1 || atime > 256) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  Tcs3400FeatureReport feature_report;
  {
    fbl::AutoLock lock(&feature_lock_);
    feature_rpt_.event_time = zx::clock::get_monotonic();
    feature_rpt_.report_interval_us = report.sensor().report_interval();
    feature_rpt_.reporting_state = report.sensor().reporting_state();
    feature_rpt_.sensitivity = report.sensor().sensitivity()[0];
    feature_rpt_.threshold_high = report.sensor().threshold_high()[0];
    feature_rpt_.threshold_low = report.sensor().threshold_low()[0];
    feature_rpt_.integration_time_us = atime * kIntegrationTimeStepSizeMicroseconds;
    feature_report = feature_rpt_;
  }
  inspect_reports_.CreateEntry(
      [feature_report](inspect::Node& n) { RecordReport(n, feature_report); });

  Configure();
  completer.ReplySuccess();
}

void Tcs3400Device::GetInputReport(GetInputReportRequestView request,
                                   GetInputReportCompleter::Sync& completer) {
  if (request->device_type != fuchsia_input_report::wire::DeviceType::kSensor) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  {
    fbl::AutoLock lock(&feature_lock_);
    if (feature_rpt_.reporting_state !=
        fuchsia_input_report::wire::SensorReportingState::kReportAllEvents) {
      // Light sensor data isn't continuously being read -- the data we have might be far out of
      // date, and we can't block to read new data from the sensor.
      completer.ReplyError(ZX_ERR_BAD_STATE);
      return;
    }
  }

  fidl::Arena<kFeatureAndDescriptorBufferSize> allocator;
  auto report = fuchsia_input_report::wire::InputReport::Builder(allocator);

  {
    fbl::AutoLock lock(&input_lock_);
    if (!input_rpt_.is_valid()) {
      // The driver is in the right mode, but hasn't had a chance to read from the sensor yet.
      completer.ReplyError(ZX_ERR_SHOULD_WAIT);
      return;
    }
    input_rpt_.ToFidlInputReport(report, allocator);
  }

  completer.ReplySuccess(report.Build());
}

void Tcs3400Device::WaitForNextReader() {
  sync_completion_wait(&next_reader_wait_, ZX_TIME_INFINITE);
  sync_completion_reset(&next_reader_wait_);
}

// static
zx_status_t Tcs3400Device::Create(void* ctx, zx_device_t* parent) {
  ddk::I2cChannel channel(parent, "i2c");
  if (!channel.is_valid()) {
    return ZX_ERR_NO_RESOURCES;
  }

  zx::result gpio =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(parent, "gpio");
  if (gpio.is_error()) {
    zxlogf(ERROR, "Failed to connect to gpio protocol: %s", gpio.status_string());
    return gpio.error_value();
  }

  auto dev = std::make_unique<tcs::Tcs3400Device>(
      parent, fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()),
      std::move(channel), std::move(gpio.value()));
  auto status = dev->Bind();
  if (status != ZX_OK) {
    zxlogf(ERROR, "bind failed: %d", status);
    return status;
  }
  // devmgr is now in charge of the memory for dev
  [[maybe_unused]] auto ptr = dev.release();

  return ZX_OK;
}

zx_status_t Tcs3400Device::InitGain(uint8_t gain) {
  if (!(gain == 1 || gain == 4 || gain == 16 || gain == 64)) {
    zxlogf(WARNING, "Invalid gain (%u) using gain = 1", gain);
    gain = 1;
  }

  again_ = gain;
  zxlogf(DEBUG, "again (%u)", again_);

  uint8_t reg;
  // clang-format off
  if (gain == 1)  reg = 0;
  if (gain == 4)  reg = 1;
  if (gain == 16) reg = 2;
  if (gain == 64) reg = 3;
  // clang-format on

  auto status = WriteReg(TCS_I2C_CONTROL, reg);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Setting gain failed %d", status);
    return status;
  }

  return ZX_OK;
}

zx_status_t Tcs3400Device::InitMetadata() {
  metadata::LightSensorParams parameters = {};
  size_t actual = {};
  auto status = device_get_metadata(parent(), DEVICE_METADATA_PRIVATE, &parameters,
                                    sizeof(metadata::LightSensorParams), &actual);
  if (status != ZX_OK || sizeof(metadata::LightSensorParams) != actual) {
    zxlogf(ERROR, "Failed to get metadata: %s", zx_status_get_string(status));
    return status;
  }

  // ATIME = 256 - Integration Time / 2.78 ms.
  int64_t atime = parameters.integration_time_us / kIntegrationTimeStepSizeMicroseconds;
  if (atime < kMinIntegrationTimeStep || atime > kMaxIntegrationTimeStep) {
    atime = kMaxIntegrationTimeStep - 1;
    zxlogf(WARNING, "Invalid integration time (%u) using atime = 1",
           parameters.integration_time_us);
  }
  atime_ = static_cast<uint8_t>(kMaxIntegrationTimeStep - atime);

  zxlogf(DEBUG, "atime (%u)", atime_);
  {
    status = WriteReg(TCS_I2C_ATIME, atime_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Setting integration time failed %d", status);
      return status;
    }
  }

  status = InitGain(parameters.gain);
  if (status != ZX_OK) {
    return status;
  }

  // Set the default features and send a configuration packet.
  Tcs3400FeatureReport feature_report;
  {
    fbl::AutoLock lock(&feature_lock_);
    // The device will trigger an interrupt outside the thresholds.  These default threshold
    // values effectively disable interrupts since we can't be outside this range, interrupts
    // get effectively enabled when we configure a range that could trigger.
    feature_rpt_.threshold_low = 0x0000;
    feature_rpt_.threshold_high = 0xFFFF;
    feature_rpt_.sensitivity = again_;
    feature_rpt_.report_interval_us = parameters.polling_time_us;
    feature_rpt_.reporting_state =
        fuchsia_input_report::wire::SensorReportingState::kReportAllEvents;
    feature_rpt_.integration_time_us = atime * kIntegrationTimeStepSizeMicroseconds;
    feature_report = feature_rpt_;
  }
  inspect_reports_.CreateEntry(
      [feature_report](inspect::Node& n) { RecordReport(n, feature_report); });

  Configure();
  return ZX_OK;
}

zx_status_t Tcs3400Device::ReadReg(uint8_t reg, uint8_t& output_value) {
  uint8_t write_buffer[] = {reg};
  constexpr uint8_t kNumberOfRetries = 2;
  constexpr zx::duration kRetryDelay = zx::msec(1);
  auto ret = i2c_.WriteReadSyncRetries(write_buffer, std::size(write_buffer), &output_value,
                                       sizeof(uint8_t), kNumberOfRetries, kRetryDelay);
  if (ret.status != ZX_OK) {
    zxlogf(ERROR, "I2C write reg 0x%02X error %d, %d retries", reg, ret.status, ret.retries);
  }
  return ret.status;
}

zx_status_t Tcs3400Device::WriteReg(uint8_t reg, uint8_t value) {
  uint8_t write_buffer[] = {reg, value};
  constexpr uint8_t kNumberOfRetries = 2;
  constexpr zx::duration kRetryDelay = zx::msec(1);
  auto ret =
      i2c_.WriteSyncRetries(write_buffer, std::size(write_buffer), kNumberOfRetries, kRetryDelay);
  if (ret.status != ZX_OK) {
    zxlogf(ERROR, "I2C write reg 0x%02X error %d, %d retries", reg, ret.status, ret.retries);
  }
  return ret.status;
}

zx_status_t Tcs3400Device::Bind() {
  zx_status_t status =
      DdkAdd(ddk::DeviceAddArgs("tcs-3400").set_inspect_vmo(inspect_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %d", status);
    return status;
  }

  fidl::Arena arena;
  auto interrupt_config = fuchsia_hardware_gpio::wire::InterruptConfiguration::Builder(arena)
                              .mode(fuchsia_hardware_gpio::InterruptMode::kEdgeLow)
                              .Build();
  fidl::WireResult configure_result = gpio_->ConfigureInterrupt(interrupt_config);
  if (!configure_result.ok()) {
    zxlogf(ERROR, "Failed to send ConfigureInterrupt request to gpio: %s",
           configure_result.status_string());
    return configure_result.status();
  }
  if (configure_result->is_error()) {
    zxlogf(ERROR, "Failed to configure interrupt: %s",
           zx_status_get_string(configure_result->error_value()));
    return configure_result->error_value();
  }

  fidl::WireResult interrupt_result = gpio_->GetInterrupt({});
  if (!interrupt_result.ok()) {
    zxlogf(ERROR, "Failed to send GetInterrupt request to gpio: %s",
           interrupt_result.status_string());
    return interrupt_result.status();
  }
  if (interrupt_result->is_error()) {
    zxlogf(ERROR, "Failed to get interrupt from gpio: %s",
           zx_status_get_string(interrupt_result->error_value()));
    return interrupt_result->error_value();
  }
  irq_ = std::move(interrupt_result->value()->interrupt);
  irq_handler_.set_object(irq_.get());
  irq_handler_.Begin(dispatcher_);

  status = InitMetadata();
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

void Tcs3400Device::DdkUnbind(ddk::UnbindTxn txn) {
  irq_handler_.Cancel();
  rearm_irq_handler_.Cancel();
  polling_handler_.Cancel();
  irq_.destroy();
  txn.Reply();
}

void Tcs3400Device::DdkRelease() { delete this; }

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = Tcs3400Device::Create;
  return ops;
}();

}  // namespace tcs

ZIRCON_DRIVER(tcs3400_light, tcs::driver_ops, "zircon", "0.1");
