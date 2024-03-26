// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi.h"

#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/wire.h>
#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/spi/spi.h>
#include <zircon/errors.h>

#include <latch>
#include <map>

#include <zxtest/zxtest.h>

#include "sdk/lib/driver/outgoing/cpp/outgoing_directory.h"
#include "spi-child.h"
#include "src/devices/lib/fidl-metadata/spi.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace spi {
using spi_channel_t = fidl_metadata::spi::Channel;

class FakeSpiImplServer : public fdf::WireServer<fuchsia_hardware_spiimpl::SpiImpl> {
 public:
  static constexpr uint8_t kChipSelectCount = 2;

  fidl::ProtocolHandler<fuchsia_hardware_spiimpl::SpiImpl> GetHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                   std::mem_fn(&FakeSpiImplServer::OnClosed));
  }

  // fuchsia_hardware_spiimpl::SpiImpl methods
  void GetChipSelectCount(fdf::Arena& arena,
                          GetChipSelectCountCompleter::Sync& completer) override {
    completer.buffer(arena).Reply(kChipSelectCount);
  }
  void TransmitVector(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVectorRequest* request,
                      fdf::Arena& arena, TransmitVectorCompleter::Sync& completer) override {
    size_t actual;
    auto status = SpiImplExchange(request->chip_select, request->data.data(), request->data.count(),
                                  nullptr, 0, &actual);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    completer.buffer(arena).ReplySuccess();
  }
  void ReceiveVector(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVectorRequest* request,
                     fdf::Arena& arena, ReceiveVectorCompleter::Sync& completer) override {
    std::vector<uint8_t> rxdata(request->size);
    size_t actual;
    auto status =
        SpiImplExchange(request->chip_select, nullptr, 0, rxdata.data(), rxdata.size(), &actual);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    rxdata.resize(actual);
    completer.buffer(arena).ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(rxdata));
  }
  void ExchangeVector(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVectorRequest* request,
                      fdf::Arena& arena, ExchangeVectorCompleter::Sync& completer) override {
    std::vector<uint8_t> rxdata(request->txdata.count());
    size_t actual;
    auto status = SpiImplExchange(request->chip_select, request->txdata.data(),
                                  request->txdata.count(), rxdata.data(), rxdata.size(), &actual);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    rxdata.resize(actual);
    completer.buffer(arena).ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(rxdata));
  }
  void LockBus(fuchsia_hardware_spiimpl::wire::SpiImplLockBusRequest* request, fdf::Arena& arena,
               LockBusCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
  }
  void UnlockBus(fuchsia_hardware_spiimpl::wire::SpiImplUnlockBusRequest* request,
                 fdf::Arena& arena, UnlockBusCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
  }
  void RegisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplRegisterVmoRequest* request,
                   fdf::Arena& arena, RegisterVmoCompleter::Sync& completer) override {
    auto status = SpiImplRegisterVmo(request->chip_select, request->vmo_id,
                                     std::move(request->vmo.vmo), request->vmo.offset,
                                     request->vmo.size, static_cast<uint32_t>(request->rights));
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    completer.buffer(arena).ReplySuccess();
  }
  void UnregisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplUnregisterVmoRequest* request,
                     fdf::Arena& arena, UnregisterVmoCompleter::Sync& completer) override {
    zx::vmo vmo;
    auto status = SpiImplUnregisterVmo(request->chip_select, request->vmo_id, &vmo);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    completer.buffer(arena).ReplySuccess(std::move(vmo));
  }
  void ReleaseRegisteredVmos(
      fuchsia_hardware_spiimpl::wire::SpiImplReleaseRegisteredVmosRequest* request,
      fdf::Arena& arena, ReleaseRegisteredVmosCompleter::Sync& completer) override {
    SpiImplReleaseRegisteredVmos(request->chip_select);
  }
  void TransmitVmo(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVmoRequest* request,
                   fdf::Arena& arena, TransmitVmoCompleter::Sync& completer) override {
    auto status = SpiImplTransmitVmo(request->chip_select, request->buffer.vmo_id,
                                     request->buffer.offset, request->buffer.size);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    completer.buffer(arena).ReplySuccess();
  }
  void ReceiveVmo(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVmoRequest* request,
                  fdf::Arena& arena, ReceiveVmoCompleter::Sync& completer) override {
    auto status = SpiImplReceiveVmo(request->chip_select, request->buffer.vmo_id,
                                    request->buffer.offset, request->buffer.size);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    completer.buffer(arena).ReplySuccess();
  }
  void ExchangeVmo(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVmoRequest* request,
                   fdf::Arena& arena, ExchangeVmoCompleter::Sync& completer) override {
    ASSERT_EQ(request->tx_buffer.size, request->rx_buffer.size);
    auto status = SpiImplExchangeVmo(request->chip_select, request->tx_buffer.vmo_id,
                                     request->tx_buffer.offset, request->rx_buffer.vmo_id,
                                     request->rx_buffer.offset, request->tx_buffer.size);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
    completer.buffer(arena).ReplySuccess();
  }

  uint32_t current_test_cs_ = 0;
  bool corrupt_rx_actual_ = false;
  bool vmos_released_since_last_call_ = false;

  enum class SpiTestMode {
    kTransmit,
    kReceive,
    kExchange,
  } test_mode_;

  std::map<uint32_t, zx::vmo> cs0_vmos;
  std::map<uint32_t, zx::vmo> cs1_vmos;

 private:
  static constexpr uint8_t kTestData[] = {1, 2, 3, 4, 5, 6, 7};

  void OnClosed(fidl::UnbindInfo info) {
    // Called when a connection to this server is closed.
    // This is provided to the binding group during |CreateHandler|.
  }

  zx_status_t SpiImplExchange(uint32_t cs, const uint8_t* txdata, size_t txdata_size,
                              uint8_t* out_rxdata, size_t rxdata_size, size_t* out_rxdata_actual) {
    EXPECT_EQ(cs, current_test_cs_, "");

    switch (test_mode_) {
      case SpiTestMode::kTransmit:
        EXPECT_NE(txdata, nullptr, "");
        EXPECT_NE(txdata_size, 0, "");
        EXPECT_EQ(out_rxdata, nullptr, "");
        EXPECT_EQ(rxdata_size, 0, "");
        *out_rxdata_actual = 0;
        break;
      case SpiTestMode::kReceive:
        EXPECT_EQ(txdata, nullptr, "");
        EXPECT_EQ(txdata_size, 0, "");
        EXPECT_NE(out_rxdata, nullptr, "");
        EXPECT_NE(rxdata_size, 0, "");
        memset(out_rxdata, 0, rxdata_size);
        memcpy(out_rxdata, kTestData, std::min(rxdata_size, sizeof(kTestData)));
        *out_rxdata_actual = rxdata_size + (corrupt_rx_actual_ ? 1 : 0);
        break;
      case SpiTestMode::kExchange:
        EXPECT_NE(txdata, nullptr, "");
        EXPECT_NE(txdata_size, 0, "");
        EXPECT_NE(out_rxdata, nullptr, "");
        EXPECT_NE(rxdata_size, 0, "");
        EXPECT_EQ(txdata_size, rxdata_size, "");
        memset(out_rxdata, 0, rxdata_size);
        memcpy(out_rxdata, txdata, std::min(rxdata_size, txdata_size));
        *out_rxdata_actual = std::min(rxdata_size, txdata_size) + (corrupt_rx_actual_ ? 1 : 0);
        break;
    }

    return ZX_OK;
  }

  zx_status_t SpiImplRegisterVmo(uint32_t chip_select, uint32_t vmo_id, zx::vmo vmo,
                                 uint64_t offset, uint64_t size, uint32_t rights) {
    if (chip_select > 1) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    std::map<uint32_t, zx::vmo>& map = chip_select == 0 ? cs0_vmos : cs1_vmos;
    if (map.find(vmo_id) != map.end()) {
      return ZX_ERR_ALREADY_EXISTS;
    }

    map[vmo_id] = std::move(vmo);
    return ZX_OK;
  }

  zx_status_t SpiImplUnregisterVmo(uint32_t chip_select, uint32_t vmo_id, zx::vmo* out_vmo) {
    if (chip_select > 1) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    std::map<uint32_t, zx::vmo>& map = chip_select == 0 ? cs0_vmos : cs1_vmos;
    auto it = map.find(vmo_id);
    if (it == map.end()) {
      return ZX_ERR_NOT_FOUND;
    }

    if (out_vmo) {
      out_vmo->reset(std::get<1>(*it).release());
    }

    map.erase(it);
    return ZX_OK;
  }

  void SpiImplReleaseRegisteredVmos(uint32_t chip_select) { vmos_released_since_last_call_ = true; }

  zx_status_t SpiImplTransmitVmo(uint32_t chip_select, uint32_t vmo_id, uint64_t offset,
                                 uint64_t size) {
    if (chip_select > 1) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    std::map<uint32_t, zx::vmo>& map = chip_select == 0 ? cs0_vmos : cs1_vmos;
    auto it = map.find(vmo_id);
    if (it == map.end()) {
      return ZX_ERR_NOT_FOUND;
    }

    uint8_t buf[sizeof(kTestData)];
    zx_status_t status = std::get<1>(*it).read(buf, offset, std::max(size, sizeof(buf)));
    if (status != ZX_OK) {
      return status;
    }

    return memcmp(buf, kTestData, std::max(size, sizeof(buf))) == 0 ? ZX_OK : ZX_ERR_IO;
  }

  zx_status_t SpiImplReceiveVmo(uint32_t chip_select, uint32_t vmo_id, uint64_t offset,
                                uint64_t size) {
    if (chip_select > 1) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    std::map<uint32_t, zx::vmo>& map = chip_select == 0 ? cs0_vmos : cs1_vmos;
    auto it = map.find(vmo_id);
    if (it == map.end()) {
      return ZX_ERR_NOT_FOUND;
    }

    return std::get<1>(*it).write(kTestData, offset, std::max(size, sizeof(kTestData)));
  }

  zx_status_t SpiImplExchangeVmo(uint32_t chip_select, uint32_t tx_vmo_id, uint64_t tx_offset,
                                 uint32_t rx_vmo_id, uint64_t rx_offset, uint64_t size) {
    if (chip_select > 1) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    std::map<uint32_t, zx::vmo>& map = chip_select == 0 ? cs0_vmos : cs1_vmos;
    auto tx_it = map.find(tx_vmo_id);
    auto rx_it = map.find(rx_vmo_id);

    if (tx_it == map.end() || rx_it == map.end()) {
      return ZX_ERR_NOT_FOUND;
    }

    uint8_t buf[8];
    zx_status_t status = std::get<1>(*tx_it).read(buf, tx_offset, std::max(size, sizeof(buf)));
    if (status != ZX_OK) {
      return status;
    }

    return std::get<1>(*rx_it).write(buf, rx_offset, std::max(size, sizeof(buf)));
  }

  fdf::ServerBindingGroup<fuchsia_hardware_spiimpl::SpiImpl> bindings_;
};

