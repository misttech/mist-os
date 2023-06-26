// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c-child.h"

#include <lib/ddk/metadata.h>

#include <algorithm>
#include <optional>

#include <zxtest/zxtest.h>

#include "i2c.h"
#include "lib/ddk/metadata.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace i2c {

constexpr uint8_t kTestWrite0 = 0x99;
constexpr uint8_t kTestWrite1 = 0x88;
constexpr uint8_t kTestWrite2 = 0x77;
constexpr uint8_t kTestRead0 = 0x12;
constexpr uint8_t kTestRead1 = 0x34;
constexpr uint8_t kTestRead2 = 0x56;

class FakeI2cImpl : public ddk::I2cImplProtocol<FakeI2cImpl> {
 public:
  explicit FakeI2cImpl(std::function<zx_status_t(const i2c_impl_op_t*, size_t)> transact)
      : transact_(std::move(transact)) {}
  FakeI2cImpl()
      : transact_([](const i2c_impl_op_t*, size_t) {
          ADD_FATAL_FAILURE();
          return ZX_ERR_INTERNAL;
        }) {}

  zx_status_t I2cImplGetMaxTransferSize(uint64_t* out_size) {
    *out_size = 1024;
    return ZX_OK;
  }
  zx_status_t I2cImplSetBitrate(uint32_t bitrate) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t I2cImplTransact(const i2c_impl_op_t* op_list, size_t op_count) {
    return transact_(op_list, op_count);
  }

  ddk::I2cImplProtocolClient GetClient() {
    i2c_impl_protocol_t proto{&i2c_impl_protocol_ops_, this};
    return &proto;
  }

 private:
  std::function<zx_status_t(const i2c_impl_op_t*, size_t)> transact_;
};

class I2cChildTest : public zxtest::Test {
 public:
  void TearDown() override {
    // Destruction must happen on the framework dispatcher.
    auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [this]() {
      device_async_remove(fake_root_->GetLatestChild());
      for (auto& child : fake_root_->GetLatestChild()->children()) {
        device_async_remove(child.get());
      }
      EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(fake_root_.get()));
    });
    EXPECT_TRUE(result.is_ok());
  }

 protected:
  fidl::WireSyncClient<fuchsia_hardware_i2c::Device> GetI2cChildClient(
      ddk::I2cImplProtocolClient i2c, const char* name = kDefaultName, const uint32_t bus_id = 0) {
    fidl::WireSyncClient<fuchsia_hardware_i2c::Device> client{};
    GetI2cChildClient(i2c, name, &client, bus_id);
    return client;
  }

 private:
  static constexpr char kDefaultName[] = "";

  void SetMetadata(const char* name, const uint32_t bus_id) {
    fidl::Arena<> arena;

    fidl::VectorView<fuchsia_hardware_i2c_businfo::wire::I2CChannel> channels(arena, 1);
    channels[0] = fuchsia_hardware_i2c_businfo::wire::I2CChannel::Builder(arena)
                      .address(0x10)
                      .name(name)
                      .Build();
    auto metadata = fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata::Builder(arena)
                        .channels(channels)
                        .bus_id(bus_id)
                        .Build();

    auto bytes = fidl::Persist(metadata);
    ASSERT_TRUE(bytes.is_ok());

    fake_root_->SetMetadata(DEVICE_METADATA_I2C_CHANNELS, bytes->data(), bytes->size());
  }

  // Helper function that returns void to allow ASSERT and EXPECT calls.
  void GetI2cChildClient(ddk::I2cImplProtocolClient i2c, const char* name,
                         fidl::WireSyncClient<fuchsia_hardware_i2c::Device>* client,
                         const uint32_t bus_id) {
    i2c_impl_protocol_t proto{};
    i2c.GetProto(&proto);
    fake_root_->AddProtocol(ZX_PROTOCOL_I2C_IMPL, proto.ops, proto.ctx);

    SetMetadata(name, bus_id);

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_i2c::Device>();
    ASSERT_OK(endpoints.status_value());

    *client = fidl::WireSyncClient<fuchsia_hardware_i2c::Device>(std::move(endpoints->client));

    {
      auto result = fdf::RunOnDispatcherSync(dispatcher_->async_dispatcher(), [&]() {
        EXPECT_OK(I2cDevice::Create(nullptr, fake_root_.get()));
      });

      EXPECT_TRUE(result.is_ok());
    }

    ASSERT_EQ(fake_root_->child_count(), 1);
    zx_device_t* i2c_root = fake_root_->GetLatestChild();

    {
      auto result = fdf::RunOnDispatcherSync(
          dispatcher_->async_dispatcher(), [=, server = std::move(endpoints->server)]() mutable {
            auto* const i2c_child = i2c_root->GetLatestChild()->GetDeviceContext<I2cChild>();
            i2c_child->Bind(std::move(server));
          });
      EXPECT_TRUE(result.is_ok());
    }
  }

  // TODO(fxb/124464): Migrate test to use dispatcher integration.
  std::shared_ptr<zx_device> fake_root_{
      MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED()};
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher dispatcher_ = runtime_.StartBackgroundDispatcher();
};

