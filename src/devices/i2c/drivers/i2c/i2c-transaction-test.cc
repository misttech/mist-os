// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/devices/i2c/drivers/i2c/i2c-test-env.h"

namespace i2c {

namespace {

constexpr uint32_t kTestAddress = 5;
constexpr uint32_t kTestBusId = 6;
const std::string_view kTestChildName = "i2c-6-5";

constexpr uint8_t kTestWrite0 = 0x99;
constexpr uint8_t kTestWrite1 = 0x88;
constexpr uint8_t kTestWrite2 = 0x77;
constexpr uint8_t kTestRead0 = 0x12;
constexpr uint8_t kTestRead1 = 0x34;
constexpr uint8_t kTestRead2 = 0x56;

}  // namespace

class I2cDriverTransactionTest : public ::testing::Test {
 public:
  void TearDown() override {
    zx::result<> result = test_runner.StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

 protected:
  void Init(FakeI2cImpl::OnTransact on_transact) {
    std::vector<fuchsia_hardware_i2c_businfo::I2CChannel> kChannels = {{{
        .address = kTestAddress,
        .i2c_class = 10,
        .vid = 10,
        .pid = 10,
        .did = 10,
    }}};

    fuchsia_hardware_i2c_businfo::I2CBusMetadata metadata;
    metadata.channels(kChannels);
    metadata.bus_id(kTestBusId);

    test_runner.RunInEnvironmentTypeContext(
        [on_transact = std::move(on_transact), metadata](TestEnvironment& env) {
          env.AddMetadata(metadata);
          env.i2c_impl().set_on_transact(std::move(on_transact));
        });
    EXPECT_TRUE(test_runner.StartDriver().is_ok());

    zx::result result = test_runner.Connect<fuchsia_hardware_i2c::Service::Device>(kTestChildName);
    ASSERT_TRUE(result.is_ok());
    i2c_client.Bind(std::move(result.value()));
    ASSERT_TRUE(i2c_client.is_valid());
  }

  fdf_testing::BackgroundDriverTest<TestConfig> test_runner;
  fidl::WireSyncClient<fuchsia_hardware_i2c::Device> i2c_client;
};

TEST_F(I2cDriverTransactionTest, Write3BytesOnce) {
  fidl::Arena arena;
  Init([](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
          FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::I2cImplOp>& ops = req->op;

    if (ops.count() != 1) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    const auto& op = ops[0];
    if (op.type.is_read_size()) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    auto write_data = op.type.write_data();
    if (write_data.count() != 3 || write_data[0] != kTestWrite0 || write_data[1] != kTestWrite1 ||
        write_data[2] != kTestWrite2) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    // No read data.
    comp.buffer(arena).ReplySuccess({});
  });

  // 3 bytes in 1 write transaction.
  size_t n_write_bytes = 3;
  auto write_buffer = std::make_unique<uint8_t[]>(n_write_bytes);
  write_buffer[0] = kTestWrite0;
  write_buffer[1] = kTestWrite1;
  write_buffer[2] = kTestWrite2;
  auto write_data = fidl::VectorView<uint8_t>::FromExternal(write_buffer.get(), n_write_bytes);

  auto write_transfer = fidl_i2c::wire::DataTransfer::WithWriteData(arena, write_data);

  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 1);
  transactions[0] =
      fidl_i2c::wire::Transaction::Builder(arena).data_transfer(write_transfer).Build();

  auto result = i2c_client->Transfer(transactions);
  ASSERT_OK(result.status());
  ASSERT_FALSE(result->is_error());
}

TEST_F(I2cDriverTransactionTest, Read3BytesOnce) {
  fidl::Arena arena;
  Init([](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
          FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::I2cImplOp>& ops = req->op;

    if (ops.count() != 1) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    const auto& op = ops[0];
    if (op.type.is_write_data() || !op.stop || op.type.read_size() != 3) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    std::vector<uint8_t> data{kTestRead0, kTestRead1, kTestRead2};
    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::ReadData> read{arena, 1};
    read[0].data = fidl::VectorView<uint8_t>{arena, data};
    comp.buffer(arena).ReplySuccess(read);
  });

  // 1 read transaction expecting 3 bytes.
  constexpr size_t n_bytes = 3;

  auto read_transfer = fidl_i2c::wire::DataTransfer::WithReadSize(n_bytes);

  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 1);
  transactions[0] =
      fidl_i2c::wire::Transaction::Builder(arena).data_transfer(read_transfer).Build();

  auto read = i2c_client->Transfer(transactions);
  ASSERT_OK(read.status());
  ASSERT_FALSE(read->is_error());
  ASSERT_EQ(read->value()->read_data.count(), 1u);
  ASSERT_EQ(read->value()->read_data[0][0], kTestRead0);
  ASSERT_EQ(read->value()->read_data[0][1], kTestRead1);
  ASSERT_EQ(read->value()->read_data[0][2], kTestRead2);
}

