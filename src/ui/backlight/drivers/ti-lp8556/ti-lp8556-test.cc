// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ti-lp8556.h"

#include <fidl/fuchsia.hardware.adhoc.lp8556/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/display-panel.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/fake-platform-device/cpp/fake-pdev.h>
#include <lib/driver/mock-mmio/cpp/region.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/reader.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/mock-i2c/mock-i2c-gtest.h>

#include <cmath>

#include <gmock/gmock.h>
#include <gtest/gtest.h>
#include <mock-mmio-reg/mock-mmio-reg.h>

#include "src/lib/testing/predicates/status.h"

namespace ti {

constexpr uint32_t kMmioRegSize = sizeof(uint32_t);
constexpr uint32_t kMmioRegCount = (kAOBrightnessStickyReg + kMmioRegSize) / kMmioRegSize;

// Need to wrap around TiLp8556 in order to expose the inspector.
class TestTiLp8556 : public TiLp8556 {
 public:
  TestTiLp8556(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : TiLp8556(std::move(start_args), std::move(driver_dispatcher)) {}

  static DriverRegistration GetDriverRegistration() {
    // Use a custom DriverRegistration to create the DUT. Without this, the non-test implementation
    // will be used by default.
    return FUCHSIA_DRIVER_REGISTRATION_V1(fdf_internal::DriverServer<TestTiLp8556>::initialize,
                                          fdf_internal::DriverServer<TestTiLp8556>::destroy);
  }

  inspect::ComponentInspector& inspector() { return TiLp8556::inspector(); }
};

class TiLp8556TestEnvironment : public fdf_testing::Environment {
 public:
  void Init(std::optional<TiLp8556Metadata> metadata, std::optional<display::PanelType> panel_type,
            std::optional<uint32_t> bootloader_panel_id,
            std::optional<fdf::PDev::BoardInfo> board_info) {
    std::map<uint32_t, fdf_fake::Mmio> mmios;
    mmios.insert({0, regs_.GetMmioBuffer()});
    fdf_fake::FakePDev::Config config{.mmios = std::move(mmios)};
    if (board_info.has_value()) {
      config.board_info = board_info.value();
    }
    pdev_.SetConfig(std::move(config));

    if (metadata.has_value()) {
      device_server_.AddMetadata(DEVICE_METADATA_PRIVATE, &metadata.value(),
                                 sizeof(metadata.value()));
    }
    if (bootloader_panel_id.has_value()) {
      device_server_.AddMetadata(DEVICE_METADATA_BOARD_PRIVATE, &bootloader_panel_id.value(),
                                 sizeof(bootloader_panel_id.value()));
    }
    if (panel_type.has_value()) {
      device_server_.AddMetadata(DEVICE_METADATA_DISPLAY_PANEL_TYPE, &panel_type.value(),
                                 sizeof(panel_type.value()));
    }
    device_server_.Initialize("pdev");
  }

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    if (zx_status_t status = device_server_.Serve(dispatcher, &to_driver_vfs); status != ZX_OK) {
      return zx::error(status);
    }

    {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
          pdev_.GetInstanceHandler(dispatcher), "pdev");
      if (result.is_error()) {
        return result.take_error();
      }
    }

    {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_i2c::Service>(
          fuchsia_hardware_i2c::Service::InstanceHandler({
              .device = i2c_bindings_.CreateHandler(&i2c_, dispatcher, fidl::kIgnoreBindingClosure),
          }),
          "i2c");
      if (result.is_error()) {
        return result.take_error();
      }
    }

    return zx::ok();
  }

  mock_i2c::MockI2cGtest& i2c() { return i2c_; }
  mock_mmio::Region& regs() { return regs_; }

 private:
  fdf_fake::FakePDev pdev_;
  mock_mmio::Region regs_{kMmioRegSize, kMmioRegCount};
  mock_i2c::MockI2cGtest i2c_;
  fidl::ServerBindingGroup<fuchsia_hardware_i2c::Device> i2c_bindings_;
  compat::DeviceServer device_server_;
};

class FixtureConfig final {
 public:
  using DriverType = TestTiLp8556;
  using EnvironmentType = TiLp8556TestEnvironment;
};

class TiLp8556Test : public ::testing::Test {
 public:
  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