class SpiDeviceTest : public zxtest::Test {
 protected:
  SpiDeviceTest()
      : runtime_(mock_ddk::GetDriverRuntime()),
        dispatcher_(runtime_->StartBackgroundDispatcher()),
        incoming_dispatcher_(runtime_->StartBackgroundDispatcher()),
        ns_(incoming_dispatcher_->async_dispatcher(), std::in_place) {}

  static constexpr uint32_t kTestBusId = 0;
  static constexpr spi_channel_t kSpiChannels[] = {
      {.cs = 0, .vid = 0, .pid = 0, .did = 0},
      {.cs = 1, .vid = 0, .pid = 0, .did = 0},
  };

  void SetUp() override {
    // TODO(https://fxbug.dev/42075363): Migrate test to use dispatcher integration.
    parent_ = MockDevice::FakeRootParent();

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());

    ns_.SyncCall([&](IncomingNamespace* ns) {
      auto service_result = ns->outgoing.AddService<fuchsia_hardware_spiimpl::Service>(
          fuchsia_hardware_spiimpl::Service::InstanceHandler({
              .device = ns->fake_spi_impl.GetHandler(),
          }));
      ZX_ASSERT(service_result.is_ok());

      ZX_ASSERT(ns->outgoing.Serve(std::move(endpoints->server)).is_ok());
    });

