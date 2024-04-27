// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <byteswap.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>
#include <lib/zx/clock.h>

#include <thread>

#include <zxtest/zxtest.h>

#include "../controller.h"
#include "fake-bus.h"

namespace ahci {

class PortTest : public zxtest::Test {
 protected:
  void SetUp() override { fdf::Logger::SetGlobalInstance(&logger_); }

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
    std::unique_ptr<FakeBus> bus(new FakeBus());
    EXPECT_OK(bus->Configure());

    PortEnable(bus.get(), port);

    fake_bus_ = std::move(bus);
  }

  // If non-null, this pointer is owned by Controller::bus_
  std::unique_ptr<FakeBus> fake_bus_;

  fdf::Logger logger_{"ahci-test", FUCHSIA_LOG_DEBUG, zx::socket{},
                      fidl::WireClient<fuchsia_logger::LogSink>()};
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

class TestController : public Controller {
 public:
  // Modify to configure the behaviour of this test controller.
  static bool support_native_command_queuing_;

  static constexpr uint32_t kTestLogicalBlockCount = 1024;

  TestController(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : Controller(std::move(start_args), std::move(dispatcher)) {}

  zx::result<std::unique_ptr<Bus>> CreateBus() override {
    // Create a fake bus.
    auto fake_bus = std::make_unique<FakeBus>(support_native_command_queuing_);

    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "sata-background-init",
        [this](fdf_dispatcher_t*) { test_shutdown_completion_.Signal(); });
    if (dispatcher.is_error()) {
      return dispatcher.take_error();
    }
    test_dispatcher_ = *std::move(dispatcher);

    // Background work to emulate a SATA device on the fake bus responding to an IDENTIFY DEVICE
    // command.
    async::PostTask(test_dispatcher_.async_dispatcher(), [this, fake_bus = fake_bus.get()] {
      Port* port = this->port(FakeBus::kTestPortNumber);
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
      fake_bus->PortRegOverride(FakeBus::kTestPortNumber, kPortSataActive, 0);
      fake_bus->PortRegOverride(FakeBus::kTestPortNumber, kPortCommandIssue, 0);

      // Set interrupt for successful transfer completion.
      fake_bus->PortRegOverride(FakeBus::kTestPortNumber, kPortInterruptStatus, AHCI_PORT_INT_DP);
      // Invoke interrupt handler.
      fake_bus->InterruptTrigger();
    });

    return zx::ok(std::move(fake_bus));
  }

  void PrepareStop(fdf::PrepareStopCompleter completer) override {
    if (test_dispatcher_.get()) {
      test_dispatcher_.ShutdownAsync();
      test_shutdown_completion_.Wait();
    }
    Shutdown();
    completer(zx::ok());
  }

 private:
  fdf::Dispatcher test_dispatcher_;
  // Signaled when test_dispatcher_ is shut down.
  libsync::Completion test_shutdown_completion_;
};

bool TestController::support_native_command_queuing_;

struct IncomingNamespace {
  fdf_testing::TestNode node{"root"};
  fdf_testing::TestEnvironment env{fdf::Dispatcher::GetCurrent()->get()};
};

class AhciTest : public inspect::InspectTestHelper, public zxtest::TestWithParam<bool> {
 public:
  AhciTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher_->async_dispatcher(), std::in_place) {}

  void SetUp() override {
    TestController::support_native_command_queuing_ = GetParam();

    // Initialize driver test environment.
    fuchsia_driver_framework::DriverStartArgs start_args;
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
    incoming_.SyncCall([&](IncomingNamespace* incoming) mutable {
      auto start_args_result = incoming->node.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client = std::move(start_args_result->outgoing_directory_client);

      ASSERT_OK(incoming->env.Initialize(std::move(start_args_result->incoming_directory_server)));
    });

    // Start dut_.
    ASSERT_OK(runtime_.RunToCompletion(dut_.Start(std::move(start_args))));

    fake_bus_ = static_cast<FakeBus*>(dut_->bus());
    ASSERT_NOT_NULL(fake_bus_);

    ASSERT_GT(dut_->sata_devices().size(), 0);
    sata_device_ = dut_->sata_devices()[0].get();
    ASSERT_NOT_NULL(sata_device_);
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
    EXPECT_OK(prepare_stop_result);

    incoming_.reset();
    runtime_.ShutdownAllDispatchers(fdf::Dispatcher::GetCurrent()->get());

    EXPECT_OK(dut_.Stop());
  }

