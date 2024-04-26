// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c-child.h"

#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/result.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>

#include <algorithm>
#include <utility>

#include <zxtest/zxtest.h>

#include "fake-incoming-namespace.h"
#include "i2c.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace i2c {
namespace fi2cimpl = fuchsia_hardware_i2cimpl;

constexpr uint8_t kTestWrite0 = 0x99;
constexpr uint8_t kTestWrite1 = 0x88;
constexpr uint8_t kTestWrite2 = 0x77;
constexpr uint8_t kTestRead0 = 0x12;
constexpr uint8_t kTestRead1 = 0x34;
constexpr uint8_t kTestRead2 = 0x56;

class I2cChildTest : public zxtest::Test {
 protected:
  // Because this test is using a fidl::WireSyncClient, we need to run any ops on the client on
  // their own thread because the testing thread is shared with the fidl::Server<fi2c::Device>. An
  // alternative strategy would be to port this to using an async client instead.
  static void RunSyncClientTask(fit::closure task) {
    async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
    loop.StartThread();
    zx::result result = fdf::RunOnDispatcherSync(loop.dispatcher(), std::move(task));
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  static constexpr char kDefaultName[] = "";

  fidl::WireSyncClient<fuchsia_hardware_i2c::Device> GetI2cChildClient(
      const char* name = kDefaultName, const uint32_t bus_id = 0) {
    fidl::WireSyncClient<fuchsia_hardware_i2c::Device> client{};
    GetI2cChildClient(name, &client, bus_id);
    return client;
  }

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
  void GetI2cChildClient(const char* name,
                         fidl::WireSyncClient<fuchsia_hardware_i2c::Device>* client,
                         const uint32_t bus_id) {
    auto io_eps = fidl::Endpoints<fuchsia_io::Directory>::Create();
    namespace_.SyncCall(&FakeIncomingNamespace::AddI2cImplService, std::move(io_eps.server));
    fake_root_->AddFidlService(fi2cimpl::Service::Name, std::move(io_eps.client));

    SetMetadata(name, bus_id);

    auto i2c_eps = fidl::Endpoints<fuchsia_hardware_i2c::Device>::Create();

    *client = fidl::WireSyncClient<fuchsia_hardware_i2c::Device>(std::move(i2c_eps.client));

    EXPECT_OK(I2cDevice::Create(nullptr, fake_root_.get()));
    ASSERT_EQ(fake_root_->child_count(), 1);

    zx_device_t* i2c_root = fake_root_->GetLatestChild();
    auto* const i2c_child = i2c_root->GetLatestChild()->GetDeviceContext<I2cChild>();
    i2c_child->Bind(std::move(i2c_eps.server));
  }

  async_dispatcher_t* dispatcher() { return dispatcher_->async_dispatcher(); }
  fdf_testing::DriverRuntime& runtime() { return *fdf_testing::DriverRuntime::GetInstance(); }

  std::shared_ptr<zx_device> fake_root_{MockDevice::FakeRootParent()};
  fdf::UnownedSynchronizedDispatcher dispatcher_ = runtime().StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<FakeIncomingNamespace> namespace_{dispatcher(), std::in_place,
                                                                        1024};
};

TEST_F(I2cChildTest, Write3BytesOnce) {
  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fi2cimpl::wire::I2cImplOp>& ops = req->op;

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
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient();
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

  RunSyncClientTask([&]() {
    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_FALSE(read->is_error());
  });
}

TEST_F(I2cChildTest, Read3BytesOnce) {
  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fi2cimpl::wire::I2cImplOp>& ops = req->op;

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

    fidl::VectorView<fi2cimpl::wire::ReadData> read{arena, 1};
    read[0].data = fidl::VectorView<uint8_t>{arena, data};
    comp.buffer(arena).ReplySuccess(read);
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient();
  ASSERT_TRUE(client_wrap.is_valid());

  // 1 read transaction expecting 3 bytes.
  constexpr size_t n_bytes = 3;

  fidl::Arena arena;
  auto read_transfer = fidl_i2c::wire::DataTransfer::WithReadSize(n_bytes);

  auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 1);
  transactions[0] =
      fidl_i2c::wire::Transaction::Builder(arena).data_transfer(read_transfer).Build();

  RunSyncClientTask([&]() {
    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_FALSE(read->is_error());
    ASSERT_EQ(read->value()->read_data.count(), 1);
    ASSERT_EQ(read->value()->read_data[0][0], kTestRead0);
    ASSERT_EQ(read->value()->read_data[0][1], kTestRead1);
    ASSERT_EQ(read->value()->read_data[0][2], kTestRead2);
  });
}

TEST_F(I2cChildTest, Write1ByteOnceRead1Byte3Times) {
  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fi2cimpl::wire::I2cImplOp>& ops = req->op;

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

    fidl::VectorView<fi2cimpl::wire::ReadData> read{arena, 3};
    read[0].data = fidl::VectorView<uint8_t>{arena, data0};
    read[1].data = fidl::VectorView<uint8_t>{arena, data1};
    read[2].data = fidl::VectorView<uint8_t>{arena, data2};

    comp.buffer(arena).ReplySuccess(read);
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient();
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

  RunSyncClientTask([&]() {
    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_FALSE(read->is_error());

    ASSERT_EQ(read->value()->read_data[0][0], kTestRead0);
    ASSERT_EQ(read->value()->read_data[1][0], kTestRead1);
    ASSERT_EQ(read->value()->read_data[2][0], kTestRead2);
  });
}

