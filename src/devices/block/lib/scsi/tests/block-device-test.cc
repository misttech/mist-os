// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/driver/testing/cpp/minimal_compat_environment.h>
#include <lib/fit/function.h>
#include <lib/scsi/block-device.h>
#include <lib/scsi/controller.h>
#include <sys/types.h>
#include <zircon/listnode.h>

#include <map>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace scsi {

// Controller for test; allows us to set expectations and fakes command responses.
class TestController : public fdf::DriverBase, public Controller {
 public:
  using IOCallbackType = fit::function<zx_status_t(uint8_t, uint16_t, iovec, bool, iovec)>;
  static constexpr char kDriverName[] = "scsi-test";

  TestController(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)) {}

  zx::result<> Start() override {
    parent_node_.Bind(std::move(node()));

    auto [controller_client_end, controller_server_end] =
        fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
    auto [node_client_end, node_server_end] =
        fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

    node_controller_.Bind(std::move(controller_client_end));
    root_node_.Bind(std::move(node_client_end));

    fidl::Arena arena;

    const auto args =
        fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, name()).Build();

    fidl::WireResult result =
        parent_node_->AddChild(args, std::move(controller_server_end), std::move(node_server_end));
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
      return zx::error(result.status());
    }
    return zx::ok();
  }

  ~TestController() { ZX_ASSERT(times_ == 0); }

  // Init the state required for testing async IOs.
  zx_status_t AsyncIoInit() {
    {
      fbl::AutoLock lock(&lock_);
      list_initialize(&queued_ios_);
      worker_thread_exit_ = false;
    }
    auto cb = [](void* arg) -> int { return static_cast<TestController*>(arg)->WorkerThread(); };
    if (thrd_create_with_name(&worker_thread_, cb, this, "scsi-test-controller") != thrd_success) {
      printf("%s: Failed to create worker thread\n", __FILE__);
      return ZX_ERR_INTERNAL;
    }
    return ZX_OK;
  }

  // De-Init the state required for testing async IOs.
  void AsyncIoRelease() {
    {
      fbl::AutoLock lock(&lock_);
      worker_thread_exit_ = true;
      cv_.Signal();
    }
    thrd_join(worker_thread_, nullptr);
    list_node_t* node;
    list_node_t* temp_node;
    fbl::AutoLock lock(&lock_);
    list_for_every_safe(&queued_ios_, node, temp_node) {
      auto* io = containerof(node, struct queued_io, node);
      list_delete(node);
      free(io);
    }
  }

  fidl::WireSyncClient<fuchsia_driver_framework::Node>& root_node() override { return root_node_; }
  std::string_view driver_name() const override { return name(); }
  const std::shared_ptr<fdf::Namespace>& driver_incoming() const override { return incoming(); }
  std::shared_ptr<fdf::OutgoingDirectory>& driver_outgoing() override { return outgoing(); }
  async_dispatcher_t* driver_async_dispatcher() const { return dispatcher(); }
  const std::optional<std::string>& driver_node_name() const override { return node_name(); }
  fdf::Logger& driver_logger() override { return logger(); }

  size_t BlockOpSize() override {
    // No additional metadata required for each command transaction.
    return sizeof(DeviceOp);
  }

  void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                           uint32_t block_size_bytes, DeviceOp* device_op, iovec data) override {
    // In the caller, enqueue the request for the worker thread,
    // poke the worker thread and return. The worker thread, on
    // waking up, will do the actual IO and call the callback.
    auto* io = reinterpret_cast<struct queued_io*>(new queued_io);
    io->target = target;
    io->lun = lun;
    // The cdb is allocated on the stack in the scsi::BlockDevice's BlockImplQueue.
    // So make a copy of that locally, and point to that instead
    memcpy(reinterpret_cast<void*>(&io->cdbptr), cdb.iov_base, cdb.iov_len);
    io->cdb.iov_base = &io->cdbptr;
    io->cdb.iov_len = cdb.iov_len;
    io->is_write = is_write;
    io->data_vmo = zx::unowned_vmo(device_op->op.rw.vmo);
    io->vmo_offset_bytes = device_op->op.rw.offset_vmo * block_size_bytes;
    io->transfer_bytes = device_op->op.rw.length * block_size_bytes;
    io->device_op = device_op;
    fbl::AutoLock lock(&lock_);
    list_add_tail(&queued_ios_, &io->node);
    cv_.Signal();
  }

  zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                 iovec data) override {
    EXPECT_TRUE(do_io_);
    EXPECT_GT(times_, 0);

    if (!do_io_ || times_ == 0) {
      return ZX_ERR_INTERNAL;
    }

    auto status = do_io_(target, lun, cdb, is_write, data);
    if (--times_ == 0) {
      decltype(do_io_) empty;
      do_io_.swap(empty);
    }
    return status;
  }

  void ExpectCall(IOCallbackType do_io, int times) {
    do_io_.swap(do_io);
    times_ = times;
  }

 private:
  IOCallbackType do_io_;
  int times_ = 0;

  int WorkerThread() {
    fbl::AutoLock lock(&lock_);
    while (true) {
      if (worker_thread_exit_ == true)
        return ZX_OK;
      // While non-empty, remove requests and execute them
      list_node_t* node;
      list_node_t* temp_node;
      list_for_every_safe(&queued_ios_, node, temp_node) {
        auto* io = containerof(node, struct queued_io, node);
        list_delete(node);
        zx_status_t status;
        std::unique_ptr<uint8_t[]> temp_buffer;

        if (io->data_vmo->is_valid()) {
          temp_buffer = std::make_unique<uint8_t[]>(io->transfer_bytes);
          // In case of WRITE command, populate the temp buffer with data from VMO.
          if (io->is_write) {
            status = zx_vmo_read(io->data_vmo->get(), temp_buffer.get(), io->vmo_offset_bytes,
                                 io->transfer_bytes);
            if (status != ZX_OK) {
              io->device_op->Complete(status);
              delete io;
              continue;
            }
          }
        }

        status = ExecuteCommandSync(io->target, io->lun, io->cdb, io->is_write,
                                    {temp_buffer.get(), io->transfer_bytes});

        // In case of READ command, populate the VMO with data from temp buffer.
        if (status == ZX_OK && !io->is_write && io->data_vmo->is_valid()) {
          status = zx_vmo_write(io->data_vmo->get(), temp_buffer.get(), io->vmo_offset_bytes,
                                io->transfer_bytes);
        }

        io->device_op->Complete(status);
        delete io;
      }
      cv_.Wait(&lock_);
    }
    return ZX_OK;
  }

  struct queued_io {
    list_node_t node;
    uint8_t target;
    uint16_t lun;
    // Deep copy of the CDB.
    union {
      Read16CDB readcdb;
      Write16CDB writecdb;
    } cdbptr;
    iovec cdb;
    bool is_write;
    zx::unowned_vmo data_vmo;
    zx_off_t vmo_offset_bytes;
    size_t transfer_bytes;
    DeviceOp* device_op;
  };

  // These are the state for testing Async IOs.
  // The test enqueues Async IOs and pokes the worker thread, which
  // does the IO, and calls back.
  fbl::Mutex lock_;
  fbl::ConditionVariable cv_;
  thrd_t worker_thread_;
  bool worker_thread_exit_ __TA_GUARDED(lock_);
  list_node_t queued_ios_ __TA_GUARDED(lock_);

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> root_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;
};