    parent_->AddFidlService(fuchsia_hardware_spiimpl::Service::Name, std::move(endpoints->client));
    SetSpiChannelMetadata(0, kSpiChannels, std::size(kSpiChannels));
    parent_->SetMetadata(DEVICE_METADATA_PRIVATE, &kTestBusId, sizeof(kTestBusId));
  }

  void CreateSpiDevice() {
    std::latch done(1);
    async::PostTask(dispatcher_->async_dispatcher(), [&]() {
      SpiDevice::Create(nullptr, parent_.get());
      done.count_down();
    });
    done.wait();
  }

  void RemoveDevice(MockDevice* device) {
    device_async_remove(device);
    ASSERT_OK(mock_ddk::ReleaseFlaggedDevices(parent_.get(), dispatcher_->async_dispatcher()));
  }

  zx::result<fidl::ClientEnd<fuchsia_hardware_spi::Device>> BindServer(
      const std::shared_ptr<MockDevice>& child) {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    auto path = std::string("svc/") +
                component::MakeServiceMemberPath<fuchsia_hardware_spi::Service::Device>("default");
    return component::ConnectAt<fuchsia_hardware_spi::Device>(child->outgoing(), path);
  }

  void SetSpiChannelMetadata(const uint32_t bus_id, const spi_channel_t* channels, size_t count) {
    const auto result = fidl_metadata::spi::SpiChannelsToFidl(
        bus_id, cpp20::span<const spi_channel_t>(channels, count));
    ASSERT_OK(result.status_value());
    parent_->SetMetadata(DEVICE_METADATA_SPI_CHANNELS, result->data(), result->size());
  }

  std::shared_ptr<fdf_testing::DriverRuntime> runtime_;
  std::shared_ptr<MockDevice> parent_;
  fdf::UnownedSynchronizedDispatcher dispatcher_;

  // Helper Methods that access fake_spi_impl
  void set_current_test_cs(uint32_t i) {
    ns_.SyncCall([i](IncomingNamespace* ns) { ns->fake_spi_impl.current_test_cs_ = i; });
  }
  void set_test_mode(FakeSpiImplServer::SpiTestMode test_mode) {
    ns_.SyncCall([test_mode](IncomingNamespace* ns) { ns->fake_spi_impl.test_mode_ = test_mode; });
  }
  void set_corrupt_rx_actual(bool actual) {
    ns_.SyncCall(
        [actual](IncomingNamespace* ns) { ns->fake_spi_impl.corrupt_rx_actual_ = actual; });
  }
  bool vmos_released_since_last_call() {
    return ns_.SyncCall([](IncomingNamespace* ns) {
      const bool value = ns->fake_spi_impl.vmos_released_since_last_call_;
      ns->fake_spi_impl.vmos_released_since_last_call_ = false;
      return value;
    });
  }

  fdf::UnownedSynchronizedDispatcher incoming_dispatcher_;
  struct IncomingNamespace {
    FakeSpiImplServer fake_spi_impl;
    fdf::OutgoingDirectory outgoing{fdf::Dispatcher::GetCurrent()->get()};
  };
  async_patterns::TestDispatcherBound<IncomingNamespace> ns_;
};