TEST_F(I2cChildTest, StopFlagPropagates) {
  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fi2cimpl::wire::I2cImplOp>& ops = req->op;

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

    fidl::VectorView<fi2cimpl::wire::ReadData> read{arena, 4};
    read[0].data = fidl::VectorView<uint8_t>{arena, data0};
    read[1].data = fidl::VectorView<uint8_t>{arena, data1};
    read[2].data = fidl::VectorView<uint8_t>{arena, data2};
    read[2].data = fidl::VectorView<uint8_t>{arena, data3};

    comp.buffer(arena).ReplySuccess(read);
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient();
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

  RunSyncClientTask([&]() {
    auto read = client_wrap->Transfer(transactions);
    ASSERT_OK(read.status());
    ASSERT_FALSE(read.value().is_error());
  });
}

TEST_F(I2cChildTest, BadTransfers) {
  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    // Won't be called into, but in case it is, error out.
    comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient();
  ASSERT_TRUE(client_wrap.is_valid());

  {
    // There must be at least one Transaction.
    fidl::Arena arena;
    auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 0);

    RunSyncClientTask([&]() {
      auto read = client_wrap->Transfer(transactions);
      ASSERT_OK(read.status());
      ASSERT_TRUE(read->is_error());
    });
  }

  {
    // Each Transaction must have data_transfer set.
    fidl::Arena arena;
    auto transactions = fidl::VectorView<fidl_i2c::wire::Transaction>(arena, 2);

    transactions[0] = fidl_i2c::wire::Transaction::Builder(arena)
                          .data_transfer(fidl_i2c::wire::DataTransfer::WithReadSize(1))
                          .Build();

    transactions[1] = fidl_i2c::wire::Transaction::Builder(arena).stop(true).Build();

    RunSyncClientTask([&]() {
      auto read = client_wrap->Transfer(transactions);
      ASSERT_OK(read.status());
      ASSERT_TRUE(read->is_error());
    });
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

    RunSyncClientTask([&]() {
      auto read = client_wrap->Transfer(transactions);
      ASSERT_OK(read.status());
      ASSERT_TRUE(read->is_error());
    });
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

    RunSyncClientTask([&]() {
      auto read = client_wrap->Transfer(transactions);
      ASSERT_OK(read.status());
      ASSERT_TRUE(read->is_error());
    });
  }
}

TEST_F(I2cChildTest, GetNameTest) {
  const std::string kTestName = "foo";

  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    // Won't be called into, but in case it is, error out.
    comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient(kTestName.c_str());
  ASSERT_TRUE(client_wrap.is_valid());

  RunSyncClientTask([&]() {
    auto name = client_wrap->GetName();
    ASSERT_OK(name.status());
    ASSERT_FALSE(name->is_error());

    ASSERT_EQ(std::string(name->value()->name.get()), kTestName);
  });
}

TEST_F(I2cChildTest, GetEmptyNameTest) {
  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    // Won't be called into, but in case it is, error out.
    comp.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient();
  ASSERT_TRUE(client_wrap.is_valid());

  RunSyncClientTask([&]() {
    auto name = client_wrap->GetName();
    ASSERT_OK(name.status());

    // Empty string here means this endpoint returns an error.
    ASSERT_TRUE(name->is_error());
  });
}

TEST_F(I2cChildTest, HugeTransfer) {
  auto on_transact = [](FakeI2cImpl::TransactRequestView req, fdf::Arena& arena,
                        FakeI2cImpl::TransactCompleter::Sync& comp) {
    fidl::VectorView<fi2cimpl::wire::I2cImplOp>& ops = req->op;
    constexpr size_t kReadCount = 1024;

    std::vector<fi2cimpl::wire::ReadData> reads;
    for (auto& op : ops) {
      if (op.type.is_read_size() > 0) {
        if (op.type.read_size() != kReadCount) {
          comp.buffer(arena).ReplyError(ZX_ERR_IO);
        }
        fi2cimpl::wire::ReadData read{{arena, kReadCount}};
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
  };
  namespace_.SyncCall(&FakeIncomingNamespace::set_on_transact, std::move(on_transact));

  auto client_wrap = GetI2cChildClient();
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

  RunSyncClientTask([&]() {
    auto read = client_wrap->Transfer(transactions);

    ASSERT_OK(read.status());
    ASSERT_FALSE(read->is_error());

    ASSERT_EQ(read->value()->read_data.count(), 1);
    ASSERT_EQ(read->value()->read_data[0].count(), 1024);
    cpp20::span data(read->value()->read_data[0].data(), read->value()->read_data[0].count());
    EXPECT_TRUE(std::all_of(data.begin(), data.end(), [](uint8_t b) { return b == 'r'; }));
  });
}

}  // namespace i2c