class TestConfig final {
 public:
  using DriverType = TestController;
  using EnvironmentType = fdf_testing::MinimalCompatEnvironment;
};

class BlockDeviceTest : public ::testing::Test {
 public:
  static constexpr uint8_t kTarget = 5;
  static constexpr uint16_t kLun = 1;
  static constexpr int kTransferSize = 32 * 1024;
  static constexpr uint32_t kBlockSize = 512;
  static constexpr uint64_t kFakeBlocks = 0x128000000;

  using DiskBlock = unsigned char[kBlockSize];

  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_OK(result);
    SetUpCommands();
  }
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_OK(result);
  }
  fdf_testing::ForegroundDriverTest<TestConfig>& driver_test() { return driver_test_; }

  void SetUpCommands() {
    // Set up default command expectations.
    driver_test().driver()->ExpectCall(
        [this](uint8_t target, uint16_t lun, iovec cdb, bool is_write, iovec data) -> auto {
          EXPECT_EQ(target, kTarget);
          EXPECT_EQ(lun, kLun);

          switch (default_seq_) {
            case 0: {
              EXPECT_EQ(cdb.iov_len, size_t{6});
              InquiryCDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::INQUIRY);
              EXPECT_FALSE(is_write);
              break;
            }
            case 1: {
              EXPECT_EQ(cdb.iov_len, size_t{6});
              InquiryCDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::TEST_UNIT_READY);
              EXPECT_FALSE(is_write);
              break;
            }
            case 2: {
              if (cdb.iov_len == 6) {
                ModeSense6CDB decoded_cdb = {};
                memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
                EXPECT_EQ(decoded_cdb.opcode, Opcode::MODE_SENSE_6);
                EXPECT_EQ(decoded_cdb.page_code(), PageCode::kAllPageCode);
                EXPECT_EQ(decoded_cdb.disable_block_descriptors(), true);
                EXPECT_FALSE(is_write);
                Mode6ParameterHeader header = {};
                memcpy(data.iov_base, reinterpret_cast<char*>(&header), sizeof(header));
              } else {
                EXPECT_EQ(cdb.iov_len, size_t{10});
                ModeSense10CDB decoded_cdb = {};
                memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
                EXPECT_EQ(decoded_cdb.opcode, Opcode::MODE_SENSE_10);
                EXPECT_EQ(decoded_cdb.page_code(), PageCode::kAllPageCode);
                EXPECT_EQ(decoded_cdb.disable_block_descriptors(), true);
                EXPECT_FALSE(is_write);
                Mode10ParameterHeader header = {};
                memcpy(data.iov_base, reinterpret_cast<char*>(&header), sizeof(header));
              }
              break;
            }
            case 3: {
              if (cdb.iov_len == 6) {
                ModeSense6CDB decoded_cdb = {};
                memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
                EXPECT_EQ(decoded_cdb.opcode, Opcode::MODE_SENSE_6);
                EXPECT_EQ(decoded_cdb.page_code(), PageCode::kCachingPageCode);
                EXPECT_EQ(decoded_cdb.disable_block_descriptors(), true);
                EXPECT_FALSE(is_write);
                Mode6ParameterHeader header = {};
                memcpy(data.iov_base, reinterpret_cast<char*>(&header), sizeof(header));
                CachingModePage response = {};
                response.set_page_code(static_cast<uint8_t>(PageCode::kCachingPageCode));
                memcpy(static_cast<char*>(data.iov_base) + sizeof(header),
                       reinterpret_cast<char*>(&response), sizeof(response));
              } else {
                EXPECT_EQ(cdb.iov_len, size_t{10});
                ModeSense10CDB decoded_cdb = {};
                memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
                EXPECT_EQ(decoded_cdb.opcode, Opcode::MODE_SENSE_10);
                EXPECT_EQ(decoded_cdb.page_code(), PageCode::kCachingPageCode);
                EXPECT_EQ(decoded_cdb.disable_block_descriptors(), true);
                EXPECT_FALSE(is_write);
                Mode10ParameterHeader header = {};
                memcpy(data.iov_base, reinterpret_cast<char*>(&header), sizeof(header));
                CachingModePage response = {};
                response.set_page_code(static_cast<uint8_t>(PageCode::kCachingPageCode));
                memcpy(static_cast<char*>(data.iov_base) + sizeof(header),
                       reinterpret_cast<char*>(&response), sizeof(response));
              }
              break;
            }
            case 4: {
              EXPECT_EQ(cdb.iov_len, size_t{10});
              ReadCapacity10CDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::READ_CAPACITY_10);
              EXPECT_FALSE(is_write);
              ReadCapacity10ParameterData response = {};
              response.returned_logical_block_address = htobe32(UINT32_MAX);
              response.block_length_in_bytes = htobe32(kBlockSize);
              memcpy(data.iov_base, reinterpret_cast<char*>(&response), sizeof(response));
              break;
            }
            case 5: {
              EXPECT_EQ(cdb.iov_len, size_t{16});
              ReadCapacity16CDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::READ_CAPACITY_16);
              EXPECT_EQ(decoded_cdb.service_action, 0x10);
              EXPECT_FALSE(is_write);
              ReadCapacity16ParameterData response = {};
              response.returned_logical_block_address = htobe64(kFakeBlocks - 1);
              response.block_length_in_bytes = htobe32(kBlockSize);
              memcpy(data.iov_base, reinterpret_cast<char*>(&response), sizeof(response));
              break;
            }
            case 6: {
              EXPECT_EQ(cdb.iov_len, size_t{6});
              InquiryCDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::INQUIRY);
              EXPECT_EQ(decoded_cdb.page_code, scsi::InquiryCDB::kPageListVpdPageCode);
              EXPECT_FALSE(is_write);
              VPDPageList vpd_page_list = {};
              vpd_page_list.peripheral_qualifier_device_type = 0;
              vpd_page_list.page_code = InquiryCDB::kPageListVpdPageCode;
              vpd_page_list.page_length = 2;
              vpd_page_list.pages[0] = InquiryCDB::kBlockLimitsVpdPageCode;
              vpd_page_list.pages[1] = InquiryCDB::kLogicalBlockProvisioningVpdPageCode;
              memcpy(data.iov_base, reinterpret_cast<char*>(&vpd_page_list), sizeof(vpd_page_list));
              break;
            }
            case 7: {
              EXPECT_EQ(cdb.iov_len, size_t{6});
              InquiryCDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::INQUIRY);
              EXPECT_EQ(decoded_cdb.page_code, InquiryCDB::kBlockLimitsVpdPageCode);
              EXPECT_FALSE(is_write);
              VPDBlockLimits block_limits = {};
              block_limits.peripheral_qualifier_device_type = 0;
              block_limits.page_code = scsi::InquiryCDB::kBlockLimitsVpdPageCode;
              block_limits.maximum_unmap_lba_count = htobe32(UINT32_MAX);
              break;
            }
            case 8: {
              EXPECT_EQ(cdb.iov_len, size_t{6});
              InquiryCDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::INQUIRY);
              EXPECT_EQ(decoded_cdb.page_code, InquiryCDB::kPageListVpdPageCode);
              EXPECT_FALSE(is_write);
              VPDPageList vpd_page_list = {};
              vpd_page_list.peripheral_qualifier_device_type = 0;
              vpd_page_list.page_code = InquiryCDB::kPageListVpdPageCode;
              vpd_page_list.page_length = 2;
              vpd_page_list.pages[0] = InquiryCDB::kBlockLimitsVpdPageCode;
              vpd_page_list.pages[1] = InquiryCDB::kLogicalBlockProvisioningVpdPageCode;
              memcpy(data.iov_base, reinterpret_cast<char*>(&vpd_page_list), sizeof(vpd_page_list));
              break;
            }
            case 9: {
              EXPECT_EQ(cdb.iov_len, size_t{6});
              InquiryCDB decoded_cdb = {};
              memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
              EXPECT_EQ(decoded_cdb.opcode, Opcode::INQUIRY);
              EXPECT_EQ(decoded_cdb.page_code, InquiryCDB::kLogicalBlockProvisioningVpdPageCode);
              EXPECT_FALSE(is_write);
              VPDLogicalBlockProvisioning provisioning = {};
              provisioning.peripheral_qualifier_device_type = 0;
              provisioning.page_code = scsi::InquiryCDB::kLogicalBlockProvisioningVpdPageCode;
              provisioning.set_lbpu(true);
              provisioning.set_provisioning_type(0x02);  // The logical unit is thin provisioned
              break;
            }
          }
          default_seq_++;

          return ZX_OK;
        },
        /*times=*/10);
  }

  zx::result<PostProcess> CheckScsiStatus(StatusCode status_code,
                                          FixedFormatSenseDataHeader& sense_data) {
    return driver_test().driver()->CheckScsiStatus(status_code, sense_data);
  }

 private:
  fdf_testing::ForegroundDriverTest<TestConfig> driver_test_;
  int default_seq_ = 0;
};

