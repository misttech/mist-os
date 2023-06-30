// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_INPUT_DRIVERS_FOCALTECH_FT_DEVICE_H_
#define SRC_UI_INPUT_DRIVERS_FOCALTECH_FT_DEVICE_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/ddk/device.h>
#include <lib/device-protocol/i2c-channel.h>
#include <lib/focaltech/focaltech.h>
#include <lib/input_report_reader/reader.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <threads.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <atomic>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <fbl/mutex.h>

// clang-format off
#define FTS_REG_CURPOINT                    0x02
#define FTS_REG_FINGER_START                0x03
#define FTS_REG_INT_CNT                     0x8F
#define FTS_REG_FLOW_WORK_CNT               0x91
#define FTS_REG_WORKMODE                    0x00
#define FTS_REG_WORKMODE_FACTORY_VALUE      0x40
#define FTS_REG_WORKMODE_WORK_VALUE         0x00
#define FTS_REG_ESDCHECK_DISABLE            0x8D
#define FTS_REG_CHIP_ID                     0xA3
#define FTS_REG_CHIP_ID2                    0x9F
#define FTS_REG_POWER_MODE                  0xA5
#define FTS_REG_POWER_MODE_SLEEP_VALUE      0x03
#define FTS_REG_FW_VER                      0xA6
#define FTS_REG_VENDOR_ID                   0xA8
#define FTS_REG_LCD_BUSY_NUM                0xAB
#define FTS_REG_FACE_DEC_MODE_EN            0xB0
#define FTS_REG_FACE_DEC_MODE_STATUS        0x01
#define FTS_REG_IDE_PARA_VER_ID             0xB5
#define FTS_REG_IDE_PARA_STATUS             0xB6
#define FTS_REG_GLOVE_MODE_EN               0xC0
#define FTS_REG_COVER_MODE_EN               0xC1
#define FTS_REG_CHARGER_MODE_EN             0x8B
#define FTS_REG_GESTURE_EN                  0xD0
#define FTS_REG_GESTURE_OUTPUT_ADDRESS      0xD3
#define FTS_REG_MODULE_ID                   0xE3
#define FTS_REG_LIC_VER                     0xE4
#define FTS_REG_ESD_SATURATE                0xED
#define FTS_REG_TYPE                        0xA0  // Chip model number (refer to datasheet)
#define FTS_REG_FIRMID                      0xA6  // Firmware version
#define FTS_REG_VENDOR_ID                   0xA8
#define FTS_REG_PANEL_ID                    0xAC
#define FTS_REG_RELEASE_ID_HIGH             0xAE  // Firmware release ID (two bytes)
#define FTS_REG_RELEASE_ID_LOW              0xAF
#define FTS_REG_IC_VERSION                  0xB1
// clang-format on

namespace ft {

class FtDevice;
using DeviceType = ddk::Device<FtDevice, ddk::Messageable<fuchsia_input_report::InputDevice>::Mixin,
                               ddk::Unbindable>;

class FtDevice : public DeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_INPUTREPORT> {
 public:
  explicit FtDevice(zx_device_t* device, async_dispatcher_t* dispatcher)
      : DeviceType(device), dispatcher_(dispatcher) {}

  static zx_status_t Create(void* ctx, zx_device_t* device);

  void DdkRelease();
  void DdkUnbind(ddk::UnbindTxn txn);