TEST_F(I2cChildTest, Write3BytesOnce) {
  FakeI2cImpl i2c([](const i2c_impl_op_t* op_list, size_t op_count) {
    if (op_count != 1) {
      return ZX_ERR_INTERNAL;
    }
    auto p0 = op_list[0].data_buffer;
    if (p0[0] != kTestWrite0 || p0[1] != kTestWrite1 || p0[2] != kTestWrite2 ||
        op_list[0].data_size != 3 || op_list[0].is_read != false || op_list[0].stop != true) {
      return ZX_ERR_INTERNAL;
    }
    return ZX_OK;
  });

  auto client_wrap = GetI2cChildClient(i2c.GetClient());
  ASSERT_TRUE(client_wrap.is_valid());

  // 3 bytes in 1 write transaction.
  size_t n_write_bytes = 3;
  auto write_buffer = std::make_unique<uint8_t[]>(n_write_bytes);
  write_buffer[0] = kTestWrite0;
  write_buffer[1] = kTestWrite1;
  write_buffer[2] = kTestWrite2;
  auto write_data = fidl::VectorView<uint8_t>::FromExternal(write_buffer.get(), n_write_bytes);

  fidl::Arena arena;
  auto write_transfer = fidl_i2c::wire::DataTransfer::WithWriteData(arena, write_data);

  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 1);
  transactions[0] =
      fidl_i2c::wire::Transaction::Builder(arena).data_transfer(write_transfer).Build();

  auto read = client_wrap->Transfer(transactions);

  ASSERT_OK(read.status());
  ASSERT_FALSE(read->is_error());
}

TEST_F(I2cChildTest, Read3BytesOnce) {
  FakeI2cImpl i2c([](const i2c_impl_op_t* op_list, size_t op_count) {
    if (op_count != 1) {
      return ZX_ERR_INTERNAL;
    }
    if (op_list[0].data_size != 3 || op_list[0].is_read != true || op_list[0].stop != true) {
      return ZX_ERR_INTERNAL;
    }
    op_list->data_buffer[0] = kTestRead0;
    op_list->data_buffer[1] = kTestRead1;
    op_list->data_buffer[2] = kTestRead2;
    return ZX_OK;
  });

  auto client_wrap = GetI2cChildClient(i2c.GetClient());
  ASSERT_TRUE(client_wrap.is_valid());

  // 1 read transaction expecting 3 bytes.
  constexpr size_t n_bytes = 3;

  fidl::Arena arena;
  auto read_transfer = fidl_i2c::wire::DataTransfer::WithReadSize(n_bytes);

  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 1);
  transactions[0] =
      fidl_i2c::wire::Transaction::Builder(arena).data_transfer(read_transfer).Build();

  auto read = client_wrap->Transfer(transactions);
  ASSERT_OK(read.status());
  ASSERT_FALSE(read->is_error());
  ASSERT_EQ(read->value()->read_data.count(), 1);

  auto* read_data = read->value()->read_data[0].data();
  ASSERT_EQ(read_data[0], kTestRead0);
  ASSERT_EQ(read_data[1], kTestRead1);
  ASSERT_EQ(read_data[2], kTestRead2);
}