// Test that we can create a block device when the underlying controller successfully executes CDBs.
TEST_F(BlockDeviceTest, TestCreateDestroy) {
  ASSERT_OK(BlockDevice::Bind(driver_test().driver(), kTarget, kLun, kTransferSize,
                              DeviceOptions(/*check_unmap_support=*/true, /*use_mode_sense_6=*/true,
                                            /*use_read_write_12=*/true)));
  driver_test().RunInNodeContext(
      [](fdf_testing::TestNode& node) { ASSERT_EQ(size_t{1}, node.children().size()); });
}

// Test that we can create a block device when the underlying controller successfully executes CDBs.
TEST_F(BlockDeviceTest, TestCreateDestroyWithModeSense10) {
  ASSERT_OK(
      BlockDevice::Bind(driver_test().driver(), kTarget, kLun, kTransferSize,
                        DeviceOptions(/*check_unmap_support=*/true, /*use_mode_sense_6=*/false,
                                      /*use_read_write_12=*/true)));
  driver_test().RunInNodeContext(
      [](fdf_testing::TestNode& node) { ASSERT_EQ(size_t{1}, node.children().size()); });
}

// Test creating a block device and executing read commands.
TEST_F(BlockDeviceTest, TestCreateReadDestroy) {
  auto dev =
      BlockDevice::Bind(driver_test().driver(), kTarget, kLun, kTransferSize,
                        DeviceOptions(/*check_unmap_support=*/true, /*use_mode_sense_6=*/true,
                                      /*use_read_write_12=*/true));
  ASSERT_OK(dev);
  driver_test().RunInNodeContext(
      [](fdf_testing::TestNode& node) { ASSERT_EQ(size_t{1}, node.children().size()); });
  block_info_t info;
  size_t op_size;
  dev->BlockImplQuery(&info, &op_size);

  // To test SCSI Read functionality, create a fake "block device" backing store in memory and
  // service reads from it. Fill block 1 with a test pattern of 0x01.
  std::map<uint64_t, DiskBlock> blocks;
  DiskBlock& test_block_1 = blocks[1];
  memset(test_block_1, 0x01, sizeof(DiskBlock));

  driver_test().driver()->ExpectCall(
      [&blocks](uint8_t target, uint16_t lun, iovec cdb, bool is_write, iovec data) -> auto {
        EXPECT_EQ(cdb.iov_len, size_t{16});
        Read16CDB decoded_cdb = {};
        memcpy(&decoded_cdb, cdb.iov_base, cdb.iov_len);
        EXPECT_EQ(decoded_cdb.opcode, Opcode::READ_16);
        EXPECT_FALSE(is_write);

        // Support reading one block.
        EXPECT_EQ(be32toh(decoded_cdb.transfer_length), uint32_t{1});
        uint64_t block_to_read = be64toh(decoded_cdb.logical_block_address);
        const DiskBlock& data_to_return = blocks.at(block_to_read);
        memcpy(data.iov_base, data_to_return, sizeof(DiskBlock));

        return ZX_OK;
      },
      /*times=*/1);

  // Issue a read to block 1 that should work.
  struct IoWait {
    fbl::Mutex lock_;
    fbl::ConditionVariable cv_;
  };
  IoWait iowait_;
  auto block_op = std::make_unique<uint8_t[]>(op_size);
  block_op_t& read = *reinterpret_cast<block_op_t*>(block_op.get());
  block_impl_queue_callback done = [](void* ctx, zx_status_t status, block_op_t* op) {
    IoWait* iowait_ = reinterpret_cast<struct IoWait*>(ctx);

    fbl::AutoLock lock(&iowait_->lock_);
    iowait_->cv_.Signal();
  };
  read.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  read.rw.length = 1;      // Read one block
  read.rw.offset_dev = 1;  // Read logical block 1
  read.rw.offset_vmo = 0;
  EXPECT_OK(zx_vmo_create(zx_system_get_page_size(), 0, &read.rw.vmo));
  driver_test().driver()->AsyncIoInit();
  {
    fbl::AutoLock lock(&iowait_.lock_);
    dev->BlockImplQueue(&read, done, &iowait_);  // NOTE: Assumes asynchronous controller
    iowait_.cv_.Wait(&iowait_.lock_);
  }
  // Make sure the contents of the VMO we read into match the expected test pattern
  DiskBlock check_buffer = {};
  EXPECT_OK(zx_vmo_read(read.rw.vmo, check_buffer, 0, sizeof(DiskBlock)));
  for (uint i = 0; i < sizeof(DiskBlock); i++) {
    EXPECT_EQ(check_buffer[i], 0x01);
  }
  driver_test().driver()->AsyncIoRelease();
}

