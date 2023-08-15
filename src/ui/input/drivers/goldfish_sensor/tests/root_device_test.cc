// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/input/drivers/goldfish_sensor/root_device.h"

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.input.report/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/driver.h>

#include <string>
#include <variant>

#include <gtest/gtest.h>

#include "src/devices/testing/goldfish/fake_pipe/fake_pipe.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/ui/input/drivers/goldfish_sensor/input_device.h"

namespace goldfish::sensor {

namespace {

class FakeInputDevice : public InputDevice {
 public:
  static fpromise::result<InputDevice*, zx_status_t> Create(RootDevice* rootdevice,
                                                            async_dispatcher_t* dispatcher,
                                                            const std::string& name) {
    auto val = new FakeInputDevice(rootdevice, dispatcher, name);
    g_devices_[name] = val;
    return fpromise::ok(val);
  }

  FakeInputDevice(RootDevice* rootdevice, async_dispatcher_t* dispatcher, const std::string& name)
      : InputDevice(
            rootdevice->zxdev(), dispatcher,
            [rootdevice](InputDevice* dev) { rootdevice->input_devices()->RemoveDevice(dev); }),
        name_(name) {}

  ~FakeInputDevice() override { g_devices_.erase(name_); }

  zx_status_t OnReport(const SensorReport& rpt) override {
    std::vector<double> new_report;
    if (rpt.name != name_) {
      return ZX_ERR_INVALID_ARGS;
    }
    for (const auto& val : rpt.data) {
      if (std::holds_alternative<std::string>(val)) {
        return ZX_ERR_INVALID_ARGS;
      }
      new_report.push_back(std::get<Numeric>(val).Double());
    }
    report_ = std::move(new_report);
    report_id_++;
    return ZX_OK;
  }

  void GetInputReportsReader(GetInputReportsReaderRequestView request,
                             GetInputReportsReaderCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void GetDescriptor(GetDescriptorCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  static std::map<std::string, FakeInputDevice*> GetAllDevices() { return g_devices_; }
  static void EraseAllDevices() {
    for (const auto& kv : GetAllDevices()) {
      delete kv.second;
    }
  }

  std::vector<double> report() const { return report_; }
  uint32_t report_id() const { return report_id_.load(); }

 private:
  // For test purposes only.
  static inline std::map<std::string, FakeInputDevice*> g_devices_;

  std::atomic<uint32_t> report_id_ = 0;
  std::vector<double> report_;
  std::string name_;
};

fpromise::result<InputDevice*, zx_status_t> CreateFakeDevice1(RootDevice* rootdevice,
                                                              async_dispatcher_t* dispatcher) {
  return FakeInputDevice::Create(rootdevice, dispatcher, "fake1");
}

fpromise::result<InputDevice*, zx_status_t> CreateFakeDevice2(RootDevice* rootdevice,
                                                              async_dispatcher_t* dispatcher) {
  return FakeInputDevice::Create(rootdevice, dispatcher, "fake2");
}

const std::map<uint64_t, InputDeviceInfo> kFakeDevices = {
    {0x0001, {"fake1", CreateFakeDevice1}},
    {0x0002, {"fake2", CreateFakeDevice2}},
};

class TestRootDevice : public RootDevice {
 public:
  using RootDevice::OnReadSensor;
  using RootDevice::RootDevice;
};

struct IncomingNamespace {
  testing::FakePipe fake_pipe_;
  component::OutgoingDirectory outgoing_{async_get_default_dispatcher()};
};

class RootDeviceTest : public ::testing::Test {
 public:
  void SetUp() override {
    ASSERT_EQ(incoming_loop_.StartThread("incoming-ns-thread"), ZX_OK);

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(endpoints.status_value(), ZX_OK);
    incoming_.SyncCall([server = std::move(endpoints->server)](IncomingNamespace* infra) mutable {
      zx::result service_result =
          infra->outgoing_.AddService<fuchsia_hardware_goldfish_pipe::Service>(
              fuchsia_hardware_goldfish_pipe::Service::InstanceHandler({
                  .device = infra->fake_pipe_.bind_handler(async_get_default_dispatcher()),
              }));
      ASSERT_EQ(service_result.status_value(), ZX_OK);
      ASSERT_EQ(infra->outgoing_.Serve(std::move(server)).status_value(), ZX_OK);
    });

    fake_parent_->AddFidlService(fuchsia_hardware_goldfish_pipe::Service::Name,
                                 std::move(endpoints->client));

    auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [&]() {
      auto device = std::make_unique<TestRootDevice>(fake_parent_.get());
      ASSERT_EQ(device->Bind(), ZX_OK);
      // dut_ will be deleted by MockDevice when test ends.
      dut_ = device.release();
    });
    EXPECT_EQ(result.status_value(), ZX_OK);
    ASSERT_EQ(fake_parent_->child_count(), 1u);
  }

  void TearDown() override {
    FakeInputDevice::EraseAllDevices();

    if (dut_) {
      auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [&]() {
        device_async_remove(dut_->zxdev());
        mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
      });
      EXPECT_EQ(result.status_value(), ZX_OK);
    }
  }

