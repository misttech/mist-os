// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <byteswap.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>
#include <lib/zx/clock.h>

#include <thread>

#include <zxtest/zxtest.h>

#include "../controller.h"
#include "fake-bus.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace ahci {

class PortTest : public zxtest::Test {
 protected:
  void TearDown() override { fake_bus_.reset(); }

  void PortEnable(Bus* bus, Port* port) {
    uint32_t cap;
    EXPECT_OK(bus->RegRead(kHbaCapabilities, &cap));
    const uint32_t max_command_tag = (cap >> 8) & 0x1f;
    EXPECT_OK(port->Configure(0, bus, kHbaPorts, max_command_tag));
    EXPECT_OK(port->Enable());

    // Fake detect of device.
    port->set_device_present(true);

    EXPECT_TRUE(port->device_present());
    EXPECT_TRUE(port->port_implemented());
    EXPECT_TRUE(port->is_valid());
    EXPECT_FALSE(port->paused_cmd_issuing());
  }

  void BusAndPortEnable(Port* port) {
    zx_device_t* fake_parent = nullptr;
    std::unique_ptr<FakeBus> bus(new FakeBus());
    EXPECT_OK(bus->Configure(fake_parent));

    PortEnable(bus.get(), port);

    fake_bus_ = std::move(bus);
  }

  // If non-null, this pointer is owned by Controller::bus_
  std::unique_ptr<FakeBus> fake_bus_;
};

TEST(SataTest, SataStringFixTest) {
  // Nothing to do.
  SataStringFix(nullptr, 0);

  // Zero length, no swapping happens.
  uint16_t a = 0x1234;
  SataStringFix(&a, 0);
  ASSERT_EQ(a, 0x1234, "unexpected string result");

  // One character, only swap to even lengths.
  a = 0x1234;
  SataStringFix(&a, 1);
  ASSERT_EQ(a, 0x1234, "unexpected string result");

  // Swap A.
  a = 0x1234;
  SataStringFix(&a, sizeof(a));
  ASSERT_EQ(a, 0x3412, "unexpected string result");

  // Swap a group of values.
  uint16_t b[] = {0x0102, 0x0304, 0x0506};
  SataStringFix(b, sizeof(b));
  const uint16_t b_rev[] = {0x0201, 0x0403, 0x0605};
  ASSERT_EQ(memcmp(b, b_rev, sizeof(b)), 0, "unexpected string result");

  // Swap a string.
  const char* qemu_model_id = "EQUMH RADDSI K";
  const char* qemu_rev = "QEMU HARDDISK ";
  const size_t qsize = strlen(qemu_model_id);

  union {
    uint16_t word[10];
    char byte[20];
  } str;

  memcpy(str.byte, qemu_model_id, qsize);
  SataStringFix(str.word, qsize);
  ASSERT_EQ(memcmp(str.byte, qemu_rev, qsize), 0, "unexpected string result");

  const char* sin = "abcdefghijklmnoprstu";  // 20 chars
  const size_t slen = strlen(sin);
  ASSERT_EQ(slen, 20, "bad string length");
  ASSERT_EQ(slen & 1, 0, "string length must be even");
  char sout[22];
  memset(sout, 0, sizeof(sout));
  memcpy(sout, sin, slen);

  // Verify swapping the length of every pair from 0 to 20 chars, inclusive.
  for (size_t i = 0; i <= slen; i += 2) {
    memcpy(str.byte, sin, slen);
    SataStringFix(str.word, i);
    ASSERT_EQ(memcmp(str.byte, sout, slen), 0, "unexpected string result");
    ASSERT_EQ(sout[slen], 0, "buffer overrun");
    char c = sout[i];
    sout[i] = sout[i + 1];
    sout[i + 1] = c;
  }
}

TEST(AhciTest, Create) {
  zx_device_t* fake_parent = nullptr;
  std::unique_ptr<FakeBus> bus(new FakeBus());

  zx::result con = Controller::CreateWithBus(fake_parent, std::move(bus));
  ASSERT_TRUE(con.is_ok());
}

