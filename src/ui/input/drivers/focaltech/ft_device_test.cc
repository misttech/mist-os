// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ft_device.h"

#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/fake-i2c/fake-i2c.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/focaltech/focaltech.h>
#include <lib/zx/clock.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstddef>

#include <gtest/gtest.h>

#include "ft_firmware.h"
#include "lib/zx/result.h"
#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wc99-designator"
// Firmware must be at least 0x120 bytes. Add some extra size to make them different.
constexpr uint8_t kFirmware0[0x120 + 0] = {0x00, 0xd2, 0xc8, 0x53, [0x10a] = 0xd5};
constexpr uint8_t kFirmware1[0x120 + 1] = {0x10, 0x58, 0xb2, 0x12, [0x10a] = 0xc8};
constexpr uint8_t kFirmware2[0x120 + 2] = {0xb7, 0xf9, 0xd1, 0x12, [0x10a] = 0xb0};
constexpr uint8_t kFirmware3[0x120 + 3] = {0x02, 0x69, 0x96, 0x71, [0x10a] = 0x61};
#pragma GCC diagnostic pop

}  // namespace

namespace ft {

const FirmwareEntry kFirmwareEntries[] = {
    {
        .display_vendor = 0,
        .ddic_version = 0,
        .firmware_data = kFirmware0,
        .firmware_size = sizeof(kFirmware0),
    },
    {
        .display_vendor = 1,
        .ddic_version = 0,
        .firmware_data = kFirmware1,
        .firmware_size = sizeof(kFirmware1),
    },
    {
        .display_vendor = 0,
        .ddic_version = 1,
        .firmware_data = kFirmware2,
        .firmware_size = sizeof(kFirmware2),
    },
    {
        .display_vendor = 1,
        .ddic_version = 1,
        .firmware_data = kFirmware3,
        .firmware_size = sizeof(kFirmware3),
    },
};

const size_t kNumFirmwareEntries = std::size(kFirmwareEntries);

class FakeFtDevice : public fake_i2c::FakeI2c {
 public:
  ~FakeFtDevice() { ZX_ASSERT(expected_report_.empty()); }

  uint32_t firmware_write_size() const { return firmware_write_size_; }

  void ExpectReport(uint8_t addr, std::vector<uint8_t> report) {
    expected_report_.emplace(addr, std::move(report));
  }