 protected:
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  fdf::UnownedSynchronizedDispatcher dispatcher_ =
      fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher();
  TestRootDevice* dut_;
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
};

TEST_F(RootDeviceTest, SetupDevices) {
  bool list_sensors_called = false;
  incoming_.SyncCall([&list_sensors_called](IncomingNamespace* infra) mutable {
    infra->fake_pipe_.SetOnCmdWriteCallback(
        [pipe = &infra->fake_pipe_, &list_sensors_called](const std::vector<uint8_t>& cmd) {
          const char* kCmdExpected = "000clist-sensors";
          if (memcmp(cmd.data(), kCmdExpected, strlen(kCmdExpected)) == 0) {
            list_sensors_called = true;
            const char* kFrameLength = "0004";
            const char* kFrameContents = "0001";
            pipe->EnqueueBytesToRead(
                std::vector<uint8_t>(kFrameLength, kFrameLength + strlen(kFrameLength)));
            pipe->EnqueueBytesToRead(
                std::vector<uint8_t>(kFrameContents, kFrameContents + strlen(kFrameContents)));
          }
        });
  });

  auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(),
                                         [&]() { ASSERT_EQ(dut_->Setup(kFakeDevices), ZX_OK); });
  EXPECT_EQ(result.status_value(), ZX_OK);
  EXPECT_EQ(FakeInputDevice::GetAllDevices().size(), 1u);
  EXPECT_TRUE(list_sensors_called);

  // Only fake1 is set.
  incoming_.SyncCall([](IncomingNamespace* infra) mutable {
    const char* kSetFake1 = "000bset:fake1:1";
    EXPECT_EQ(
        memcmp(infra->fake_pipe_.io_buffer_contents().back().data(), kSetFake1, strlen(kSetFake1)),
        0);
  });
}

TEST_F(RootDeviceTest, SetupMultipleDevices) {
  bool list_sensors_called = false;
  incoming_.SyncCall([&list_sensors_called](IncomingNamespace* infra) mutable {
    infra->fake_pipe_.SetOnCmdWriteCallback(
        [pipe = &infra->fake_pipe_, &list_sensors_called](const std::vector<uint8_t>& cmd) {
          const char* kCmdExpected = "000clist-sensors";
          if (memcmp(cmd.data(), kCmdExpected, strlen(kCmdExpected)) == 0) {
            list_sensors_called = true;
            const char* kFrameLength = "0004";
            const char* kFrameContents = "0003";
            pipe->EnqueueBytesToRead(
                std::vector<uint8_t>(kFrameLength, kFrameLength + strlen(kFrameLength)));
            pipe->EnqueueBytesToRead(
                std::vector<uint8_t>(kFrameContents, kFrameContents + strlen(kFrameContents)));
          }
        });
  });

  auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(),
                                         [&]() { ASSERT_EQ(dut_->Setup(kFakeDevices), ZX_OK); });
  EXPECT_EQ(result.status_value(), ZX_OK);
  EXPECT_EQ(FakeInputDevice::GetAllDevices().size(), 2u);
  EXPECT_TRUE(list_sensors_called);