  void VerifyGetBrightness(bool power, double brightness) {
    bool pwr;
    double brt;
    EXPECT_OK(driver().GetBacklightState(&pwr, &brt));
    EXPECT_EQ(pwr, power);
    EXPECT_EQ(brt, brightness);
  }

  void VerifySetBrightness(bool power, double brightness) {
    if (brightness != driver().GetDeviceBrightness()) {
      uint16_t brightness_reg_value =
          static_cast<uint16_t>(ceil(brightness * kBrightnessRegMaxValue));
      driver_test_.RunInEnvironmentTypeContext([&](auto& env) {
        auto& i2c = env.i2c();
        i2c.ExpectWriteStop({kBacklightBrightnessLsbReg,
                             static_cast<uint8_t>(brightness_reg_value & kBrightnessLsbMask)});
        // An I2C bus read is a write of the address followed by a read of the data.
        i2c.ExpectWrite({kBacklightBrightnessMsbReg}).ExpectReadStop({0});
        i2c.ExpectWriteStop({kBacklightBrightnessMsbReg,
                             static_cast<uint8_t>(((brightness_reg_value & kBrightnessMsbMask) >>
                                                   kBrightnessMsbShift) &
                                                  kBrightnessMsbByteMask)});
      });

      auto sticky_reg = BrightnessStickyReg::Get().FromValue(0);
      sticky_reg.set_brightness(brightness_reg_value & kBrightnessRegMask);
      sticky_reg.set_is_valid(1);

      driver_test_.RunInEnvironmentTypeContext([&](auto& env) {
        env.regs()[BrightnessStickyReg::Get().addr()].ExpectWrite(sticky_reg.reg_value());
      });
    }

    if (power != driver().GetDevicePower()) {
      const uint8_t control_value = kDeviceControlDefaultValue | (power ? kBacklightOn : 0);
      auto cfg2 = driver().GetCfg2();
      driver_test_.RunInEnvironmentTypeContext([&](auto& env) {
        auto& i2c = env.i2c();
        i2c.ExpectWriteStop({kDeviceControlReg, control_value});
        if (power) {
          i2c.ExpectWriteStop({kCfg2Reg, cfg2});
        }
      });
    }
    EXPECT_OK(driver().SetBacklightState(power, brightness));

    driver_test_.RunInEnvironmentTypeContext([&](auto& env) {
      ASSERT_NO_FATAL_FAILURE(env.regs()[BrightnessStickyReg::Get().addr()].VerifyAndClear());
      ASSERT_NO_FATAL_FAILURE(env.i2c().VerifyAndClear());
    });
  }

 protected:
  void StartDriver(std::optional<TiLp8556Metadata> metadata,
                   std::optional<display::PanelType> panel_type,
                   std::optional<uint32_t> bootloader_panel_id,
                   std::optional<fdf::PDev::BoardInfo> board_info) {
    driver_test_.RunInEnvironmentTypeContext(
        [&](auto& env) { env.Init(metadata, panel_type, bootloader_panel_id, board_info); });
    ASSERT_OK(driver_test_.StartDriver());

    zx::result client = driver_test_.ConnectThroughDevfs<fuchsia_hardware_adhoc_lp8556::Device>(
        TiLp8556::kChildNodeName);
    ASSERT_OK(client);
    client_.Bind(std::move(client.value()));
  }

  void FailStartDriver(std::optional<TiLp8556Metadata> metadata) {
    driver_test_.RunInEnvironmentTypeContext(
        [&](auto& env) { env.Init(metadata, std::nullopt, std::nullopt, std::nullopt); });
    ASSERT_NE(driver_test_.StartDriver().status_value(), ZX_OK);
  }

  void WithClient(
      fit::callback<void(fidl::WireSyncClient<fuchsia_hardware_adhoc_lp8556::Device>&)> callback) {
    // Synchronous FIDL requests must be made in a background dispatcher in order to avoid hanging
    // because the driver (i.e. the FIDL server) is running in the foreground.
    ASSERT_OK(driver_test_.RunOnBackgroundDispatcherSync(
        [&client = client_, callback = std::move(callback)]() mutable { callback(client); }));
  }

  TestTiLp8556& driver() { return *driver_test_.driver(); }
  fdf_testing::ForegroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }

 private:
  fidl::WireSyncClient<fuchsia_hardware_adhoc_lp8556::Device> client_;
  fdf_testing::ForegroundDriverTest<FixtureConfig> driver_test_;
};