TEST_F(SpiDeviceTest, SpiTest) {
  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == std::size(kSpiChannels); });
  EXPECT_EQ(spi_bus->child_count(), std::size(kSpiChannels));

  // test it
  uint8_t txbuf[] = {0, 1, 2, 3, 4, 5, 6};
  uint8_t rxbuf[sizeof txbuf];

  uint32_t i = 0;
  for (auto it = spi_bus->children().begin(); it != spi_bus->children().end(); it++, i++) {
    set_current_test_cs(i);

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_spi::Device>();
    ASSERT_OK(endpoints);
    auto& [client, server] = endpoints.value();
    fidl::BindServer(dispatcher_->async_dispatcher(), std::move(server),
                     (*it)->GetDeviceContext<SpiChild>());

    set_test_mode(FakeSpiImplServer::SpiTestMode::kTransmit);
    EXPECT_OK(spilib_transmit(client, txbuf, sizeof txbuf));

    set_test_mode(FakeSpiImplServer::SpiTestMode::kReceive);
    EXPECT_OK(spilib_receive(client, rxbuf, sizeof rxbuf));

    set_test_mode(FakeSpiImplServer::SpiTestMode::kExchange);
    EXPECT_OK(spilib_exchange(client, txbuf, rxbuf, sizeof txbuf));
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);
}