  // Both fake1 and fake2 are set.
  incoming_.SyncCall([](IncomingNamespace* infra) mutable {
    const char* kSetFake1 = "000bset:fake1:1";
    const char* kSetFake2 = "000bset:fake2:1";
    EXPECT_EQ(memcmp(infra->fake_pipe_.io_buffer_contents().rbegin()->data(), kSetFake2,
                     strlen(kSetFake2)),
              0);
    EXPECT_EQ(memcmp((++infra->fake_pipe_.io_buffer_contents().rbegin())->data(), kSetFake1,
                     strlen(kSetFake1)),
              0);
  });
}

TEST_F(RootDeviceTest, DispatchSensorReports) {
  // Set list-sensors mask to 0x03, enabling both fake1 and fake2 devices.
  bool list_sensors_called = false;
  incoming_.SyncCall([&list_sensors_called](IncomingNamespace* infra) mutable {
    infra->fake_pipe_.SetOnCmdWriteCallback(
        [pipe = &infra->fake_pipe_, &list_sensors_called](const std::vector<uint8_t>& cmd) {
          const char* kCmdExpected = "000clist-sensors";
          if (memcmp(cmd.data(), kCmdExpected, strlen(kCmdExpected)) == 0) {
            list_sensors_called = true;
            const char* kFrameLength = "0004";
            const char* kFrameContents = "0003";
            pipe->EnqueueBytesToRead(
                std::vector<uint8_t>(kFrameLength, kFrameLength + strlen(kFrameLength)));
            pipe->EnqueueBytesToRead(
                std::vector<uint8_t>(kFrameContents, kFrameContents + strlen(kFrameContents)));
          }
        });
  });

  auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(),
                                         [&]() { ASSERT_EQ(dut_->Setup(kFakeDevices), ZX_OK); });
  EXPECT_EQ(result.status_value(), ZX_OK);
  EXPECT_EQ(FakeInputDevice::GetAllDevices().size(), 2u);
  EXPECT_TRUE(list_sensors_called);

  auto fake1 = FakeInputDevice::GetAllDevices().at("fake1");
  auto fake2 = FakeInputDevice::GetAllDevices().at("fake2");
  auto fake1_report_id = fake1->report_id();
  auto fake2_report_id = fake2->report_id();

  const char* kFake1Report = "fake1:0.1:0.2";
  PipeIo::ReadResult<char> read_result =
      zx::ok(std::string(kFake1Report, kFake1Report + strlen(kFake1Report)));
  dut_->OnReadSensor(std::move(read_result));

  EXPECT_EQ(fake1->report_id(), fake1_report_id + 1);
  EXPECT_EQ(fake2->report_id(), fake2_report_id);
  EXPECT_EQ(fake1->report().size(), 2u);
  EXPECT_EQ(fake1->report()[0], 0.1);
  EXPECT_EQ(fake1->report()[1], 0.2);

  const char* kFake2Report = "fake2:0:0.2:0.3";
  read_result = zx::ok(std::string(kFake2Report, kFake2Report + strlen(kFake2Report)));
  dut_->OnReadSensor(std::move(read_result));

  EXPECT_EQ(fake1->report_id(), fake1_report_id + 1);
  EXPECT_EQ(fake2->report_id(), fake2_report_id + 1);
  EXPECT_EQ(fake2->report().size(), 3u);
  EXPECT_EQ(fake2->report()[0], 0);
  EXPECT_EQ(fake2->report()[1], 0.2);
  EXPECT_EQ(fake2->report()[2], 0.3);

  const char* kFake3Report = "fake3:1:2:3:4";
  read_result = zx::ok(std::string(kFake3Report, kFake3Report + strlen(kFake3Report)));
  dut_->OnReadSensor(std::move(read_result));

  EXPECT_EQ(fake1->report_id(), fake1_report_id + 1);
  EXPECT_EQ(fake2->report_id(), fake2_report_id + 1);
}

}  // namespace

}  // namespace goldfish::sensor