 protected:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  fdf_testing::DriverUnderTest<TestController> dut_;
  FakeBus* fake_bus_;
  SataDevice* sata_device_;
};

TEST_P(AhciTest, SataDeviceRead) {
  ASSERT_NO_FATAL_FAILURE(ReadInspect(dut_->inspector().DuplicateVmo()));
  const auto* ahci = hierarchy().GetByPath({"ahci"});
  ASSERT_NOT_NULL(ahci);
  const auto* ncq = ahci->node().get_property<inspect::BoolPropertyValue>("native_command_queuing");
  ASSERT_NOT_NULL(ncq);
  EXPECT_EQ(ncq->value(), TestController::support_native_command_queuing_);

  block_info_t info;
  uint64_t op_size;
  sata_device_->BlockImplQuery(&info, &op_size);
  EXPECT_EQ(info.block_size, 512);
  EXPECT_EQ(info.block_count, TestController::kTestLogicalBlockCount);
  if (TestController::support_native_command_queuing_) {
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
  sata_device_->BlockImplQueue(op, callback, &done);

  Port* port = dut_->port(FakeBus::kTestPortNumber);
  const SataTransaction* command;
  while (true) {
    command = port->TestGetRunning(0);
    if (command != nullptr) {
      break;
    }
    // Wait until read command is issued.
    zx::nanosleep(zx::deadline_after(zx::msec(1)));
  }
  if (TestController::support_native_command_queuing_) {
    EXPECT_EQ(command->cmd, SATA_CMD_READ_FPDMA_QUEUED);
  } else {
    EXPECT_EQ(command->cmd, SATA_CMD_READ_DMA_EXT);
  }

  // Clear the running bit in the bus.
  fake_bus_->PortRegOverride(FakeBus::kTestPortNumber, kPortSataActive, 0);
  fake_bus_->PortRegOverride(FakeBus::kTestPortNumber, kPortCommandIssue, 0);

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(FakeBus::kTestPortNumber, kPortInterruptStatus, AHCI_PORT_INT_DP);
  // Invoke interrupt handler.
  fake_bus_->InterruptTrigger();

  sync_completion_wait(&done, ZX_TIME_INFINITE);
}

TEST_P(AhciTest, ShutdownWaitsForTransactionsInFlight) {
  Port* port = dut_->port(FakeBus::kTestPortNumber);

  // Set up a transaction that will timeout in 5 seconds.

  zx_status_t status = ZX_OK;  // Value to be overwritten by callback.

  SataTransaction txn = {};
  txn.timeout = zx::clock::get_monotonic() + zx::sec(5);
  txn.completion_cb = cb_status;
  txn.cookie = &status;

  uint32_t slot = 0;

  // Set txn as running.
  port->TestSetRunning(&txn, slot);
  // Set the running bit in the bus.
  fake_bus_->PortRegOverride(FakeBus::kTestPortNumber, kPortSataActive, (1u << slot));
  fake_bus_->PortRegOverride(FakeBus::kTestPortNumber, kPortCommandIssue, (1u << slot));

  // Set interrupt for successful transfer completion.
  fake_bus_->PortRegOverride(FakeBus::kTestPortNumber, kPortInterruptStatus, AHCI_PORT_INT_DP);

  // True means there are running command(s).
  EXPECT_TRUE(port->Complete());

  zx::time time = zx::clock::get_monotonic();
  dut_->Shutdown();
  zx::duration shutdown_duration = zx::clock::get_monotonic() - time;

  // The shutdown duration should be around 5 seconds (+/-). Conservatively check for > 2.5 seconds.
  EXPECT_GT(shutdown_duration, zx::msec(2500));

  // Set by completion callback.
  EXPECT_EQ(status, ZX_ERR_TIMED_OUT);
}

INSTANTIATE_TEST_SUITE_P(NativeCommandQueuingSupportTest, AhciTest, zxtest::Bool());

}  // namespace ahci

FUCHSIA_DRIVER_EXPORT(ahci::TestController);