 protected:
  zx_status_t Transact(const uint8_t* write_buffer, size_t write_buffer_size, uint8_t* read_buffer,
                       size_t* read_buffer_size) override {
    if (write_buffer_size < 1) {
      return ZX_ERR_IO;
    }

    *read_buffer_size = 0;

    if (write_buffer[0] == 0xa3) {  // Chip core register
      read_buffer[0] = 0x58;        // Firmware is valid
      *read_buffer_size = 1;
    } else if (write_buffer[0] == 0xa6) {  // Chip firmware version
      read_buffer[0] = kFirmware1[0x10a];  // Set to a known version to test the up-to-date case
      *read_buffer_size = 1;
    } else if (write_buffer[0] == 0xfc && write_buffer_size == 2) {  // Chip work mode
      if (write_buffer[1] != 0xaa && write_buffer[1] != 0x55) {      // Soft reset
        return ZX_ERR_IO;
      }
    } else if (write_buffer[0] == 0xeb && write_buffer_size == 3) {  // HID to STD
      if (write_buffer[1] != 0xaa || write_buffer[2] != 0x09) {
        return ZX_ERR_IO;
      }
    } else if (write_buffer[0] == 0x55 && write_buffer_size == 1) {  // Unlock boot
    } else if (write_buffer[0] == 0x90 && write_buffer_size == 1) {  // Boot ID
      read_buffer[0] = 0x58;
      read_buffer[1] = 0x2c;
      *read_buffer_size = 2;
    } else if (write_buffer[0] == 0x09 && write_buffer_size == 2) {  // Flash erase
      if (write_buffer[1] != 0x0b) {                                 // Erase app area
        return ZX_ERR_IO;
      }
    } else if (write_buffer[0] == 0xb0 && write_buffer_size == 4) {  // Set erase size
    } else if (write_buffer[0] == 0x61 && write_buffer_size == 1) {  // Start erase
      ecc_ = 0;
      flash_status_ = 0xf0aa;
    } else if (write_buffer[0] == 0x6a && write_buffer_size == 1) {  // Read flash status
      read_buffer[0] = flash_status_ >> 8;
      read_buffer[1] = flash_status_ & 0xff;
      *read_buffer_size = 2;
    } else if (write_buffer[0] == 0xbf && write_buffer_size >= 6) {  // Firmware packet
      const uint32_t address = (write_buffer[1] << 16) | (write_buffer[2] << 8) | write_buffer[3];
      const auto packet_size = static_cast<uint8_t>((write_buffer[4] << 8) | write_buffer[5]);

      if ((packet_size + 6) != write_buffer_size) {
        return ZX_ERR_IO;
      }

      for (uint32_t i = 6; i < write_buffer_size; i++) {
        ecc_ ^= write_buffer[i];
      }

      flash_status_ = (0x1000 + (address / packet_size)) & 0xffff;
      firmware_write_size_ += packet_size;  // Ignore overlapping addresses.
    } else if (write_buffer[0] == 0x64 && write_buffer_size == 1) {  // ECC initialization
    } else if (write_buffer[0] == 0x65 && write_buffer_size == 6) {  // Start ECC calculation
      flash_status_ = 0xf055;                                        // ECC calculation done
    } else if (write_buffer[0] == 0x66 && write_buffer_size == 1) {  // Read calculated ECC
      read_buffer[0] = ecc_;
      *read_buffer_size = 1;
    } else if (write_buffer[0] == 0x07 && write_buffer_size == 1) {  // Reset
    } else if ((write_buffer[0] == FTS_REG_TYPE || write_buffer[0] == FTS_REG_FIRMID ||
                write_buffer[0] == FTS_REG_VENDOR_ID || write_buffer[0] == FTS_REG_PANEL_ID ||
                write_buffer[0] == FTS_REG_RELEASE_ID_HIGH ||
                write_buffer[0] == FTS_REG_RELEASE_ID_LOW ||
                write_buffer[0] == FTS_REG_IC_VERSION) &&
               write_buffer_size == 1) {  // LogRegisterValue
      read_buffer[0] = 0;
      *read_buffer_size = 1;
    } else if (write_buffer_size == 1) {  // Read report
      EXPECT_FALSE(expected_report_.empty());
      EXPECT_EQ(write_buffer[0], expected_report_.front().first);
      memcpy(read_buffer, expected_report_.front().second.data(),
             expected_report_.front().second.size());
      *read_buffer_size = expected_report_.front().second.size();
      expected_report_.pop();
    }

    return ZX_OK;
  }

 private:
  uint16_t flash_status_ = 0;
  uint8_t ecc_ = 0;
  uint32_t firmware_write_size_ = 0;