TEST(AhciTest, CreateBusConfigFailure) {
  zx_device_t* fake_parent = nullptr;
  std::unique_ptr<FakeBus> bus(new FakeBus());
  bus->DoFailConfigure();

  // Expected to fail during bus configure.
  zx::result con = Controller::CreateWithBus(fake_parent, std::move(bus));
  ASSERT_TRUE(con.is_error());
}

TEST(AhciTest, LaunchIrqAndWorkerThreads) {
  zx_device_t* fake_parent = nullptr;
  std::unique_ptr<FakeBus> bus(new FakeBus());

  zx::result con = Controller::CreateWithBus(fake_parent, std::move(bus));
  ASSERT_TRUE(con.is_ok());

  EXPECT_OK(con->LaunchIrqAndWorkerThreads());
  con->Shutdown();
}

TEST(AhciTest, HbaReset) {
  zx_device_t* fake_parent = nullptr;
  std::unique_ptr<FakeBus> bus(new FakeBus());
  zx::result con = Controller::CreateWithBus(fake_parent, std::move(bus));
  ASSERT_TRUE(con.is_ok());

  // Test reset function.
  EXPECT_OK(con->HbaReset());

  con->Shutdown();
}

TEST_F(PortTest, PortTestEnable) {
  Port port;
  BusAndPortEnable(&port);
}

void cb_status(void* cookie, zx_status_t status, block_op_t* bop) {
  *static_cast<zx_status_t*>(cookie) = status;
}

void cb_assert(void* cookie, zx_status_t status, block_op_t* bop) { EXPECT_TRUE(false); }

TEST_F(PortTest, PortCompleteNone) {
  Port port;
  BusAndPortEnable(&port);

  // Complete with no running transactions.

  EXPECT_FALSE(port.Complete());
}

TEST_F(PortTest, PortCompleteRunning) {
  Port port;
  BusAndPortEnable(&port);

  // Complete with running transaction. No completion should occur, cb_assert should not fire.

  SataTransaction txn = {};
  txn.timeout = zx::clock::get_monotonic() + zx::sec(5);
  txn.completion_cb = cb_assert;

  uint32_t slot = 0;

  // Set txn as running.
  port.TestSetRunning(&txn, slot);
  // Set the running bit in the bus.
  fake_bus_->PortRegOverride(0, kPortSataActive, (1u << slot));

  // Set interrupt for successful transfer completion, but keep the running bit set.
  // Simulates a non-error interrupt that will cause the IRQ handler to examin the running
  // transactions.
  fake_bus_->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  port.HandleIrq();

  EXPECT_TRUE(port.Complete());
}