TEST_F(SpiDeviceTest, SpiFidlVmoTest) {
  using fuchsia_hardware_sharedmemory::wire::SharedVmoRight;

  constexpr uint8_t kTestData[] = {1, 2, 3, 4, 5, 6, 7};

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == std::size(kSpiChannels); });
  EXPECT_EQ(spi_bus->child_count(), std::size(kSpiChannels));

  fidl::WireSharedClient<fuchsia_hardware_spi::Device> cs0_client, cs1_client;

  {
    const auto& child0 = spi_bus->children().front();
    zx::result client = BindServer(child0);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  {
    const auto& child1 = *++spi_bus->children().begin();
    zx::result client = BindServer(child1);
    ASSERT_OK(client);
    cs1_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  zx::vmo cs0_vmo, cs1_vmo;
  ASSERT_OK(zx::vmo::create(4096, 0, &cs0_vmo));
  ASSERT_OK(zx::vmo::create(4096, 0, &cs1_vmo));

  {
    fuchsia_mem::wire::Range vmo = {.offset = 0, .size = 4096};
    ASSERT_OK(cs0_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo.vmo));
    auto result = cs0_client.sync()->RegisterVmo(1, std::move(vmo),
                                                 SharedVmoRight::kRead | SharedVmoRight::kWrite);
    ASSERT_OK(result.status());
    EXPECT_TRUE(result.value().is_ok());
  }

  {
    fuchsia_mem::wire::Range vmo = {.offset = 0, .size = 4096};
    ASSERT_OK(cs1_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo.vmo));
    auto result = cs1_client.sync()->RegisterVmo(2, std::move(vmo),
                                                 SharedVmoRight::kRead | SharedVmoRight::kWrite);
    ASSERT_OK(result.status());
    EXPECT_TRUE(result.value().is_ok());
  }

  ASSERT_OK(cs0_vmo.write(kTestData, 1024, sizeof(kTestData)));
  {
    auto result = cs0_client.sync()->Exchange(
        {
            .vmo_id = 1,
            .offset = 1024,
            .size = sizeof(kTestData),
        },
        {
            .vmo_id = 1,
            .offset = 2048,
            .size = sizeof(kTestData),
        });
    ASSERT_OK(result.status());
    EXPECT_TRUE(result.value().is_ok());

    uint8_t buf[sizeof(kTestData)];
    ASSERT_OK(cs0_vmo.read(buf, 2048, sizeof(buf)));
    EXPECT_BYTES_EQ(buf, kTestData, sizeof(buf));
  }

  ASSERT_OK(cs1_vmo.write(kTestData, 1024, sizeof(kTestData)));
  {
    auto result = cs1_client.sync()->Transmit({
        .vmo_id = 2,
        .offset = 1024,
        .size = sizeof(kTestData),
    });
    ASSERT_OK(result.status());
    EXPECT_TRUE(result.value().is_ok());
  }

  {
    auto result = cs0_client.sync()->Receive({
        .vmo_id = 1,
        .offset = 1024,
        .size = sizeof(kTestData),
    });
    ASSERT_OK(result.status());
    EXPECT_TRUE(result.value().is_ok());

    uint8_t buf[sizeof(kTestData)];
    ASSERT_OK(cs0_vmo.read(buf, 1024, sizeof(buf)));
    EXPECT_BYTES_EQ(buf, kTestData, sizeof(buf));
  }

  {
    auto result = cs0_client.sync()->UnregisterVmo(1);
    ASSERT_OK(result.status());
    EXPECT_TRUE(result.value().is_ok());
  }

  {
    auto result = cs1_client.sync()->UnregisterVmo(2);
    ASSERT_OK(result.status());
    EXPECT_TRUE(result.value().is_ok());
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);
}