TEST_F(TiLp8556Test, Brightness) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.i2c()
        .ExpectWrite({kCfg2Reg})
        .ExpectReadStop({kCfg2Default})
        .ExpectWrite({kCurrentLsbReg})
        .ExpectReadStop({0x05, 0x4e})
        .ExpectWrite({kBacklightBrightnessLsbReg})
        .ExpectReadStop({0xab, 0x05})
        .ExpectWrite({kDeviceControlReg})
        .ExpectReadStop({0x85})
        .ExpectWrite({kCfgReg})
        .ExpectReadStop({0x01});
    env.regs()[BrightnessStickyReg::Get().addr()].ExpectRead();
  });
  StartDriver(std::nullopt, std::nullopt, std::nullopt, std::nullopt);

  VerifySetBrightness(false, 0.0);
  VerifyGetBrightness(false, 0.0);

  VerifySetBrightness(true, 0.5);
  VerifyGetBrightness(true, 0.5);

  VerifySetBrightness(true, 1.0);
  VerifyGetBrightness(true, 1.0);

  VerifySetBrightness(true, 0.0);
  VerifyGetBrightness(true, 0.0);
}

TEST_F(TiLp8556Test, InitRegisters) {
  const TiLp8556Metadata kDeviceMetadata = {
      .panel_id = 0,
      .registers =
          {
              // Registers
              0x01, 0x85,  // Device Control
                           // EPROM
              0xa2, 0x30,  // CFG2
              0xa3, 0x32,  // CFG3
              0xa5, 0x54,  // CFG5
              0xa7, 0xf4,  // CFG7
              0xa9, 0x60,  // CFG9
              0xae, 0x09,  // CFGE
          },
      .register_count = 14,
  };
  // constexpr uint8_t kInitialRegisterValues[] = {
  //     0x01, 0x85, 0xa2, 0x30, 0xa3, 0x32, 0xa5, 0x54, 0xa7, 0xf4, 0xa9, 0x60, 0xae, 0x09,
  // };

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.i2c()
        .ExpectWriteStop({0x01, 0x85})
        .ExpectWriteStop({0xa2, 0x30})
        .ExpectWriteStop({0xa3, 0x32})
        .ExpectWriteStop({0xa5, 0x54})
        .ExpectWriteStop({0xa7, 0xf4})
        .ExpectWriteStop({0xa9, 0x60})
        .ExpectWriteStop({0xae, 0x09})
        .ExpectWrite({kCfg2Reg})
        .ExpectReadStop({kCfg2Default})
        .ExpectWrite({kCurrentLsbReg})
        .ExpectReadStop({0x05, 0x4e})
        .ExpectWrite({kBacklightBrightnessLsbReg})
        .ExpectReadStop({0xab, 0x05})
        .ExpectWrite({kDeviceControlReg})
        .ExpectReadStop({0x85})
        .ExpectWrite({kCfgReg})
        .ExpectReadStop({0x01});
  });

  StartDriver(kDeviceMetadata, std::nullopt, std::nullopt, std::nullopt);

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    ASSERT_NO_FATAL_FAILURE(env.regs()[BrightnessStickyReg::Get().addr()].VerifyAndClear());
    ASSERT_NO_FATAL_FAILURE(env.i2c().VerifyAndClear());
  });
}

TEST_F(TiLp8556Test, InitNoRegisters) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.i2c()
        .ExpectWrite({kCfg2Reg})
        .ExpectReadStop({kCfg2Default})
        .ExpectWrite({kCurrentLsbReg})
        .ExpectReadStop({0x05, 0x4e})
        .ExpectWrite({kBacklightBrightnessLsbReg})
        .ExpectReadStop({0xab, 0x05})
        .ExpectWrite({kDeviceControlReg})
        .ExpectReadStop({0x85})
        .ExpectWrite({kCfgReg})
        .ExpectReadStop({0x01});
    env.regs()[BrightnessStickyReg::Get().addr()].ExpectRead();
  });
  StartDriver(std::nullopt, std::nullopt, std::nullopt, std::nullopt);

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    ASSERT_NO_FATAL_FAILURE(env.regs()[BrightnessStickyReg::Get().addr()].VerifyAndClear());
    ASSERT_NO_FATAL_FAILURE(env.i2c().VerifyAndClear());
  });
}