  std::queue<std::pair<uint8_t, std::vector<uint8_t>>> expected_report_;  // address, data pair
};

struct IncomingNamespace {
  FakeFtDevice i2c_;
  fake_gpio::FakeGpio interrupt_gpio_;
  fake_gpio::FakeGpio reset_gpio_;
  component::OutgoingDirectory i2c_fragment_outgoing_{async_get_default_dispatcher()};
  component::OutgoingDirectory interrupt_gpio_fragment_outgoing_{async_get_default_dispatcher()};
  component::OutgoingDirectory reset_gpio_fragment_outgoing_{async_get_default_dispatcher()};
};

class FocaltechTest : public testing::Test {
 public:
  void SetUp() override {
    ASSERT_EQ(ZX_OK, incoming_loop_.StartThread("incoming-ns-thread"));

    // I2c fragment
    {
      auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
      incoming_.SyncCall([server = std::move(endpoints.server)](IncomingNamespace* infra) mutable {
        zx::result service_result =
            infra->i2c_fragment_outgoing_.AddService<fuchsia_hardware_i2c::Service>(
                fuchsia_hardware_i2c::Service::InstanceHandler(
                    {.device = infra->i2c_.bind_handler(async_get_default_dispatcher())}));
        ZX_ASSERT(service_result.is_ok());
        ZX_ASSERT(infra->i2c_fragment_outgoing_.Serve(std::move(server)).is_ok());
      });
      fake_parent_->AddFidlService(fuchsia_hardware_i2c::Service::Name, std::move(endpoints.client),
                                   "i2c");
    }

    // Reset gpio fragment
    {
      auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
      incoming_.SyncCall([server = std::move(endpoints.server)](IncomingNamespace* infra) mutable {
        auto service_result =
            infra->reset_gpio_fragment_outgoing_.AddService<fuchsia_hardware_gpio::Service>(
                infra->reset_gpio_.CreateInstanceHandler());
        ZX_ASSERT(service_result.is_ok());
        ZX_ASSERT(infra->reset_gpio_fragment_outgoing_.Serve(std::move(server)).is_ok());
      });
      fake_parent_->AddFidlService(fuchsia_hardware_gpio::Service::Name,
                                   std::move(endpoints.client), "gpio-reset");
    }

    // Interrupt gpio fragment
    {
      auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
      incoming_.SyncCall([server = std::move(endpoints.server)](IncomingNamespace* infra) mutable {
        auto service_result =
            infra->interrupt_gpio_fragment_outgoing_.AddService<fuchsia_hardware_gpio::Service>(
                infra->interrupt_gpio_.CreateInstanceHandler());
        ZX_ASSERT(service_result.is_ok());
        ZX_ASSERT(infra->interrupt_gpio_fragment_outgoing_.Serve(std::move(server)).is_ok());
      });
      fake_parent_->AddFidlService(fuchsia_hardware_gpio::Service::Name,
                                   std::move(endpoints.client), "gpio-int");
    }

    zx::interrupt interrupt;
    ASSERT_EQ(ZX_OK, zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &interrupt));
    ASSERT_EQ(ZX_OK, interrupt.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq_));
    incoming_.SyncCall([interrupt = std::move(interrupt)](IncomingNamespace* infra) mutable {
      infra->interrupt_gpio_.SetInterrupt(zx::ok(std::move(interrupt)));
    });
  }

  fidl::ClientEnd<fuchsia_input_report::InputDevice> CreateDut() {
    auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [&]() {
      EXPECT_EQ(ZX_OK, FtDevice::Create(nullptr, fake_parent_.get()));
    });
    EXPECT_EQ(ZX_OK, result.status_value());
    EXPECT_EQ(size_t(1), fake_parent_->child_count());
    child_ = fake_parent_->GetLatestChild();
    dut_ = child_->GetDeviceContext<FtDevice>();
    VerifyGpioInit();

    auto endpoints = fidl::Endpoints<fuchsia_input_report::InputDevice>::Create();
    fidl::BindServer(dispatcher_->async_dispatcher(), std::move(endpoints.server), dut_);
    return std::move(std::move(endpoints.client));
  }

  void TearDown() override {
    auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [&]() {
      device_async_remove(child_);
      mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
      ;
    });
    EXPECT_EQ(ZX_OK, result.status_value());
  }

 private:
  void VerifyGpioInit() {
    incoming_.SyncCall([](IncomingNamespace* infra) mutable {
      std::vector interrupt_states = infra->interrupt_gpio_.GetStateLog();
      ASSERT_GE(interrupt_states.size(), size_t(1));
      ASSERT_EQ(fake_gpio::ReadSubState{}, interrupt_states[0].sub_state);

      std::vector reset_states = infra->reset_gpio_.GetStateLog();
      ASSERT_GE(reset_states.size(), size_t(2));
      ASSERT_EQ(fake_gpio::WriteSubState{.value = 0}, reset_states[0].sub_state);
      ASSERT_EQ(fake_gpio::WriteSubState{.value = 1}, reset_states[1].sub_state);
    });
  }

  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};

 protected:
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  fdf::UnownedSynchronizedDispatcher dispatcher_ =
      fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher();
  zx::interrupt irq_;
  zx_device_t* child_;
  FtDevice* dut_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
};