TEST_F(I2cChildTest, Write1ByteOnceRead1Byte3Times) {
  FakeI2cImpl i2c([](const i2c_impl_op_t* op_list, size_t op_count) {
    if (op_count != 4) {
      return ZX_ERR_INTERNAL;
    }
    auto p0 = op_list[0].data_buffer;
    if (p0[0] != kTestWrite0 || op_list[0].data_size != 1 || op_list[0].is_read != false ||
        op_list[0].stop != false || op_list[1].data_size != 1 || op_list[1].is_read != true ||
        op_list[1].stop != false || op_list[2].data_size != 1 || op_list[2].is_read != true ||
        op_list[2].stop != false || op_list[3].data_size != 1 || op_list[3].is_read != true ||
        op_list[3].stop != true) {
      return ZX_ERR_INTERNAL;
    }
    *op_list[1].data_buffer = kTestRead0;
    *op_list[2].data_buffer = kTestRead1;
    *op_list[3].data_buffer = kTestRead2;
    return ZX_OK;
  });

  auto client_wrap = GetI2cChildClient(i2c.GetClient());
  ASSERT_TRUE(client_wrap.is_valid());

  // 1 byte in 1 write transaction.
  size_t n_write_bytes = 1;
  auto write_buffer = std::make_unique<uint8_t[]>(n_write_bytes);
  write_buffer[0] = kTestWrite0;
  auto write_data = fidl::VectorView<uint8_t>::FromExternal(write_buffer.get(), n_write_bytes);

  fidl::Arena arena;
  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 4);

  transactions[0] =
      fidl_i2c::wire::Transaction::Builder(arena)
          .data_transfer(fidl_i2c::wire::DataTransfer::WithWriteData(arena, write_data))
          .Build();

  // 3 read transaction expecting 1 byte each.
  transactions[1] = fidl_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                        .Build();

  transactions[2] = fidl_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                        .Build();

  transactions[3] = fidl_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                        .Build();

  auto read = client_wrap->Transfer(transactions);
  ASSERT_OK(read.status());
  ASSERT_FALSE(read->is_error());

  auto& read_data = read->value()->read_data;
  ASSERT_EQ(read_data[0].data()[0], kTestRead0);
  ASSERT_EQ(read_data[1].data()[0], kTestRead1);
  ASSERT_EQ(read_data[2].data()[0], kTestRead2);
}

TEST_F(I2cChildTest, StopFlagPropagates) {
  FakeI2cImpl i2c([](const i2c_impl_op_t* op_list, size_t op_count) {
    if (op_count != 4) {
      return ZX_ERR_INTERNAL;
    }
    // Verify that the I2C child driver set the stop flags correctly based on the transaction
    // list passed in below.
    if (!op_list[0].stop || op_list[1].stop || op_list[2].stop || !op_list[3].stop) {
      return ZX_ERR_INTERNAL;
    }
    *op_list[0].data_buffer = kTestRead0;
    *op_list[1].data_buffer = kTestRead1;
    *op_list[2].data_buffer = kTestRead2;
    *op_list[3].data_buffer = kTestRead0;
    return ZX_OK;
  });

  auto client_wrap = GetI2cChildClient(i2c.GetClient());
  ASSERT_TRUE(client_wrap.is_valid());

  fidl::Arena arena;
  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 4);

  // Specified and set to true: the stop flag should be set to true.
  transactions[0] = fidl_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                        .stop(true)
                        .Build();

  // Specified and set to false: the stop flag should be set to false.
  transactions[1] = fidl_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                        .stop(false)
                        .Build();

  // Unspecified: the stop flag should be set to false.
  transactions[2] = fidl_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                        .Build();

  // Final transaction: the stop flag should be set to true.
  transactions[3] = fidl_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                        .stop(false)
                        .Build();

  auto read = client_wrap->Transfer(transactions);
  ASSERT_OK(read.status());
  ASSERT_FALSE(read.value().is_error());
}