TEST_F(SpiDeviceTest, SpiFidlVectorTest) {
  fidl::WireSharedClient<fuchsia_hardware_spi::Device> cs0_client, cs1_client;

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == std::size(kSpiChannels); });
  EXPECT_EQ(spi_bus->child_count(), std::size(kSpiChannels));

  {
    const auto& child0 = spi_bus->children().front();
    zx::result client = BindServer(child0);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  {
    const auto& child1 = *++spi_bus->children().begin();
    zx::result client = BindServer(child1);
    ASSERT_OK(client);
    cs1_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  uint8_t test_data[] = {1, 2, 3, 4, 5, 6, 7};

  set_current_test_cs(0);
  set_test_mode(FakeSpiImplServer::SpiTestMode::kTransmit);
  {
    auto tx_buffer = fidl::VectorView<uint8_t>::FromExternal(test_data);
    auto result = cs0_client.sync()->TransmitVector(tx_buffer);
    ASSERT_OK(result.status());
    EXPECT_OK(result.value().status);
  }

  set_current_test_cs(1);
  set_test_mode(FakeSpiImplServer::SpiTestMode::kReceive);
  {
    auto result = cs1_client.sync()->ReceiveVector(sizeof(test_data));
    ASSERT_OK(result.status());
    EXPECT_OK(result.value().status);
    ASSERT_EQ(result.value().data.count(), std::size(test_data));
    EXPECT_BYTES_EQ(result.value().data.data(), test_data, sizeof(test_data));
  }

  set_current_test_cs(0);
  set_test_mode(FakeSpiImplServer::SpiTestMode::kExchange);
  {
    auto tx_buffer = fidl::VectorView<uint8_t>::FromExternal(test_data);
    auto result = cs0_client.sync()->ExchangeVector(tx_buffer);
    ASSERT_OK(result.status());
    EXPECT_OK(result.value().status);
    ASSERT_EQ(result.value().rxdata.count(), std::size(test_data));
    EXPECT_BYTES_EQ(result.value().rxdata.data(), test_data, sizeof(test_data));
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);
}

TEST_F(SpiDeviceTest, SpiFidlVectorErrorTest) {
  fidl::WireSharedClient<fuchsia_hardware_spi::Device> cs0_client, cs1_client;

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == std::size(kSpiChannels); });
  EXPECT_EQ(spi_bus->child_count(), std::size(kSpiChannels));

  {
    const auto& child0 = spi_bus->children().front();
    zx::result client = BindServer(child0);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  {
    const auto& child1 = *++spi_bus->children().begin();
    zx::result client = BindServer(child1);
    ASSERT_OK(client);
    cs1_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  set_corrupt_rx_actual(true);

  uint8_t test_data[] = {1, 2, 3, 4, 5, 6, 7};

  set_current_test_cs(0);
  set_test_mode(FakeSpiImplServer::SpiTestMode::kTransmit);
  {
    auto tx_buffer = fidl::VectorView<uint8_t>::FromExternal(test_data);
    auto result = cs0_client.sync()->TransmitVector(tx_buffer);
    ASSERT_OK(result.status());
    EXPECT_OK(result.value().status);
  }

  set_current_test_cs(1);
  set_test_mode(FakeSpiImplServer::SpiTestMode::kReceive);
  {
    auto result = cs1_client.sync()->ReceiveVector(sizeof(test_data));
    ASSERT_OK(result.status());
    EXPECT_EQ(result.value().status, ZX_ERR_INTERNAL);
    EXPECT_EQ(result.value().data.count(), 0);
  }

  set_current_test_cs(0);
  set_test_mode(FakeSpiImplServer::SpiTestMode::kExchange);
  {
    auto tx_buffer = fidl::VectorView<uint8_t>::FromExternal(test_data);
    auto result = cs0_client.sync()->ExchangeVector(tx_buffer);
    ASSERT_OK(result.status());
    EXPECT_EQ(result.value().status, ZX_ERR_INTERNAL);
    EXPECT_EQ(result.value().rxdata.count(), 0);
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);
}

TEST_F(SpiDeviceTest, AssertCsWithSiblingTest) {
  fidl::WireSharedClient<fuchsia_hardware_spi::Device> cs0_client, cs1_client;

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == std::size(kSpiChannels); });
  EXPECT_EQ(spi_bus->child_count(), std::size(kSpiChannels));

  {
    const auto& child0 = spi_bus->children().front();
    zx::result client = BindServer(child0);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  {
    const auto& child1 = *++spi_bus->children().begin();
    zx::result client = BindServer(child1);
    ASSERT_OK(client);
    cs1_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  {
    auto result = cs0_client.sync()->CanAssertCs();
    ASSERT_OK(result.status());
    ASSERT_FALSE(result.value().can);
  }

  {
    auto result = cs1_client.sync()->CanAssertCs();
    ASSERT_OK(result.status());
    ASSERT_FALSE(result.value().can);
  }

  {
    auto result = cs0_client.sync()->AssertCs();
    ASSERT_OK(result.status());
    ASSERT_STATUS(result.value().status, ZX_ERR_NOT_SUPPORTED);
  }

  {
    auto result = cs1_client.sync()->AssertCs();
    ASSERT_OK(result.status());
    ASSERT_STATUS(result.value().status, ZX_ERR_NOT_SUPPORTED);
  }

  {
    auto result = cs0_client.sync()->DeassertCs();
    ASSERT_OK(result.status());
    ASSERT_STATUS(result.value().status, ZX_ERR_NOT_SUPPORTED);
  }

  {
    auto result = cs1_client.sync()->DeassertCs();
    ASSERT_OK(result.status());
    ASSERT_STATUS(result.value().status, ZX_ERR_NOT_SUPPORTED);
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);
}

TEST_F(SpiDeviceTest, AssertCsNoSiblingTest) {
  SetSpiChannelMetadata(0, kSpiChannels, 1);

  fidl::WireSharedClient<fuchsia_hardware_spi::Device> cs0_client;

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == 1; });
  EXPECT_EQ(spi_bus->child_count(), 1);

  {
    const auto& child0 = spi_bus->children().front();
    zx::result client = BindServer(child0);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()), dispatcher_->async_dispatcher());
  }

  {
    auto result = cs0_client.sync()->CanAssertCs();
    ASSERT_OK(result.status());
    ASSERT_TRUE(result.value().can);
  }

  {
    auto result = cs0_client.sync()->AssertCs();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  {
    auto result = cs0_client.sync()->DeassertCs();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);
}