void VerifyDescriptor(const fuchsia_input_report::wire::DeviceDescriptor& descriptor, int64_t x_max,
                      int64_t y_max) {
  EXPECT_TRUE(descriptor.has_device_information());
  EXPECT_EQ(descriptor.device_information().vendor_id(),
            static_cast<uint32_t>(fuchsia_input_report::wire::VendorId::kGoogle));
  EXPECT_EQ(descriptor.device_information().product_id(),
            static_cast<uint32_t>(
                fuchsia_input_report::wire::VendorGoogleProductId::kFocaltechTouchscreen));

  EXPECT_TRUE(descriptor.has_touch());
  EXPECT_FALSE(descriptor.has_consumer_control());
  EXPECT_FALSE(descriptor.has_keyboard());
  EXPECT_FALSE(descriptor.has_mouse());
  EXPECT_FALSE(descriptor.has_sensor());

  EXPECT_TRUE(descriptor.touch().has_input());
  EXPECT_FALSE(descriptor.touch().has_feature());

  EXPECT_TRUE(descriptor.touch().input().has_touch_type());
  EXPECT_EQ(descriptor.touch().input().touch_type(),
            fuchsia_input_report::wire::TouchType::kTouchscreen);

  EXPECT_TRUE(descriptor.touch().input().has_max_contacts());
  EXPECT_EQ(descriptor.touch().input().max_contacts(), uint32_t(10));

  EXPECT_FALSE(descriptor.touch().input().has_buttons());
  EXPECT_TRUE(descriptor.touch().input().has_contacts());
  EXPECT_EQ(descriptor.touch().input().contacts().count(), size_t(10));

  for (const auto& c : descriptor.touch().input().contacts()) {
    EXPECT_TRUE(c.has_position_x());
    EXPECT_TRUE(c.has_position_y());
    EXPECT_FALSE(c.has_contact_height());
    EXPECT_FALSE(c.has_contact_width());
    EXPECT_FALSE(c.has_pressure());

    EXPECT_EQ(c.position_x().range.min, 0);
    EXPECT_EQ(c.position_x().range.max, x_max);
    EXPECT_EQ(c.position_x().unit.type, fuchsia_input_report::wire::UnitType::kOther);
    EXPECT_EQ(c.position_x().unit.exponent, 0);

    EXPECT_EQ(c.position_y().range.min, 0);
    EXPECT_EQ(c.position_y().range.max, y_max);
    EXPECT_EQ(c.position_y().unit.type, fuchsia_input_report::wire::UnitType::kOther);
    EXPECT_EQ(c.position_y().unit.exponent, 0);
  }
}

TEST_F(FocaltechTest, Metadata3x27) {
  constexpr FocaltechMetadata kFt3x27Metadata = {
      .device_id = FOCALTECH_DEVICE_FT3X27,
      .needs_firmware = false,
  };
  fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kFt3x27Metadata, sizeof(kFt3x27Metadata));

  fidl::WireSyncClient<fuchsia_input_report::InputDevice> client(CreateDut());

  auto result = client->GetDescriptor();
  EXPECT_TRUE(result.ok());
  VerifyDescriptor(result->descriptor, 600, 1024);
}

TEST_F(FocaltechTest, Metadata5726) {
  constexpr FocaltechMetadata kFt5726Metadata = {
      .device_id = FOCALTECH_DEVICE_FT5726,
      .needs_firmware = false,
  };
  fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kFt5726Metadata, sizeof(kFt5726Metadata));

  fidl::WireSyncClient<fuchsia_input_report::InputDevice> client(CreateDut());

  auto result = client->GetDescriptor();
  EXPECT_TRUE(result.ok());
  VerifyDescriptor(result->descriptor, 800, 1280);
}

TEST_F(FocaltechTest, Metadata6336) {
  constexpr FocaltechMetadata kFt6336Metadata = {
      .device_id = FOCALTECH_DEVICE_FT6336,
      .needs_firmware = false,
  };
  fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kFt6336Metadata, sizeof(kFt6336Metadata));

  fidl::WireSyncClient<fuchsia_input_report::InputDevice> client(CreateDut());

  auto result = client->GetDescriptor();
  EXPECT_TRUE(result.ok());
  VerifyDescriptor(result->descriptor, 480, 800);
}