TEST_F(I2cDriverTransactionTest, Write1ByteOnceRead1Byte3TimesTransactions) {
  Init([](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
          FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::I2cImplOp>& ops = req->op;

    if (ops.count() != 4) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    const auto& op1 = ops[0];
    const auto& op2 = ops[1];
    const auto& op3 = ops[2];
    const auto& op4 = ops[3];

    if (op1.type.is_read_size()) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }
    const auto& op1_write_data = op1.type.write_data();
    if (op1_write_data.count() != 1 || op1_write_data[0] != kTestWrite0) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    if (op2.type.is_write_data() || op2.type.read_size() != 1 || op3.type.is_write_data() ||
        op3.type.read_size() != 1 || op4.type.is_write_data() || op4.type.read_size() != 1) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    std::vector<uint8_t> data0{kTestRead0};
    std::vector<uint8_t> data1{kTestRead1};
    std::vector<uint8_t> data2{kTestRead2};

    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::ReadData> read{arena, 3};
    read[0].data = fidl::VectorView<uint8_t>{arena, data0};
    read[1].data = fidl::VectorView<uint8_t>{arena, data1};
    read[2].data = fidl::VectorView<uint8_t>{arena, data2};

    comp.buffer(arena).ReplySuccess(read);
  });

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

  auto read = i2c_client->Transfer(transactions);
  ASSERT_OK(read.status());
  ASSERT_FALSE(read->is_error());

  ASSERT_EQ(read->value()->read_data[0][0], kTestRead0);
  ASSERT_EQ(read->value()->read_data[1][0], kTestRead1);
  ASSERT_EQ(read->value()->read_data[2][0], kTestRead2);
}

TEST_F(I2cDriverTransactionTest, StopFlagPropagates) {
  fidl::Arena arena;
  Init([](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
          FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::I2cImplOp>& ops = req->op;
    if (ops.count() != 4) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    // Verify that the I2C child driver set the stop flags correctly based on the transaction
    // list passed in below.
    if (!ops[0].stop || ops[1].stop || ops[2].stop || !ops[3].stop) {
      comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
      return;
    }

    std::vector<uint8_t> data0{kTestRead0};
    std::vector<uint8_t> data1{kTestRead1};
    std::vector<uint8_t> data2{kTestRead2};
    std::vector<uint8_t> data3{kTestRead0};

    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::ReadData> read{arena, 4};
    read[0].data = fidl::VectorView<uint8_t>{arena, data0};
    read[1].data = fidl::VectorView<uint8_t>{arena, data1};
    read[2].data = fidl::VectorView<uint8_t>{arena, data2};
    read[2].data = fidl::VectorView<uint8_t>{arena, data3};

    comp.buffer(arena).ReplySuccess(read);
  });

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

  auto read = i2c_client->Transfer(transactions);
  ASSERT_OK(read.status());
  ASSERT_FALSE(read.value().is_error());
}

TEST_F(I2cDriverTransactionTest, BadTransfers) {
  Init([](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
          FakeI2cImpl::TransactCompleter::Sync& comp) {
    // Won't be called into, but in case it is, error out.
    comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  });

  {
    // There must be at least one Transaction.
    fidl::Arena arena;
    auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 0);

    auto read = i2c_client->Transfer(transactions);
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

    auto read = i2c_client->Transfer(transactions);
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

    auto read = i2c_client->Transfer(transactions);
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

    auto read = i2c_client->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_TRUE(read->is_error());
  }
}

TEST_F(I2cDriverTransactionTest, HugeTransfer) {
  Init([](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
          FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::I2cImplOp>& ops = req->op;
    constexpr size_t kReadCount = 1024;

    std::vector<fuchsia_hardware_i2cimpl::wire::ReadData> reads;
    for (auto& op : ops) {
      if (op.type.is_read_size() > 0) {
        if (op.type.read_size() != kReadCount) {
          comp.buffer(arena).ReplyError(ZX_ERR_IO);
        }
        fuchsia_hardware_i2cimpl::wire::ReadData read{{arena, kReadCount}};
        memset(read.data.data(), 'r', kReadCount);
        reads.push_back(read);
      } else {
        auto& write_data = op.type.write_data();
        if (std::any_of(write_data.begin(), write_data.end(), [](uint8_t b) { return b != 'w'; })) {
          comp.buffer(arena).ReplyError(ZX_ERR_IO);
          return;
        }
      }
    }
    comp.buffer(arena).ReplySuccess({arena, reads});
  });

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

  auto read = i2c_client->Transfer(transactions);

  ASSERT_OK(read.status());
  ASSERT_FALSE(read->is_error());

  ASSERT_EQ(read->value()->read_data.count(), 1u);
  ASSERT_EQ(read->value()->read_data[0].count(), 1024u);
  cpp20::span data(read->value()->read_data[0].data(), read->value()->read_data[0].count());
  EXPECT_TRUE(std::all_of(data.begin(), data.end(), [](uint8_t b) { return b == 'r'; }));
}

}  // namespace i2c