TEST_F(SpiDeviceTest, OneClient) {
  SetSpiChannelMetadata(0, kSpiChannels, 1);

  fidl::WireSyncClient<fuchsia_hardware_spi::Device> cs0_client;

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == 1; });
  EXPECT_EQ(spi_bus->child_count(), 1);

  std::shared_ptr<MockDevice> spi_child = nullptr;
  for (auto& child : spi_bus->children()) {
    if (std::string_view(child->name()) == "spi-0-0") {
      spi_child = child;
      break;
    }
  }
  ASSERT_NOT_NULL(spi_child);

  // Establish a FIDL connection and verify that it works.
  {
    zx::result client = BindServer(spi_child);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()));
  }

  {
    auto result = cs0_client->CanAssertCs();
    ASSERT_OK(result.status());
    ASSERT_TRUE(result.value().can);
  }

  // Trying to make a new connection should fail.
  {
    zx::result client = BindServer(spi_child);
    ASSERT_OK(client);
    fidl::WireSyncClient cs0_client_1(std::move(client.value()));
    EXPECT_STATUS(cs0_client_1->UnregisterVmo(1).status(), ZX_ERR_PEER_CLOSED);
  }

  EXPECT_FALSE(vmos_released_since_last_call());

  // Close the first client so that another one can connect.
  cs0_client = {};

  // We don't know when the driver will be ready for a new client, just loop
  // until the connection is established.
  for (;;) {
    zx::result client = BindServer(spi_child);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()));

    // By the time this connection succeeds, the SPI device has already called to its parent to
    // release registered VMOs. UnregisterVmo will then make a synchronous call to the parent, so
    // waiting for the UnregisterVmo response is sufficient to guarantee that the first call to the
    // parent (the fake spiimpl device) has been processed, and that vmos_released_since_last_call
    // will return the correct value.
    auto result = cs0_client->UnregisterVmo(1);
    if (result.ok()) {
      break;
    }
    EXPECT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
    cs0_client = {};
  }

  EXPECT_TRUE(vmos_released_since_last_call());

  // OpenSession should fail when another client is connected.
  {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_spi::Controller>();
    ASSERT_OK(endpoints);
    auto& [controller, server] = endpoints.value();
    fidl::BindServer(dispatcher_->async_dispatcher(), std::move(server),
                     spi_child->GetDeviceContext<SpiChild>());
    {
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_spi::Device>();
      ASSERT_OK(endpoints);
      auto& [device, server] = endpoints.value();
      ASSERT_OK(fidl::WireCall(controller)->OpenSession(std::move(server)));
      ASSERT_STATUS(fidl::WireCall(device)->CanAssertCs(), ZX_ERR_PEER_CLOSED);
    }
  }

  // Close the first client and make sure OpenSession now works.
  cs0_client = {};

  fidl::ClientEnd<fuchsia_hardware_spi::Device> device;
  while (true) {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_spi::Controller>();
    ASSERT_OK(endpoints);
    auto& [controller, server] = endpoints.value();
    fidl::BindServer(dispatcher_->async_dispatcher(), std::move(server),
                     spi_child->GetDeviceContext<SpiChild>());
    {
      zx::result server = fidl::CreateEndpoints<fuchsia_hardware_spi::Device>(&device);
      ASSERT_OK(server);
      ASSERT_OK(fidl::WireCall(controller)->OpenSession(std::move(server.value())));
      auto result = fidl::WireCall(device)->UnregisterVmo(1);
      if (result.ok()) {
        break;
      }
      ASSERT_STATUS(result, ZX_ERR_PEER_CLOSED);
    }
  }

  EXPECT_TRUE(vmos_released_since_last_call());

  // FIDL clients shouldn't be able to connect, and calling OpenSession a second time should fail.
  {
    zx::result client = BindServer(spi_child);
    ASSERT_OK(client);
    fidl::WireSyncClient cs0_client_1(std::move(client.value()));
    EXPECT_STATUS(cs0_client_1->CanAssertCs().status(), ZX_ERR_PEER_CLOSED);
  }

  {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_spi::Controller>();
    ASSERT_OK(endpoints);
    auto& [controller, server] = endpoints.value();
    fidl::BindServer(dispatcher_->async_dispatcher(), std::move(server),
                     spi_child->GetDeviceContext<SpiChild>());
    {
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_spi::Device>();
      ASSERT_OK(endpoints);
      auto& [device, server] = endpoints.value();
      ASSERT_OK(fidl::WireCall(controller)->OpenSession(std::move(server)));
      ASSERT_STATUS(fidl::WireCall(device)->CanAssertCs(), ZX_ERR_PEER_CLOSED);
    }
  }

  // Close the open session and make sure that a new client can now connect.
  device = {};

  for (;;) {
    zx::result client = BindServer(spi_child);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()));

    auto result = cs0_client->UnregisterVmo(1);
    if (result.ok()) {
      break;
    }

    EXPECT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
    cs0_client = {};
  }

  EXPECT_TRUE(vmos_released_since_last_call());

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);
}