TEST_F(FocaltechTest, Firmware5726) {
  constexpr FocaltechMetadata kFt5726Metadata = {
      .device_id = FOCALTECH_DEVICE_FT5726,
      .needs_firmware = true,
      .display_vendor = 1,
      .ddic_version = 1,
  };
  fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kFt5726Metadata, sizeof(kFt5726Metadata));

  CreateDut();

  incoming_.SyncCall([](IncomingNamespace* infra) mutable {
    EXPECT_EQ(infra->i2c_.firmware_write_size(), sizeof(kFirmware3));
  });
}

TEST_F(FocaltechTest, Firmware5726UpToDate) {
  constexpr FocaltechMetadata kFt5726Metadata = {
      .device_id = FOCALTECH_DEVICE_FT5726,
      .needs_firmware = true,
      .display_vendor = 1,
      .ddic_version = 0,
  };
  fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kFt5726Metadata, sizeof(kFt5726Metadata));

  CreateDut();

  incoming_.SyncCall([](IncomingNamespace* infra) mutable {
    EXPECT_EQ(infra->i2c_.firmware_write_size(), uint32_t(0));
  });
}

TEST_F(FocaltechTest, Touch) {
  constexpr FocaltechMetadata kFt6336Metadata = {
      .device_id = FOCALTECH_DEVICE_FT6336,
      .needs_firmware = false,
  };
  fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kFt6336Metadata, sizeof(kFt6336Metadata));

  fidl::WireSyncClient<fuchsia_input_report::InputDevice> client(CreateDut());

  auto reader_endpoints = fidl::Endpoints<fuchsia_input_report::InputReportsReader>::Create();
  auto result = client->GetInputReportsReader(std::move(reader_endpoints.server));
  ASSERT_EQ(ZX_OK, result.status());
  auto reader = fidl::WireSyncClient<fuchsia_input_report::InputReportsReader>(
      std::move(reader_endpoints.client));

  ASSERT_EQ(ZX_OK, dut_->WaitForNextReader(zx::duration::infinite()));

  // clang-format off
  static const uint8_t expected_report[] = {
      0x02,  // contact_count

      // Contact 0, finger_id = 0
      0x80, 0x01,  // x = 0x001
      0x00, 0x13,  // y = 0x013
      0x00, 0x00,

      // Contact 1, finger_id = 1
      0x80, 0x31,  // x = 0x031
      0x10, 0x00,  // y = 0x000
      0x00, 0x00,

      // Contact 2
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

      // Contact 3
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

      // Contact 4
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

      // Contact 5
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

      // Contact 6
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

      // Contact 7
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

      // Contact 8
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

      // Contact 9
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
  };
  // clang-format on
  incoming_.SyncCall([](IncomingNamespace* infra) mutable {
    for (size_t i = 0; i < sizeof(expected_report); i += 8) {
      infra->i2c_.ExpectReport(
          static_cast<uint8_t>(i + FTS_REG_CURPOINT),
          std::vector<uint8_t>(expected_report + i,
                               expected_report + std::min(i + 8, sizeof(expected_report))));
    }
  });
  irq_.trigger(0, zx::clock::get_boot());

  {
    auto result = reader->ReadInputReports();
    ASSERT_EQ(ZX_OK, result.status());
    ASSERT_FALSE(result.value().is_error());
    auto& reports = result.value().value()->reports;

    ASSERT_EQ(size_t(1), reports.count());
    auto report = reports[0];

    ASSERT_TRUE(report.has_event_time());
    ASSERT_TRUE(report.has_touch());
    auto& touch_report = report.touch();

    ASSERT_TRUE(touch_report.has_contacts());
    ASSERT_EQ(touch_report.contacts().count(), size_t(2));
    EXPECT_EQ(touch_report.contacts()[0].contact_id(), uint32_t(0));
    EXPECT_EQ(touch_report.contacts()[0].position_x(), 0x001);
    EXPECT_EQ(touch_report.contacts()[0].position_y(), 0x013);

    EXPECT_EQ(touch_report.contacts()[1].contact_id(), uint32_t(1));
    EXPECT_EQ(touch_report.contacts()[1].position_x(), 0x031);
    EXPECT_EQ(touch_report.contacts()[1].position_y(), 0x000);
  }
}

}  // namespace ft

int main(int argc, char** argv) {
  testing::InitGoogleTest(&argc, argv);
  return RUN_ALL_TESTS();
}