TEST_F(BlockDeviceTest, ScsiComplete) {
  ASSERT_OK(BlockDevice::Bind(driver_test().driver(), kTarget, kLun, kTransferSize,
                              DeviceOptions(/*check_unmap_support=*/true, /*use_mode_sense_6=*/true,
                                            /*use_read_write_12=*/true)));
  driver_test().RunInNodeContext(
      [](fdf_testing::TestNode& node) { ASSERT_EQ(size_t{1}, node.children().size()); });

  StatusMessage status_message = {HostStatusCode::kOk, StatusCode::GOOD};

  FixedFormatSenseDataHeader sense_data;
  sense_data.set_response_code(SenseDataResponseCodes::kFixedCurrentInformation);
  sense_data.set_filemark(false);
  sense_data.set_eom(false);
  sense_data.set_ili(false);
  sense_data.set_sense_key(SenseKey::NO_SENSE);
  // ASC=00h, ASCQ=00h, NO ADDITIONAL SENSE INFORMATION
  sense_data.additional_sense_code = 0x0;
  sense_data.additional_sense_code_qualifier = 0x0;

  // Success
  EXPECT_OK(driver_test().driver()->ScsiComplete(status_message, sense_data));

  // Abort
  status_message.host_status_code = HostStatusCode::kAbort;
  EXPECT_EQ(driver_test().driver()->ScsiComplete(status_message, sense_data).status_value(),
            ZX_ERR_IO_REFUSED);

  // Unexpected host status value
  status_message.host_status_code = HostStatusCode::kUnknown;
  EXPECT_EQ(driver_test().driver()->ScsiComplete(status_message, sense_data).status_value(),
            ZX_ERR_BAD_STATE);

  // Error handling
  status_message.host_status_code = HostStatusCode::kTimeout;
  EXPECT_OK(driver_test().driver()->ScsiComplete(status_message, sense_data));

  // Retry
  status_message.host_status_code = HostStatusCode::kRequeue;
  EXPECT_EQ(driver_test().driver()->ScsiComplete(status_message, sense_data).status_value(),
            ZX_ERR_BAD_STATE);
}