TEST_F(SpiDeviceTest, DdkLifecycle) {
  SetSpiChannelMetadata(0, kSpiChannels, 1);

  fidl::WireSyncClient<fuchsia_hardware_spi::Device> cs0_client;

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == 1; });
  EXPECT_EQ(spi_bus->child_count(), 1);

  {
    const auto& child0 = spi_bus->children().front();
    zx::result client = BindServer(child0);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()));
  }

  {
    auto result = cs0_client->AssertCs();
    ASSERT_OK(result.status());
    EXPECT_OK(result.value().status);
  }

  RemoveDevice(spi_bus->children().front().get());
  EXPECT_EQ(spi_bus->descendant_count(), 0);

  {
    auto result = cs0_client->DeassertCs();
    EXPECT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);

  {
    auto result = cs0_client->DeassertCs();
    // The parent has stopped its loop, this should now fail.
    EXPECT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
  }
}

TEST_F(SpiDeviceTest, DdkLifecycleWithDispatcher) {
  SetSpiChannelMetadata(0, kSpiChannels, 1);

  {
    // Add scheduler role metadata that will cause the core driver to create a new driver
    // dispatcher. Verify that FIDL calls can still be made, and that dispatcher shutdown using the
    // unbind hook works.
    fuchsia_scheduler::RoleName role("no.such.scheduler.role");
    const auto result = fidl::Persist(role);
    ASSERT_TRUE(result.is_ok());
    parent_->SetMetadata(DEVICE_METADATA_SCHEDULER_ROLE_NAME, result->data(), result->size());
  }

  fidl::WireSyncClient<fuchsia_hardware_spi::Device> cs0_client;

  CreateSpiDevice();
  auto* const spi_bus = parent_->GetLatestChild();
  ASSERT_NOT_NULL(spi_bus);
  runtime_->RunUntil([&]() { return spi_bus->child_count() == 1; });
  EXPECT_EQ(spi_bus->child_count(), 1);

  {
    const auto& child0 = spi_bus->children().front();
    zx::result client = BindServer(child0);
    ASSERT_OK(client);
    cs0_client.Bind(std::move(client.value()));
  }

  {
    auto result = cs0_client->AssertCs();
    ASSERT_OK(result.status());
    EXPECT_OK(result.value().status);
  }

  RemoveDevice(spi_bus->children().front().get());
  EXPECT_EQ(spi_bus->descendant_count(), 0);

  {
    auto result = cs0_client->DeassertCs();
    EXPECT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
  }

  RemoveDevice(spi_bus);
  EXPECT_EQ(parent_->descendant_count(), 0);

  {
    auto result = cs0_client->DeassertCs();
    // The parent has stopped its loop, this should now fail.
    EXPECT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
  }
}

}  // namespace spi