  // fuchsia_input_report::InputDevice required methods
  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override;
  void GetDescriptor(GetDescriptorCompleter::Sync& completer) override;
  void SendOutputReport(SendOutputReportRequestView request,
                        SendOutputReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetFeatureReport(GetFeatureReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void SetFeatureReport(SetFeatureReportRequestView request,
                        SetFeatureReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void GetInputReport(GetInputReportRequestView request,
                      GetInputReportCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  // Visible for testing.
  zx_status_t Init();
  void StartThread();
  zx_status_t ShutDown();

#ifdef FT_TEST
  zx_status_t WaitForNextReader(zx::duration timeout) {
    zx_status_t status = sync_completion_wait(&next_reader_wait_, timeout.get());
    if (status == ZX_OK) {
      sync_completion_reset(&next_reader_wait_);
    }
    return status;
  }
#endif

 private:
  static constexpr size_t kFeatureAndDescriptorBufferSize = 512;

  /* Note: the focaltouch device is connected via i2c and is NOT a HID
      device.  This driver reads a collection of data from the data and
      parses it into a message which will be sent up the stack.  This message
      complies with a HID descriptor that manually scripted (i.e. - not
      reported by the device iteself).
  */
  // Number of touch points this device can report simultaneously
  static constexpr uint32_t kMaxPoints = 5;
  // Size of each individual touch record (note: there are kMaxPoints of
  //  them) on the i2c bus.  This is not the HID report size.
  static constexpr uint32_t kFingerRptSize = 6;

  static constexpr size_t kMaxI2cTransferLength = 8;

  struct FtInputReport {
    zx::time event_time = zx::time(ZX_TIME_INFINITE_PAST);
    uint8_t contact_count;
    struct Contact {
      uint8_t finger_id;
      uint16_t x;
      uint16_t y;
    };
    std::array<Contact, kMaxPoints> contacts;

    void ToFidlInputReport(
        fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
        fidl::AnyArena& allocator);
  };

  static uint8_t CalculateEcc(const uint8_t* buffer, size_t size, uint8_t initial = 0);

  // Enters romboot and returns true if firmware download is needed, returns false otherwise.
  zx::result<bool> CheckFirmwareAndStartRomboot(uint8_t firmware_version);
  zx_status_t StartRomboot();
  zx_status_t WaitForRomboot();

  zx::result<uint16_t> GetBootId();

  // Returns true if the expected value was read before the timeout, false if not.
  zx::result<bool> WaitForFlashStatus(uint16_t expected_value, int tries, zx::duration retry_sleep);

  zx_status_t EraseFlash(size_t firmware_size);
  zx_status_t SendFirmware(cpp20::span<const uint8_t> firmware);
  zx_status_t SendFirmwarePacket(uint32_t address, const uint8_t* buffer, size_t size);
  zx_status_t CheckFirmwareEcc(size_t size, uint8_t expected_ecc);

  zx::result<uint8_t> ReadReg8(uint8_t address);
  zx::result<uint16_t> ReadReg16(uint8_t address);

  zx_status_t Write8(uint8_t value);
  zx_status_t WriteReg8(uint8_t address, uint8_t value);
  zx_status_t WriteReg16(uint8_t address, uint16_t value);

  uint8_t Read(uint8_t addr);
  zx_status_t Read(uint8_t addr, uint8_t* buf, size_t len);

  int Thread();

  static FtInputReport ParseReport(const uint8_t* buf);

  void LogRegisterValue(uint8_t addr, const char* name);

  zx_status_t UpdateFirmwareIfNeeded(const FocaltechMetadata& metadata);

  async_dispatcher_t* dispatcher_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> int_gpio_;
  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> reset_gpio_;
  zx::interrupt irq_;
  ddk::I2cChannel i2c_;

  thrd_t thread_;
  std::atomic<bool> running_;

  inspect::Inspector inspector_;
  inspect::Node node_;
  inspect::ValueList values_;

  inspect::Node metrics_root_;
  inspect::UintProperty average_latency_usecs_;
  inspect::UintProperty max_latency_usecs_;
  inspect::UintProperty total_report_count_;
  inspect::UintProperty last_event_timestamp_;

  uint64_t report_count_ = 0;
  zx::duration total_latency_ = {};
  zx::duration max_latency_ = {};

  input_report_reader::InputReportReaderManager<FtInputReport> readers_;
  uint32_t x_max_;
  uint32_t y_max_;

#ifdef FT_TEST
  sync_completion_t next_reader_wait_;
#endif
};
}  // namespace ft

#endif  // SRC_UI_INPUT_DRIVERS_FOCALTECH_FT_DEVICE_H_