TEST_F(BlockDeviceTest, CheckScsiStatus) {
  ASSERT_OK(BlockDevice::Bind(driver_test().driver(), kTarget, kLun, kTransferSize,
                              DeviceOptions(/*check_unmap_support=*/true, /*use_mode_sense_6=*/true,
                                            /*use_read_write_12=*/true)));
  driver_test().RunInNodeContext(
      [](fdf_testing::TestNode& node) { ASSERT_EQ(size_t{1}, node.children().size()); });

  FixedFormatSenseDataHeader sense_data;
  sense_data.set_response_code(SenseDataResponseCodes::kFixedCurrentInformation);
  sense_data.set_filemark(false);
  sense_data.set_eom(false);
  sense_data.set_ili(false);
  sense_data.set_sense_key(SenseKey::NO_SENSE);
  // ASC=00h, ASCQ=00h, NO ADDITIONAL SENSE INFORMATION
  sense_data.additional_sense_code = 0x0;
  sense_data.additional_sense_code_qualifier = 0x0;

  // StatusCode::GOOD, TASK_ABORTED
  {
    EXPECT_OK(CheckScsiStatus(StatusCode::GOOD, sense_data));
  }

  // StatusCode::CHECK_CONDITION
  {
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNone);
  }

  // StatusCode::TASK_SET_FULL
  {
    auto post_process = CheckScsiStatus(StatusCode::TASK_SET_FULL, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNeedsRetry);
  }

  // StatusCode::BUSY
  {
    auto post_process = CheckScsiStatus(StatusCode::BUSY, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNeedsRetry);
  }

  // Not supported status codes
  {
    auto post_process = CheckScsiStatus(StatusCode::CONDITION_MET, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_NOT_SUPPORTED);
  }
}