TEST_F(PortTest, PortCompleteSuccess) {
  Port port;
  BusAndPortEnable(&port);

  // Transaction has successfully completed.

  zx_status_t status = 100;  // Bogus value to be overwritten by callback.

  SataTransaction txn = {};
  txn.timeout = zx::clock::get_monotonic() + zx::sec(5);
  txn.completion_cb = cb_status;
  txn.cookie = &status;

  uint32_t slot = 0;

  // Set txn as running.
  port.TestSetRunning(&txn, slot);
  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(0, kPortSataActive, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  port.HandleIrq();

  // False means no more running commands.
  EXPECT_FALSE(port.Complete());
  // Set by completion callback.
  EXPECT_OK(status);
}

TEST_F(PortTest, PortCompleteTimeout) {
  Port port;
  BusAndPortEnable(&port);

  // Transaction has successfully completed.

  zx_status_t status = ZX_OK;  // Value to be overwritten by callback.

  SataTransaction txn = {};
  // Set timeout in the past.
  txn.timeout = zx::clock::get_monotonic() - zx::sec(1);
  txn.completion_cb = cb_status;
  txn.cookie = &status;

  uint32_t slot = 0;

  // Set txn as running.
  port.TestSetRunning(&txn, slot);
  // Set the running bit in the bus.
  fake_bus_->PortRegOverride(0, kPortSataActive, (1u << slot));

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  port.HandleIrq();

  // False means no more running commands.
  EXPECT_FALSE(port.Complete());
  // Set by completion callback.
  EXPECT_NOT_OK(status);
}

TEST_F(PortTest, ShutdownWaitsForTransactionsInFlight) {
  zx_device_t* fake_parent = nullptr;
  std::unique_ptr<FakeBus> bus(new FakeBus());
  FakeBus* bus_ptr = bus.get();

  zx::result con = Controller::CreateWithBus(fake_parent, std::move(bus));
  ASSERT_TRUE(con.is_ok());

  Port& port = *con->port(0);
  PortEnable(bus_ptr, &port);

  // Set up a transaction that will timeout in 5 seconds.

  zx_status_t status = ZX_OK;  // Value to be overwritten by callback.

  SataTransaction txn = {};
  txn.timeout = zx::clock::get_monotonic() + zx::sec(5);
  txn.completion_cb = cb_status;
  txn.cookie = &status;

  uint32_t slot = 0;

  // Set txn as running.
  port.TestSetRunning(&txn, slot);
  // Set the running bit in the bus.
  bus_ptr->PortRegOverride(0, kPortSataActive, (1u << slot));

  // Set interrupt for successful transfer completion.
  bus_ptr->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Kick off interrupt-handler and worker threads.
  EXPECT_OK(con->LaunchIrqAndWorkerThreads());

  // True means there are running command(s).
  EXPECT_TRUE(port.Complete());

  bool shutdown_complete = false;
  zx::duration shutdown_duration;

  std::thread shutdown_thread([&]() {
    zx::time time = zx::clock::get_monotonic();
    con->Shutdown();
    shutdown_duration = zx::clock::get_monotonic() - time;
    shutdown_complete = true;
  });

  // TODO(fxbug.dev/109707): This should be handled by a watchdog in the driver.
  std::thread watchdog_thread([&]() {
    while (!shutdown_complete) {
      con->SignalWorker();
    }
  });

  shutdown_thread.join();
  watchdog_thread.join();
  // The shutdown duration should be around 5 seconds (+/-). Conservatively check for > 2.5 seconds.
  EXPECT_GT(shutdown_duration, zx::msec(2500));

  // Set by completion callback.
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);
}