TEST_F(TiLp8556Test, InitInvalidRegisters) {
  TiLp8556Metadata kDeviceMetadata = {
      .panel_id = 0,
      .registers =
          {
              0x01,
              0x85,
              0xa2,
              0x30,
              0xa3,
              0x32,
              0xa5,
              0x54,
              0xa7,
              0xf4,
              0xa9,
              0x60,
              0xae,
          },
      .register_count = 13,
  };

  FailStartDriver(kDeviceMetadata);

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    ASSERT_NO_FATAL_FAILURE(env.regs()[BrightnessStickyReg::Get().addr()].VerifyAndClear());
    ASSERT_NO_FATAL_FAILURE(env.i2c().VerifyAndClear());
  });
}

TEST_F(TiLp8556Test, OverwriteStickyRegister) {
  // constexpr uint8_t kInitialRegisterValues[] = {
  //     kBacklightBrightnessLsbReg,
  //     0xab,
  //     kBacklightBrightnessMsbReg,
  //     0xcd,
  // };

  TiLp8556Metadata kDeviceMetadata = {
      .panel_id = 0,
      .registers =
          {// Registers
           kBacklightBrightnessLsbReg, 0xab, kBacklightBrightnessMsbReg, 0xcd},
      .register_count = 4,
  };

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.i2c()
        .ExpectWriteStop({kBacklightBrightnessLsbReg, 0xab})
        .ExpectWriteStop({kBacklightBrightnessMsbReg, 0xcd})
        .ExpectWrite({kCfg2Reg})
        .ExpectReadStop({kCfg2Default})
        .ExpectWrite({kCurrentLsbReg})
        .ExpectReadStop({0x05, 0x4e})
        .ExpectWrite({kBacklightBrightnessLsbReg})
        .ExpectReadStop({0xab, 0xcd})
        .ExpectWrite({kDeviceControlReg})
        .ExpectReadStop({0x85})
        .ExpectWrite({kCfgReg})
        .ExpectReadStop({0x01});
    env.regs()[BrightnessStickyReg::Get().addr()].ExpectRead();
  });

  StartDriver(kDeviceMetadata, std::nullopt, std::nullopt, std::nullopt);

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    const uint32_t kStickyRegValue =
        BrightnessStickyReg::Get().FromValue(0).set_is_valid(1).set_brightness(0x400).reg_value();
    env.regs()[BrightnessStickyReg::Get().addr()].ExpectWrite(kStickyRegValue);

    // The DUT should set the brightness to 0.25 by writing 0x0400, starting with the LSB. The MSB
    // register needs to be RMW, so check that the upper four bits are preserved (0xab -> 0xa4).
    env.i2c()
        .ExpectWriteStop({kBacklightBrightnessLsbReg, 0x00})
        .ExpectWrite({kBacklightBrightnessMsbReg})
        .ExpectReadStop({0xab})
        .ExpectWriteStop({kBacklightBrightnessMsbReg, 0xa4});
  });

  WithClient([](auto& client) {
    auto result = client->SetStateNormalized({true, 0.25});
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_error());
  });

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    ASSERT_NO_FATAL_FAILURE(env.regs()[BrightnessStickyReg::Get().addr()].VerifyAndClear());
    ASSERT_NO_FATAL_FAILURE(env.i2c().VerifyAndClear());
  });
}

