// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/drivers/misc/goldfish/instance.h"

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/markers.h>
#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fake-bti/bti.h>
#include <lib/fidl-async/cpp/bind.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>

#include <tuple>

#include <zxtest/zxtest.h>

#include "src/devices/lib/acpi/mock/mock-acpi.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/drivers/misc/goldfish/pipe_device.h"

namespace goldfish {

class FakeInstance : public Instance {
 public:
  FakeInstance(PipeDevice* pipe_device, async_dispatcher_t* dispatcher)
      : Instance(pipe_device, dispatcher) {}

  zx_status_t Connect(async_dispatcher_t* dispatcher,
                      fidl::ServerEnd<fuchsia_hardware_goldfish::PipeDevice> server) {
    return fidl::BindSingleInFlightOnly(dispatcher, std::move(server), this);
  }
};

class FakePipeDevice
    : public PipeDevice,
      public fidl::testing::WireTestBase<fuchsia_hardware_goldfish_pipe::GoldfishPipe> {
 public:
  FakePipeDevice(zx_device_t* parent, acpi::Client client, async_dispatcher_t* dispatcher)
      : PipeDevice(parent, std::move(client), dispatcher) {}

  zx_status_t Create(int32_t* out_id, zx::vmo* out_vmo) {
    zx_status_t status = zx::vmo::create(16 * 1024, 0u, out_vmo);
    (*out_id)++;
    return status;
  }

  zx_status_t GetBti(zx::bti* out_bti) { return fake_bti_create(out_bti->reset_and_get_address()); }

  void GetBti(GetBtiCompleter::Sync& completer) override {
    ASSERT_OK(GetBti(&bti_));
    completer.ReplySuccess(std::move(bti_));
  }

  void Create(CreateCompleter::Sync& completer) override {
    ASSERT_OK(Create(&id_, &vmo_));
    completer.ReplySuccess(id_, std::move(vmo_));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  zx::bti bti_;
  zx::vmo vmo_;
  int32_t id_ = 0;
};

// Test suite creating fake Instance on a mock Pipe device.
class InstanceDeviceTest : public zxtest::Test, public loop_fixture::RealLoop {
 public:
  InstanceDeviceTest()
      : zxtest::Test(),
        fake_root_(MockDevice::FakeRootParent()),
        loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        outgoing_(dispatcher()) {}

  // |zxtest::Test|
  void SetUp() override {
    auto acpi_result = mock_acpi_.CreateClient(loop_.dispatcher());
    ASSERT_OK(acpi_result.status_value());

    fake_root_->AddFidlProtocol(
        fidl::DiscoverableProtocolName<fuchsia_hardware_goldfish_pipe::GoldfishPipe>,
        [this](zx::channel channel) {
          fidl::BindServer(
              loop_.dispatcher(),
              fidl::ServerEnd<fuchsia_hardware_goldfish_pipe::GoldfishPipe>(std::move(channel)),
              pipe_device_.get());
          return ZX_OK;
        },
        "goldfish-pipe");

    zx::result service_result = outgoing_.AddService<fuchsia_hardware_sysmem::Service>(
        fuchsia_hardware_sysmem::Service::InstanceHandler({
            .sysmem =
                [](fidl::ServerEnd<fuchsia_hardware_sysmem::Sysmem> request) {
                  // The device connects to the protocol in its constructor but does not
                  // otherwise use it, so we don't need to bind a server here.
                },
        }));
    ASSERT_EQ(service_result.status_value(), ZX_OK);

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints.status_value());
    ASSERT_OK(outgoing_.Serve(std::move(endpoints->server)).status_value());

    fake_root_->AddFidlService(fuchsia_hardware_sysmem::Service::Name, std::move(endpoints->client),
                               "sysmem-fidl");

    pipe_device_ = std::make_unique<FakePipeDevice>(
        fake_root_.get(), std::move(acpi_result.value()), loop_.dispatcher());

    ASSERT_OK(loop_.StartThread("goldfish-pipe-thread"));

    dut_.emplace(pipe_device_.get(), loop_.dispatcher());

    {
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_goldfish::PipeDevice>();
      ASSERT_OK(endpoints.status_value());

      ASSERT_OK(dut_.value().Connect(loop_.dispatcher(), std::move(endpoints->server)));
      fidl_goldfish_client_.Bind(std::move(endpoints->client));
    }
  }

  // |zxtest::Test|
  void TearDown() override {
    loop_.Quit();
    loop_.JoinThreads();
  }

 protected:
  acpi::mock::Device mock_acpi_;

  std::unique_ptr<FakePipeDevice> pipe_device_;
  // Not really optional, but needs to be late-bound.
  std::optional<FakeInstance> dut_;
  std::shared_ptr<MockDevice> fake_root_;

  fidl::WireSyncClient<fuchsia_hardware_goldfish::PipeDevice> fidl_goldfish_client_;
  async::Loop loop_;
  component::OutgoingDirectory outgoing_;

 private:
  zx::bti acpi_bti_;
};

TEST_F(InstanceDeviceTest, OpenPipe) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_goldfish::Pipe>();
  ASSERT_TRUE(endpoints.is_ok());

  ASSERT_TRUE(fidl_goldfish_client_->OpenPipe(std::move(endpoints->server)).ok());
  loop_.RunUntilIdle();
}

TEST_F(InstanceDeviceTest, OpenPipeCloseDutFirst) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_goldfish::Pipe>();
  ASSERT_TRUE(endpoints.is_ok());

  ASSERT_TRUE(fidl_goldfish_client_->OpenPipe(std::move(endpoints->server)).ok());
  loop_.RunUntilIdle();

  endpoints->client.reset();
}

}  // namespace goldfish