TEST_F(PortTest, FlushWhenCommandQueueEmpty) {
  Port port;
  BusAndPortEnable(&port);

  SataDeviceInfo di;
  di.block_size = 512;
  di.max_cmd = 31;
  port.SetDevInfo(&di);

  zx_status_t status = ZX_ERR_IO;  // Value to be overwritten by callback.

  SataTransaction txn = {};
  txn.bop.command.opcode = BLOCK_OPCODE_FLUSH;
  txn.completion_cb = cb_status;
  txn.cookie = &status;
  txn.cmd = SATA_CMD_FLUSH_EXT;

  // Queue txn.
  port.Queue(&txn);  // Sets txn.timeout.

  // Process txn while the port has paused command issuing.
  EXPECT_TRUE(port.ProcessQueued());
  EXPECT_TRUE(port.paused_cmd_issuing());

  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(0, kPortSataActive, 0);
  fake_bus_->PortRegOverride(0, kPortCommandIssue, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  port.HandleIrq();

  // There are no more running commands (txn complete), and the port has unpaused.
  EXPECT_FALSE(port.Complete());
  EXPECT_FALSE(port.paused_cmd_issuing());
  EXPECT_EQ(status, ZX_OK);

  // There are no more commands to process.
  EXPECT_FALSE(port.ProcessQueued());
  EXPECT_FALSE(port.paused_cmd_issuing());
}

TEST_F(PortTest, FlushWhenWritePrecedingAndReadFollowing) {
  Port port;
  BusAndPortEnable(&port);

  SataDeviceInfo di;
  di.block_size = 512;
  di.max_cmd = 31;
  port.SetDevInfo(&di);

  zx_status_t write_status = ZX_ERR_IO;  // Value to be overwritten by callback.

  SataTransaction write_txn = {};
  write_txn.bop.command.opcode = BLOCK_OPCODE_WRITE;
  write_txn.completion_cb = cb_status;
  write_txn.cookie = &write_status;
  write_txn.cmd = SATA_CMD_WRITE_FPDMA_QUEUED;

  // Queue write_txn.
  port.Queue(&write_txn);

  zx_status_t flush_status = ZX_ERR_IO;  // Value to be overwritten by callback.

  SataTransaction flush_txn = {};
  flush_txn.bop.command.opcode = BLOCK_OPCODE_FLUSH;
  flush_txn.completion_cb = cb_status;
  flush_txn.cookie = &flush_status;
  flush_txn.cmd = SATA_CMD_FLUSH_EXT;

  // Queue flush_txn.
  port.Queue(&flush_txn);

  zx_status_t read_status = ZX_ERR_IO;  // Value to be overwritten by callback.

  SataTransaction read_txn = {};
  read_txn.bop.command.opcode = BLOCK_OPCODE_READ;
  read_txn.completion_cb = cb_status;
  read_txn.cookie = &read_status;
  read_txn.cmd = SATA_CMD_READ_FPDMA_QUEUED;

  // Queue read_txn.
  port.Queue(&read_txn);

  // Process write_txn while the port has paused command issuing.
  EXPECT_TRUE(port.ProcessQueued());
  EXPECT_TRUE(port.paused_cmd_issuing());

  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(0, kPortSataActive, 0);
  fake_bus_->PortRegOverride(0, kPortCommandIssue, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  port.HandleIrq();

  // There are no more running commands (write_txn complete), and the port has unpaused.
  EXPECT_FALSE(port.Complete());
  EXPECT_FALSE(port.paused_cmd_issuing());
  EXPECT_EQ(write_status, ZX_OK);

  // Process flush_txn while the port has paused command issuing.
  EXPECT_TRUE(port.ProcessQueued());
  EXPECT_TRUE(port.paused_cmd_issuing());

  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(0, kPortSataActive, 0);
  fake_bus_->PortRegOverride(0, kPortCommandIssue, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  port.HandleIrq();

  // There are no more running commands (flush_txn complete), and the port has unpaused.
  EXPECT_FALSE(port.Complete());
  EXPECT_FALSE(port.paused_cmd_issuing());
  EXPECT_EQ(flush_status, ZX_OK);

  // Process read_txn. The port remains unpaused.
  EXPECT_TRUE(port.ProcessQueued());
  EXPECT_FALSE(port.paused_cmd_issuing());

  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(0, kPortSataActive, 0);
  fake_bus_->PortRegOverride(0, kPortCommandIssue, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(0, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  port.HandleIrq();

  // There are no more running commands (read_txn complete).
  EXPECT_FALSE(port.Complete());
  EXPECT_FALSE(port.paused_cmd_issuing());
  EXPECT_EQ(read_status, ZX_OK);

  // There are no more commands to process.
  EXPECT_FALSE(port.ProcessQueued());
  EXPECT_FALSE(port.paused_cmd_issuing());
}

class AhciTestFixture : public inspect::InspectTestHelper, public zxtest::TestWithParam<bool> {
 public:
  static constexpr uint32_t kTestLogicalBlockCount = 1024;

  void SetUp() override {
    support_native_command_queuing_ = GetParam();
    fake_root_ = MockDevice::FakeRootParent();

    // Create a fake bus.
    auto bus = std::make_unique<FakeBus>(support_native_command_queuing_);
    fake_bus_ = bus.get();

    // Set up the driver.
    zx::result controller = Controller::CreateWithBus(fake_root_.get(), std::move(bus));
    ASSERT_TRUE(controller.is_ok());
    ASSERT_OK(controller->AddDevice());
    [[maybe_unused]] auto unused = controller.value().release();

    device_ = fake_root_->GetLatestChild();
    controller_ = device_->GetDeviceContext<Controller>();
  }

  void RunInit() {
    device_->InitOp();
    ASSERT_OK(device_->WaitUntilInitReplyCalled(zx::time::infinite()));
    ASSERT_OK(device_->InitReplyCallStatus());
  }

  void TearDown() override {
    device_async_remove(device_);
    EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(device_));
  }

 protected:
  bool support_native_command_queuing_;
  std::shared_ptr<zx_device> fake_root_;
  FakeBus* fake_bus_;
  zx_device* device_;
  Controller* controller_;
};

TEST_P(AhciTestFixture, SataDeviceInitAndRead) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_NE(device_->child_count(), 0);

  zx_device* sata_device = device_->GetLatestChild();
  std::thread device_init_thread([&]() {
    sata_device->InitOp();  // Issues IDENTIFY DEVICE command.
    sata_device->WaitUntilInitReplyCalled(zx::time::infinite());
    ASSERT_OK(sata_device->InitReplyCallStatus());
  });

  const uint32_t port_number = sata_device->GetDeviceContext<SataDevice>()->port();
  Port* port = controller_->port(port_number);
  const SataTransaction* command;
  while (true) {
    command = port->TestGetRunning(0);
    if (command != nullptr) {
      break;
    }
    // Wait until IDENTIFY DEVICE command is issued.
    zx::nanosleep(zx::deadline_after(zx::msec(1)));
  }
  ASSERT_EQ(command->cmd, SATA_CMD_IDENTIFY_DEVICE);

  // Perform IDENTIFY DEVICE command.
  SataIdentifyDeviceResponse devinfo{};
  devinfo.major_version = 1 << 10;  // Support ACS-3.
  devinfo.capabilities_1 = 1 << 9;  // Spec simply says, "Shall be set to one."
  devinfo.lba_capacity = kTestLogicalBlockCount;
  zx::unowned_vmo vmo(command->bop.rw.vmo);
  ASSERT_OK(vmo->write(&devinfo, 0, sizeof(devinfo)));

  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(port_number, kPortSataActive, 0);
  fake_bus_->PortRegOverride(port_number, kPortCommandIssue, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(port_number, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  fake_bus_->InterruptTrigger();

  device_init_thread.join();
  ASSERT_NO_FATAL_FAILURE(ReadInspect(controller_->inspector().DuplicateVmo()));
  const auto* ahci = hierarchy().GetByPath({"ahci"});
  ASSERT_NOT_NULL(ahci);
  const auto* ncq = ahci->node().get_property<inspect::BoolPropertyValue>("native_command_queuing");
  ASSERT_NOT_NULL(ncq);
  EXPECT_EQ(ncq->value(), support_native_command_queuing_);

  ddk::BlockImplProtocolClient client(sata_device);
  block_info_t info;
  uint64_t op_size;
  client.Query(&info, &op_size);
  EXPECT_EQ(info.block_size, 512);
  EXPECT_EQ(info.block_count, kTestLogicalBlockCount);
  if (support_native_command_queuing_) {
    EXPECT_TRUE(info.flags & FLAG_FUA_SUPPORT);
  } else {
    EXPECT_FALSE(info.flags & FLAG_FUA_SUPPORT);
  }

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo read_vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &read_vmo));
  auto block_op = std::make_unique<uint8_t[]>(op_size);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {.rw = {
             .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0},
             .vmo = read_vmo.get(),
             .length = 1,
             .offset_dev = 0,
             .offset_vmo = 0,
         }};
  client.Queue(op, callback, &done);

  while (true) {
    command = port->TestGetRunning(0);
    if (command != nullptr) {
      break;
    }
    // Wait until read command is issued.
    zx::nanosleep(zx::deadline_after(zx::msec(1)));
  }
  if (support_native_command_queuing_) {
    EXPECT_EQ(command->cmd, SATA_CMD_READ_FPDMA_QUEUED);
  } else {
    EXPECT_EQ(command->cmd, SATA_CMD_READ_DMA_EXT);
  }

  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(port_number, kPortSataActive, 0);
  fake_bus_->PortRegOverride(port_number, kPortCommandIssue, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(port_number, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  fake_bus_->InterruptTrigger();

  sync_completion_wait(&done, ZX_TIME_INFINITE);
}

INSTANTIATE_TEST_SUITE_P(NativeCommandQueuingSupportTest, AhciTestFixture, zxtest::Bool());

}  // namespace ahci