TEST_F(TiLp8556Test, Inspect) {
  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.i2c()
        .ExpectWrite({kCfg2Reg})
        .ExpectReadStop({kCfg2Default})
        .ExpectWrite({kCurrentLsbReg})
        .ExpectReadStop({0x05, 0x4e})
        .ExpectWrite({kBacklightBrightnessLsbReg})
        .ExpectReadStop({0xff, 0x0f})
        .ExpectWrite({kDeviceControlReg})
        .ExpectReadStop({0x85})
        .ExpectWrite({kCfgReg})
        .ExpectReadStop({0x01});
    env.regs()[BrightnessStickyReg::Get().addr()].ExpectRead();
  });
  StartDriver(std::nullopt, std::nullopt, std::nullopt, std::nullopt);

  fpromise::result hierarchy_result =
      inspect::ReadFromVmo(driver().inspector().inspector().DuplicateVmo());
  ASSERT_TRUE(hierarchy_result.is_ok());

  inspect::Hierarchy hierarchy = std::move(hierarchy_result.value());
  const inspect::Hierarchy* root_node = hierarchy.GetByPath({"ti-lp8556"});
  ASSERT_TRUE(root_node);

  EXPECT_THAT(root_node->node(),
              inspect::testing::PropertyList(testing::AllOf(
                  testing::Contains(inspect::testing::DoubleIs("brightness", 1.0)),
                  testing::Contains(inspect::testing::UintIs("scale", 3589u)),
                  testing::Contains(inspect::testing::UintIs("calibrated_scale", 3589u)),
                  testing::Contains(inspect::testing::BoolIs("power", true)))));

  EXPECT_FALSE(root_node->node().get_property<inspect::UintPropertyValue>("persistent_brightness"));
  EXPECT_FALSE(
      root_node->node().get_property<inspect::DoublePropertyValue>("max_absolute_brightness_nits"));
}

TEST_F(TiLp8556Test, GetBackLightPower) {
  TiLp8556Metadata kDeviceMetadata = {
      .panel_id = 2,
      .registers = {},
      .register_count = 0,
  };

  constexpr uint32_t kBootloaderPanelId = 2;  // kBoeFiti9364
  constexpr display::PanelType kPanelType = display::PanelType::kBoeTv070wsmFitipowerJd9364Nelson;

  fdf::PDev::BoardInfo board_info{
      .pid = PDEV_PID_NELSON,
  };

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.i2c()
        .ExpectWrite({kCfg2Reg})
        .ExpectReadStop({kCfg2Default})
        .ExpectWrite({kCurrentLsbReg})
        .ExpectReadStop({0x42, 0x36})
        .ExpectWrite({kBacklightBrightnessLsbReg})
        .ExpectReadStop({0xab, 0x05})
        .ExpectWrite({kDeviceControlReg})
        .ExpectReadStop({0x85})
        .ExpectWrite({kCfgReg})
        .ExpectReadStop({0x36});
    env.regs()[BrightnessStickyReg::Get().addr()].ExpectRead();
  });
  StartDriver(kDeviceMetadata, kPanelType, kBootloaderPanelId, board_info);

  VerifySetBrightness(false, 0.0);
  EXPECT_LT(abs(driver().GetBacklightPower(0) - 0.0141694967), 0.000001f);

  VerifySetBrightness(true, 0.5);
  EXPECT_LT(abs(driver().GetBacklightPower(2048) - 0.5352831254), 0.000001f);

  VerifySetBrightness(true, 1.0);
  EXPECT_LT(abs(driver().GetBacklightPower(4095) - 1.0637770353), 0.000001f);
}

TEST_F(TiLp8556Test, GetPowerWatts) {
  TiLp8556Metadata kDeviceMetadata = {
      .panel_id = 2,
      .registers = {},
      .register_count = 0,
  };

  constexpr uint32_t kBootloaderPanelId = 2;  // kBoeFiti9364
  constexpr display::PanelType kPanelType = display::PanelType::kBoeTv070wsmFitipowerJd9364Nelson;

  ASSERT_NO_FATAL_FAILURE();

  driver_test().RunInEnvironmentTypeContext([](auto& env) {
    env.i2c()
        .ExpectWrite({kCfg2Reg})
        .ExpectReadStop({kCfg2Default})
        .ExpectWrite({kCurrentLsbReg})
        .ExpectReadStop({0x42, 0x36})
        .ExpectWrite({kBacklightBrightnessLsbReg})
        .ExpectReadStop({0xab, 0x05})
        .ExpectWrite({kDeviceControlReg})
        .ExpectReadStop({0x85})
        .ExpectWrite({kCfgReg})
        .ExpectReadStop({0x36});
    env.regs()[BrightnessStickyReg::Get().addr()].ExpectRead();
  });
  StartDriver(kDeviceMetadata, kPanelType, kBootloaderPanelId,
              fdf::PDev::BoardInfo{
                  .pid = PDEV_PID_NELSON,
              });

  VerifySetBrightness(true, 1.0);
  EXPECT_LT(abs(driver().GetBacklightPower(4095) - 1.0637770353), 0.000001f);

  WithClient([](auto& client) {
    auto result = client->GetPowerWatts();
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_error());
  });
}

}  // namespace ti