TEST_F(I2cChildTest, BadTransfers) {
  FakeI2cImpl i2c;
  auto client_wrap = GetI2cChildClient(i2c.GetClient());
  ASSERT_TRUE(client_wrap.is_valid());

  {
    // There must be at least one Transaction.
    fidl::Arena arena;
    auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 0);

    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_TRUE(read->is_error());
  }

  {
    // Each Transaction must have data_transfer set.
    fidl::Arena arena;
    auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 2);

    transactions[0] = fidl_i2c::wire::Transaction::Builder(arena)
                          .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                          .Build();

    transactions[1] = fidl_i2c::wire::Transaction::Builder(arena).stop(true).Build();

    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_TRUE(read->is_error());
  }

  {
    // Read transfers must be at least one byte.
    fidl::Arena arena;
    auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 2);

    transactions[0] = fidl_i2c::wire::Transaction::Builder(arena)
                          .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                          .Build();

    transactions[1] = fidl_i2c::wire::Transaction::Builder(arena)
                          .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(0))
                          .Build();

    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_TRUE(read->is_error());
  }

  {
    // Each Transaction must have data_transfer set.
    fidl::Arena arena;
    auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 2);

    auto write0 = fidl::VectorView<uint8_t>(arena, 1);
    write0[0] = 0xff;

    auto write1 = fidl::VectorView<uint8_t>(arena, 0);

    transactions[0] = fidl_i2c::wire::Transaction::Builder(arena)
                          .data_transfer(fidl_i2c::wire::DataTransfer::WithWriteData(arena, write0))
                          .Build();

    transactions[1] = fidl_i2c::wire::Transaction::Builder(arena)
                          .data_transfer(fidl_i2c::wire::DataTransfer::WithWriteData(arena, write1))
                          .Build();

    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_TRUE(read->is_error());
  }
}

TEST_F(I2cChildTest, GetNameTest) {
  const std::string kTestName = "foo";

  FakeI2cImpl i2c;
  auto client_wrap = GetI2cChildClient(i2c.GetClient(), kTestName.c_str());
  ASSERT_TRUE(client_wrap.is_valid());

  auto name = client_wrap->GetName();
  ASSERT_OK(name.status());
  ASSERT_FALSE(name->is_error());

  ASSERT_EQ(std::string(name->value()->name.get()), kTestName);
}

TEST_F(I2cChildTest, GetEmptyNameTest) {
  FakeI2cImpl i2c;
  auto client_wrap = GetI2cChildClient(i2c.GetClient(), "");
  ASSERT_TRUE(client_wrap.is_valid());

  auto name = client_wrap->GetName();
  ASSERT_OK(name.status());

  // Empty string here means this endpoint returns an error.
  ASSERT_TRUE(name->is_error());
}

TEST_F(I2cChildTest, HugeTransfer) {
  FakeI2cImpl i2c([](const i2c_impl_op_t* op_list, size_t op_count) {
    for (size_t i = 0; i < op_count; i++) {
      cpp20::span data(op_list[i].data_buffer, op_list[i].data_size);
      if (op_list[i].is_read) {
        if (data.size() != 1024) {
          return ZX_ERR_IO;
        }
        memset(data.data(), 'r', data.size());
      } else if (std::any_of(data.begin(), data.end(), [](uint8_t b) { return b != 'w'; })) {
        return ZX_ERR_IO;
      }
    }
    return ZX_OK;
  });

  auto client_wrap = GetI2cChildClient(i2c.GetClient());
  ASSERT_TRUE(client_wrap.is_valid());

  auto write_buffer = std::make_unique<uint8_t[]>(1024);
  auto write_data = fidl::VectorView<uint8_t>::FromExternal(write_buffer.get(), 1024);
  memset(write_data.data(), 'w', write_data.count());

  fidl::Arena arena;
  auto write_transfer = fidl_i2c::wire::DataTransfer::WithWriteData(arena, write_data);
  auto read_transfer = fidl_i2c::wire::DataTransfer::WithReadSize(1024);

  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 2);
  transactions[0] =
      fidl_i2c::wire::Transaction::Builder(arena).data_transfer(write_transfer).Build();
  transactions[1] =
      fidl_i2c::wire::Transaction::Builder(arena).data_transfer(read_transfer).Build();

  auto read = client_wrap->Transfer(transactions);

  ASSERT_OK(read.status());
  ASSERT_FALSE(read->is_error());

  ASSERT_EQ(read->value()->read_data.count(), 1);
  ASSERT_EQ(read->value()->read_data[0].count(), 1024);
  cpp20::span data(read->value()->read_data[0].data(), read->value()->read_data[0].count());
  EXPECT_TRUE(std::all_of(data.begin(), data.end(), [](uint8_t b) { return b == 'r'; }));
}

}  // namespace i2c