TEST_F(BlockDeviceTest, CheckSenseData) {
  ASSERT_OK(BlockDevice::Bind(driver_test().driver(), kTarget, kLun, kTransferSize,
                              DeviceOptions(/*check_unmap_support=*/true, /*use_mode_sense_6=*/true,
                                            /*use_read_write_12=*/true)));
  driver_test().RunInNodeContext(
      [](fdf_testing::TestNode& node) { ASSERT_EQ(size_t{1}, node.children().size()); });

  FixedFormatSenseDataHeader sense_data;
  sense_data.set_response_code(SenseDataResponseCodes::kFixedCurrentInformation);
  sense_data.set_filemark(false);
  sense_data.set_eom(false);
  sense_data.set_ili(false);
  sense_data.set_sense_key(SenseKey::NO_SENSE);
  // ASC=00h, ASCQ=00h, NO ADDITIONAL SENSE INFORMATION
  sense_data.additional_sense_code = 0x0;
  sense_data.additional_sense_code_qualifier = 0x0;

  // Invalid response code
  {
    sense_data.set_response_code(SenseDataResponseCodes::kDescriptorCurrentInformation);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_NOT_SUPPORTED);

    sense_data.set_response_code(SenseDataResponseCodes::kFixedCurrentInformation);
  }

  // Invalid FILEMARK, EOM, ILI
  {
    sense_data.set_filemark(true);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_INVALID_ARGS);
    sense_data.set_filemark(false);
  }

  // Invalid EOM
  {
    sense_data.set_eom(true);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_INVALID_ARGS);
    sense_data.set_eom(false);
  }

  // Invalid ILI
  {
    sense_data.set_ili(true);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_INVALID_ARGS);
    sense_data.set_ili(false);
  }

  // SenseKey::NO_SENSE
  {
    sense_data.set_sense_key(SenseKey::NO_SENSE);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNone);
  }

  // SenseKey::RECOVERED_ERROR
  {
    sense_data.set_sense_key(SenseKey::NO_SENSE);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNone);
  }

  // SenseKey::ABORTED_COMMAND
  {
    sense_data.set_sense_key(SenseKey::ABORTED_COMMAND);
    sense_data.additional_sense_code = 0x10;  // DIF
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_IO_DATA_INTEGRITY);

    // ASC=0x2e, ASCQ=0x01: COMMAND TIMEOUT BEFORE PROCESSING
    sense_data.additional_sense_code = 0x2e;
    sense_data.additional_sense_code_qualifier = 0x01;
    post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_TIMED_OUT);
    sense_data.additional_sense_code = 0;
    sense_data.additional_sense_code_qualifier = 0;

    post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNeedsRetry);
  }

  // SenseKey::NOT_READY, UNIT_ATTENTION
  {
    // Expected UNIT_ATTENTION
    sense_data.set_sense_key(SenseKey::UNIT_ATTENTION);
    driver_test().driver()->SetExpectCheckConditionOrUnitAttention(true);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNeedsRetry);

    // Unit is not ready
    driver_test().driver()->SetExpectCheckConditionOrUnitAttention(false);
    // ASC=0x04, ASCQ=0x01: LOGICAL UNIT IS IN PROCESS OF BECOMING READY
    sense_data.additional_sense_code = 0x04;
    sense_data.additional_sense_code_qualifier = 0x01;
    post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_OK(post_process);
    EXPECT_EQ(post_process.value(), PostProcess::kNeedsRetry);
    sense_data.additional_sense_code = 0;
    sense_data.additional_sense_code_qualifier = 0;

    post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_BAD_STATE);
  }

  // Not supported
  {
    sense_data.set_sense_key(SenseKey::MEDIUM_ERROR);
    auto post_process = CheckScsiStatus(StatusCode::CHECK_CONDITION, sense_data);
    EXPECT_EQ(post_process.status_value(), ZX_ERR_NOT_SUPPORTED);
  }
}

}  // namespace scsi

FUCHSIA_DRIVER_EXPORT(scsi::TestController);
