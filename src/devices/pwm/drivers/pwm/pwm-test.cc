// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pwm.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace pwm {

namespace {
constexpr size_t kMaxConfigBufferSize = 256;

}  // namespace

struct fake_mode_config {
  uint32_t mode;
};

class FakePwmImpl : public ddk::PwmImplProtocol<FakePwmImpl> {
 public:
  FakePwmImpl()
      : proto_({&pwm_impl_protocol_ops_, this}),
        buffer_(std::make_unique<uint8_t[]>(kMaxConfigBufferSize)) {
    config_.mode_config_buffer = buffer_.get();
    config_.mode_config_size = 0;
  }
  const pwm_impl_protocol_t* proto() const { return &proto_; }

  zx_status_t PwmImplGetConfig(uint32_t idx, pwm_config_t* out_config) {
    get_config_count_++;

    ZX_ASSERT(out_config->mode_config_size >= config_.mode_config_size);

    out_config->polarity = config_.polarity;
    out_config->period_ns = config_.period_ns;
    out_config->duty_cycle = config_.duty_cycle;

    memcpy(out_config->mode_config_buffer, config_.mode_config_buffer, config_.mode_config_size);
    out_config->mode_config_size = config_.mode_config_size;
    return ZX_OK;
  }
  zx_status_t PwmImplSetConfig(uint32_t idx, const pwm_config_t* config) {
    set_config_count_++;

    ZX_ASSERT(config->mode_config_size <= kMaxConfigBufferSize);

    config_.polarity = config->polarity;
    config_.period_ns = config->period_ns;
    config_.duty_cycle = config->duty_cycle;
    memcpy(config_.mode_config_buffer, config->mode_config_buffer, config->mode_config_size);
    config_.mode_config_size = config->mode_config_size;
    return ZX_OK;
  }
  zx_status_t PwmImplEnable(uint32_t idx) {
    enable_count_++;
    return ZX_OK;
  }
  zx_status_t PwmImplDisable(uint32_t idx) {
    disable_count_++;
    return ZX_OK;
  }

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_PWM_IMPL};
    config.callbacks[ZX_PROTOCOL_PWM_IMPL] = banjo_server_.callback();
    return config;
  }

  // Accessors
  unsigned int GetConfigCount() const { return get_config_count_; }
  unsigned int SetConfigCount() const { return set_config_count_; }
  unsigned int EnableCount() const { return enable_count_; }
  unsigned int DisableCount() const { return disable_count_; }

 private:
  unsigned int get_config_count_ = 0;
  unsigned int set_config_count_ = 0;
  unsigned int enable_count_ = 0;
  unsigned int disable_count_ = 0;

  pwm_impl_protocol_t proto_;
  pwm_config_t config_;
  std::unique_ptr<uint8_t[]> buffer_;
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_PWM_IMPL, this, &pwm_impl_protocol_ops_};
};

class PwmTestEnvironment : public fdf_testing::Environment {
 public:
  void Init(const fuchsia_hardware_pwm::PwmChannelsMetadata& metadata) {
    device_server_.Initialize("default", std::nullopt, pwm_impl_.GetBanjoConfig());

    fit::result persisted_metadata = fidl::Persist(metadata);
    ASSERT_TRUE(persisted_metadata.is_ok());
    device_server_.AddMetadata(DEVICE_METADATA_PWM_CHANNELS, persisted_metadata.value().data(),
                               persisted_metadata.value().size());
  }

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    if (zx_status_t status =
            device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs);
        status != ZX_OK) {
      return zx::error(status);
    }

    return zx::ok();
  }

  FakePwmImpl& pwm_impl() { return pwm_impl_; }

 private:
  FakePwmImpl pwm_impl_;
  compat::DeviceServer device_server_;
};

class FixtureConfig final {
 public:
  using DriverType = Pwm;
  using EnvironmentType = PwmTestEnvironment;
};

class PwmTest : public ::testing::Test {
 public:
  void SetUp() override {
    static const fuchsia_hardware_pwm::PwmChannelsMetadata kTestMetadataChannels{
        {.channels{{{{.id = 0}}}}}};

    driver_test_.RunInEnvironmentTypeContext([&](auto& env) { env.Init(kTestMetadataChannels); });
    ASSERT_OK(driver_test_.StartDriver());

    zx::result pwm = driver_test_.Connect<fuchsia_hardware_pwm::Service::Pwm>("pwm-0");
    ASSERT_OK(pwm);
    pwm_.Bind(std::move(pwm.value()));
  }

  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

 protected:
  void WithPwmImpl(fit::callback<void(FakePwmImpl& pwm_impl)> callback) {
    driver_test_.RunInEnvironmentTypeContext(
        [callback = std::move(callback)](auto& env) mutable { callback(env.pwm_impl()); });
  }

  fidl::SyncClient<fuchsia_hardware_pwm::Pwm>& pwm() { return pwm_; }

 private:
  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;
  fidl::SyncClient<fuchsia_hardware_pwm::Pwm> pwm_;
};

TEST_F(PwmTest, GetConfigTest) {
  EXPECT_OK(pwm()->GetConfig());
  EXPECT_OK(pwm()->GetConfig());  // Second time
}

TEST_F(PwmTest, SetConfigTest) {
  fake_mode_config fake_mode{
      .mode = 0,
  };
  const auto* fake_mode_bytes = reinterpret_cast<uint8_t*>(&fake_mode);
  fuchsia_hardware_pwm::PwmConfig fake_config{
      {.polarity = false,
       .period_ns = 1000,
       .duty_cycle = 45.0,
       .mode_config =
           std::vector<uint8_t>{&fake_mode_bytes[0], &fake_mode_bytes[sizeof(fake_mode) - 1]}}};
  EXPECT_OK(pwm()->SetConfig(fake_config));

  fake_mode.mode = 3;
  fake_mode_bytes = reinterpret_cast<uint8_t*>(&fake_mode);
  fake_config.mode_config() =
      std::vector<uint8_t>{&fake_mode_bytes[0], &fake_mode_bytes[sizeof(fake_mode) - 1]};
  fake_config.polarity() = true;
  fake_config.duty_cycle() = 68.0;
  EXPECT_OK(pwm()->SetConfig(fake_config));

  EXPECT_OK(pwm()->SetConfig(fake_config));
}

TEST_F(PwmTest, EnableTest) {
  EXPECT_OK(pwm()->Enable());
  EXPECT_OK(pwm()->Enable());  // Second time
}

TEST_F(PwmTest, DisableTest) {
  EXPECT_OK(pwm()->Disable());
  EXPECT_OK(pwm()->Disable());  // Second time
}

TEST_F(PwmTest, GetConfigFidlTest) {
  // Set a config via the Banjo interface and validate that the same config is
  // returned via the FIDL interface.
  fake_mode_config fake_mode{
      .mode = 0xdeadbeef,
  };
  const auto* fake_mode_bytes = reinterpret_cast<uint8_t*>(&fake_mode);
  fuchsia_hardware_pwm::PwmConfig fake_config{{
      .polarity = false,
      .period_ns = 1000,
      .duty_cycle = 45.0,
      .mode_config =
          std::vector<uint8_t>{&fake_mode_bytes[0], &fake_mode_bytes[sizeof(fake_mode) - 1]},
  }};
  EXPECT_OK(pwm()->SetConfig(fake_config));

  fidl::Result resp = pwm()->GetConfig();

  ASSERT_OK(resp);
  auto& config = resp.value().config();

  WithPwmImpl([](auto& pwm_impl) {
    EXPECT_EQ(pwm_impl.EnableCount(), 0u);
    EXPECT_EQ(pwm_impl.DisableCount(), 0u);
    EXPECT_EQ(pwm_impl.GetConfigCount(), 1u);
    EXPECT_EQ(pwm_impl.SetConfigCount(), 1u);
  });

  EXPECT_EQ(config.polarity(), fake_config.polarity());
  EXPECT_EQ(config.period_ns(), fake_config.period_ns());
  EXPECT_EQ(config.duty_cycle(), fake_config.duty_cycle());
  EXPECT_EQ(config.mode_config(), fake_config.mode_config());
}

TEST_F(PwmTest, SetConfigFidlTest) {
  // Set a config via the FIDL interface and validate that the same config is
  // returned via the Banjo interface.
  fake_mode_config fake_mode{
      .mode = 0xdeadbeef,
  };
  const auto* fake_mode_bytes = reinterpret_cast<uint8_t*>(&fake_mode);
  fuchsia_hardware_pwm::PwmConfig config{{
      .polarity = true,
      .period_ns = 1235,
      .duty_cycle = 45.0,
      .mode_config =
          std::vector<uint8_t>{&fake_mode_bytes[0], &fake_mode_bytes[sizeof(fake_mode) - 1]},
  }};

  EXPECT_OK(pwm()->SetConfig(config));

  fidl::Result fake_config_result = pwm()->GetConfig();
  EXPECT_OK(fake_config_result);
  const auto& fake_config = fake_config_result.value().config();

  WithPwmImpl([](auto& pwm_impl) {
    EXPECT_EQ(pwm_impl.EnableCount(), 0u);
    EXPECT_EQ(pwm_impl.DisableCount(), 0u);
    EXPECT_EQ(pwm_impl.GetConfigCount(), 1u);
    EXPECT_EQ(pwm_impl.SetConfigCount(), 1u);
  });

  EXPECT_EQ(config.polarity(), fake_config.polarity());
  EXPECT_EQ(config.period_ns(), fake_config.period_ns());
  EXPECT_EQ(config.duty_cycle(), fake_config.duty_cycle());
  EXPECT_EQ(config.mode_config(), fake_config.mode_config());
}

TEST_F(PwmTest, EnableFidlTest) {
  ASSERT_OK(pwm()->Enable());

  WithPwmImpl([](auto& pwm_impl) {
    EXPECT_EQ(pwm_impl.EnableCount(), 1u);
    EXPECT_EQ(pwm_impl.DisableCount(), 0u);
    EXPECT_EQ(pwm_impl.GetConfigCount(), 0u);
    EXPECT_EQ(pwm_impl.SetConfigCount(), 0u);
  });
}

TEST_F(PwmTest, DisableFidlTest) {
  ASSERT_OK(pwm()->Disable());

  WithPwmImpl([](auto& pwm_impl) {
    EXPECT_EQ(pwm_impl.EnableCount(), 0u);
    EXPECT_EQ(pwm_impl.DisableCount(), 1u);
    EXPECT_EQ(pwm_impl.GetConfigCount(), 0u);
    EXPECT_EQ(pwm_impl.SetConfigCount(), 0u);
  });
}

}  // namespace pwm
