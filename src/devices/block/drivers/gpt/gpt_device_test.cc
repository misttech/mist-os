// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpt.metadata/cpp/fidl.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <inttypes.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>
#include <zircon/hw/gpt.h>

#include <iterator>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>
#include <fbl/string.h>
#include <fbl/vector.h>
#include <gpt/c/gpt.h>
#include <zxtest/zxtest.h>

#include "gpt.h"
#include "gpt_test_data.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace gpt {
namespace {

// To make sure that we correctly convert UTF-16, to UTF-8, the second partition has a suffix with
// codepoint 0x10000, which in UTF-16 requires a surrogate pair.
constexpr std::string_view kPartition1Name = "Linux filesystem\xf0\x90\x80\x80";

class FakeBlockDevice : public ddk::BlockProtocol<FakeBlockDevice> {
 public:
  FakeBlockDevice()
      : header_(new uint8_t[sizeof(test_partition_table)]),
        header_len_(sizeof(test_partition_table)),
        proto_({&block_protocol_ops_, this}) {
    std::copy(std::begin(test_partition_table), std::end(test_partition_table), header_.get());
    info_.block_count = kBlockCnt;
    info_.block_size = kBlockSz;
    info_.max_transfer_size = fuchsia_hardware_block::wire::kMaxTransferUnbounded;
  }

  block_protocol_t* proto() { return &proto_; }

  void SetInfo(const block_info_t* info) { info_ = *info; }

  void BlockQuery(block_info_t* info_out, size_t* block_op_size_out) {
    *info_out = info_;
    *block_op_size_out = sizeof(block_op_t);
  }

  void BlockQueue(block_op_t* operation, block_queue_callback completion_cb, void* cookie);

 private:
  zx_status_t BlockQueueOp(block_op_t* op);

  std::unique_ptr<uint8_t[]> header_;
  size_t header_len_;
  block_protocol_t proto_{};
  block_info_t info_{};
};

void FakeBlockDevice::BlockQueue(block_op_t* operation, block_queue_callback completion_cb,
                                 void* cookie) {
  zx_status_t status = BlockQueueOp(operation);
  completion_cb(cookie, status, operation);
}

zx_status_t FakeBlockDevice::BlockQueueOp(block_op_t* op) {
  const uint8_t opcode = op->command.opcode;
  const uint32_t bsize = info_.block_size;
  if (opcode == BLOCK_OPCODE_READ || opcode == BLOCK_OPCODE_WRITE) {
    if ((op->rw.offset_dev + op->rw.length) > (bsize * info_.block_count)) {
      return ZX_ERR_OUT_OF_RANGE;
    }
  } else if (opcode == BLOCK_OPCODE_TRIM) {
    if ((op->trim.offset_dev + op->trim.length) > (bsize * info_.block_count)) {
      return ZX_ERR_OUT_OF_RANGE;
    }
    return ZX_OK;
  } else if (opcode == BLOCK_OPCODE_FLUSH) {
    return ZX_OK;
  } else {
    return ZX_ERR_NOT_SUPPORTED;
  }

  size_t rw_off = op->rw.offset_dev * bsize;
  size_t rw_len = op->rw.length * bsize;

  if (rw_len == 0) {
    return ZX_OK;
  }

  uint64_t vmo_addr = op->rw.offset_vmo * bsize;

  // Read or write initial part from header if in range.
  if (rw_off < header_len_) {
    size_t part_rw_len = header_len_ - rw_off;
    if (part_rw_len > rw_len) {
      part_rw_len = rw_len;
    }
    if (opcode == BLOCK_OPCODE_READ) {
      if (zx_status_t status =
              zx_vmo_write(op->rw.vmo, header_.get() + rw_off, vmo_addr, part_rw_len);
          status != ZX_OK) {
        return status;
      }
    } else {
      if (zx_status_t status =
              zx_vmo_read(op->rw.vmo, header_.get() + rw_off, vmo_addr, part_rw_len);
          status != ZX_OK) {
        return status;
      }
    }

    rw_len -= part_rw_len;
    rw_off += part_rw_len;
    vmo_addr += part_rw_len;

    if (rw_len == 0) {
      return ZX_OK;
    }
  }

  if (opcode == BLOCK_OPCODE_WRITE) {
    return ZX_OK;
  }

  std::unique_ptr<uint8_t[]> zbuf(new uint8_t[bsize]);
  memset(zbuf.get(), 0, bsize);
  // Zero-fill remaining.
  for (; rw_len > 0; rw_len -= bsize) {
    zx_vmo_write(op->rw.vmo, zbuf.get(), vmo_addr, bsize);
    vmo_addr += bsize;
  }
  return ZX_OK;
}

class GptDeviceTest : public zxtest::Test {
 public:
  GptDeviceTest() = default;

  void SetInfo(const block_info_t* info) { fake_block_device_.SetInfo(info); }

  void SetUp() override {
    fake_parent_->AddProtocol(ZX_PROTOCOL_BLOCK, fake_block_device_.proto()->ops,
                              fake_block_device_.proto()->ctx);
  }

  zx_status_t Bind() {
    zx_status_t status = PartitionManager::Bind(nullptr, fake_parent_.get());
    if (status == ZX_OK) {
      loop_.StartThread();
      auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_block_volume::VolumeManager>();
      EXPECT_OK(endpoints.status_value());
      fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), GetPartitionManager());
      volume_manager_fidl_.Bind(std::move(endpoints->client));
    }
    return status;
  }

  zx_status_t Rebind() {
    volume_manager_fidl_.TakeClientEnd();
    loop_.RunUntilIdle();
    fake_parent_ = MockDevice::FakeRootParent();
    fake_parent_->AddProtocol(ZX_PROTOCOL_BLOCK, fake_block_device_.proto()->ops,
                              fake_block_device_.proto()->ctx);
    return Bind();
  }

  void TearDown() override { loop_.Shutdown(); }

  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();

  fidl::WireSyncClient<fuchsia_hardware_block_volume::VolumeManager>& volume_manager_fidl() {
    return volume_manager_fidl_;
  }

  PartitionDevice* GetDev(size_t device_number) {
    // Skip the first device which is the partition manager.
    ZX_ASSERT(fake_parent_->child_count() > device_number + 1);
    auto iter = std::next(fake_parent_->children().begin(), device_number + 1);
    return (*iter)->GetDeviceContext<PartitionDevice>();
  }

 protected:
  struct BlockOpResult {
    sync_completion_t completion;
    block_op_t op;
    zx_status_t status;
  };

  static void BlockOpCompleter(void* cookie, zx_status_t status, block_op_t* bop) {
    auto* result = static_cast<BlockOpResult*>(cookie);
    result->status = status;
    result->op = *bop;
    sync_completion_signal(&result->completion);
  }

 private:
  PartitionManager* GetPartitionManager() {
    ZX_ASSERT(fake_parent_->child_count() > 1);
    return fake_parent_->children().front()->GetDeviceContext<PartitionManager>();
  }

  fidl::WireSyncClient<fuchsia_hardware_block_volume::VolumeManager> volume_manager_fidl_;
  FakeBlockDevice fake_block_device_;
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

TEST_F(GptDeviceTest, DeviceTooSmall) {
  const block_info_t info = {20, 512, fuchsia_hardware_block::wire::kMaxTransferUnbounded, 0};
  SetInfo(&info);

  ASSERT_STATUS(ZX_ERR_NO_SPACE, PartitionManager::Bind(nullptr, fake_parent_.get()));
}

TEST_F(GptDeviceTest, ValidatePartitionGuid) {
  ASSERT_OK(Bind());
  ASSERT_EQ(fake_parent_->child_count(), 3);

  char name[MAX_PARTITION_NAME_LENGTH];
  guid_t guid;

  // Device 0
  PartitionDevice* dev0 = GetDev(0);
  ASSERT_OK(dev0->BlockPartitionGetName(name, sizeof(name)));
  ASSERT_EQ(strcmp(name, "Linux filesystem"), 0);
  ASSERT_OK(dev0->BlockPartitionGetGuid(GUIDTYPE_TYPE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_LINUX_FILESYSTEM;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }
  ASSERT_OK(dev0->BlockPartitionGetGuid(GUIDTYPE_INSTANCE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_UNIQUE_PART0;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }

  PartitionDevice* dev1 = GetDev(1);
  ASSERT_OK(dev1->BlockPartitionGetName(name, sizeof(name)));
  ASSERT_EQ(kPartition1Name, name);

  ASSERT_OK(dev1->BlockPartitionGetGuid(GUIDTYPE_TYPE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_LINUX_FILESYSTEM;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }
  ASSERT_OK(dev1->BlockPartitionGetGuid(GUIDTYPE_INSTANCE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_UNIQUE_PART1;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }
}

TEST_F(GptDeviceTest, ValidatePartitionBindProps) {
  ASSERT_OK(Bind());
  ASSERT_EQ(fake_parent_->child_count(), 3);

  char name[MAX_PARTITION_NAME_LENGTH];

  // Skip the first device which is the partition manager.
  auto iter = ++fake_parent_->children().begin();

  {
    PartitionDevice* dev = (*iter)->GetDeviceContext<PartitionDevice>();
    ASSERT_OK(dev->BlockPartitionGetName(name, sizeof(name)));
    EXPECT_STREQ(name, "Linux filesystem");

    cpp20::span<const zx_device_str_prop_t> props = (*iter)->GetStringProperties();
    ASSERT_EQ(props.size(), 3);

    EXPECT_STREQ(props[0].key, "fuchsia.gpt.PartitionName");
    ASSERT_EQ(props[0].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_STRING);
    EXPECT_STREQ(props[0].property_value.data.str_val, "Linux filesystem");

    EXPECT_STREQ(props[1].key, "fuchsia.gpt.PartitionTypeGuid");
    ASSERT_EQ(props[1].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_STRING);
    EXPECT_STREQ(props[1].property_value.data.str_val, "0FC63DAF-8483-4772-8E79-3D69D8477DE4");

    EXPECT_STREQ(props[2].key, "fuchsia.block.IgnoreDevice");
    ASSERT_EQ(props[2].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_BOOL);
    EXPECT_FALSE(props[2].property_value.data.bool_val);
  }

  iter++;

  {
    PartitionDevice* dev = (*iter)->GetDeviceContext<PartitionDevice>();
    ASSERT_OK(dev->BlockPartitionGetName(name, sizeof(name)));
    EXPECT_EQ(name, kPartition1Name);

    cpp20::span<const zx_device_str_prop_t> props = (*iter)->GetStringProperties();
    ASSERT_EQ(props.size(), 3);

    EXPECT_STREQ(props[0].key, "fuchsia.gpt.PartitionName");
    ASSERT_EQ(props[0].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_STRING);
    EXPECT_STREQ(props[0].property_value.data.str_val, kPartition1Name);

    EXPECT_STREQ(props[1].key, "fuchsia.gpt.PartitionTypeGuid");
    ASSERT_EQ(props[1].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_STRING);
    EXPECT_STREQ(props[1].property_value.data.str_val, "0FC63DAF-8483-4772-8E79-3D69D8477DE4");

    EXPECT_STREQ(props[2].key, "fuchsia.block.IgnoreDevice");
    ASSERT_EQ(props[2].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_BOOL);
    EXPECT_FALSE(props[2].property_value.data.bool_val);
  }
}

TEST_F(GptDeviceTest, ValidatePartitionBindPropIgnoreDevice) {
  const fuchsia_hardware_gpt_metadata::GptInfo metadata = {{
      .partition_info = {{
          {{
              .name = "Linux filesystem",
              .options = {{.block_driver_should_ignore_device{{true}}}},
          }},
      }},
  }};

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  fake_parent_->SetMetadata(DEVICE_METADATA_GPT_INFO, encoded.value().data(),
                            encoded.value().size());

  ASSERT_OK(Bind());
  ASSERT_EQ(fake_parent_->child_count(), 3);

  // Skip the first child, which is the partition manager
  auto iter = ++fake_parent_->children().begin();

  {
    cpp20::span<const zx_device_str_prop_t> props = (*iter)->GetStringProperties();
    ASSERT_EQ(props.size(), 3);

    EXPECT_STREQ(props[2].key, "fuchsia.block.IgnoreDevice");
    ASSERT_EQ(props[2].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_BOOL);
    EXPECT_TRUE(props[2].property_value.data.bool_val);
  }

  iter++;

  {
    cpp20::span<const zx_device_str_prop_t> props = (*iter)->GetStringProperties();
    ASSERT_EQ(props.size(), 3);

    EXPECT_STREQ(props[2].key, "fuchsia.block.IgnoreDevice");
    ASSERT_EQ(props[2].property_value.data_type, ZX_DEVICE_PROPERTY_VALUE_BOOL);
    EXPECT_FALSE(props[2].property_value.data.bool_val);
  }
}

TEST_F(GptDeviceTest, ValidatePartitionGuidWithMap) {
  const fuchsia_hardware_gpt_metadata::GptInfo metadata = {{
      .partition_info = {{
          {{
              .name = "Linux filesystem",
              .options = {{
                  .type_guid_override = {{std::array<uint8_t, 16>{GUID_METADATA}}},
              }},
          }},
      }},
  }};

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  fake_parent_->SetMetadata(DEVICE_METADATA_GPT_INFO, encoded.value().data(),
                            encoded.value().size());

  ASSERT_OK(Bind());
  ASSERT_EQ(fake_parent_->child_count(), 3);

  char name[MAX_PARTITION_NAME_LENGTH];
  guid_t guid;

  // Device 0
  PartitionDevice* dev0 = GetDev(0);
  ASSERT_NOT_NULL(dev0);
  ASSERT_OK(dev0->BlockPartitionGetName(name, sizeof(name)));
  ASSERT_EQ(strcmp(name, "Linux filesystem"), 0);
  ASSERT_OK(dev0->BlockPartitionGetGuid(GUIDTYPE_TYPE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_METADATA;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }
  ASSERT_OK(dev0->BlockPartitionGetGuid(GUIDTYPE_INSTANCE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_UNIQUE_PART0;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }

  PartitionDevice* dev1 = GetDev(1);
  ASSERT_NOT_NULL(dev1);
  ASSERT_OK(dev1->BlockPartitionGetName(name, sizeof(name)));
  ASSERT_EQ(kPartition1Name, name);

  ASSERT_OK(dev1->BlockPartitionGetGuid(GUIDTYPE_TYPE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_LINUX_FILESYSTEM;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }
  ASSERT_OK(dev1->BlockPartitionGetGuid(GUIDTYPE_INSTANCE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_UNIQUE_PART1;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }
}

TEST_F(GptDeviceTest, CorruptMetadataMap) {
  const fuchsia_hardware_gpt_metadata::GptInfo metadata = {{
      .partition_info = {{
          {{
              .name = "Linux filesystem",
              .options = {{
                  .type_guid_override = {{std::array<uint8_t, 16>{GUID_METADATA}}},
              }},
          }},
      }},
  }};

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  // Set the length of the metadata to be one byte smaller than is expected, which should then fail
  // loading the driver (rather than continuing with a corrupt GUID map).
  fake_parent_->SetMetadata(DEVICE_METADATA_GPT_INFO, encoded.value().data(),
                            encoded.value().size() - 1);
  ASSERT_STATUS(ZX_ERR_INTERNAL, Bind());
}

TEST_F(GptDeviceTest, BlockOpsPropagate) {
  const fuchsia_hardware_gpt_metadata::GptInfo metadata = {{
      .partition_info = {{
          {{
              .name = "Linux filesystem",
              .options = {{
                  .type_guid_override = {{std::array<uint8_t, 16>{GUID_METADATA}}},
              }},
          }},
      }},
  }};

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  fake_parent_->SetMetadata(DEVICE_METADATA_GPT_INFO, encoded.value().data(),
                            encoded.value().size());

  ASSERT_OK(Bind());
  ASSERT_EQ(fake_parent_->child_count(), 3);

  PartitionDevice* dev0 = GetDev(0);
  PartitionDevice* dev1 = GetDev(1);

  block_info_t block_info = {};
  size_t block_op_size = 0;
  dev0->BlockImplQuery(&block_info, &block_op_size);
  EXPECT_EQ(block_op_size, sizeof(block_op_t));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(4 * block_info.block_size, 0, &vmo));

  block_op_t op = {};
  op.rw.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  op.rw.vmo = vmo.get();
  op.rw.length = 4;
  op.rw.offset_dev = 1000;

  BlockOpResult result;
  dev0->BlockImplQueue(&op, BlockOpCompleter, &result);
  sync_completion_wait(&result.completion, ZX_TIME_INFINITE);
  sync_completion_reset(&result.completion);

  EXPECT_EQ(result.op.command.opcode, BLOCK_OPCODE_READ);
  EXPECT_EQ(result.op.rw.length, 4);
  EXPECT_EQ(result.op.rw.offset_dev, 2048 + 1000);
  EXPECT_OK(result.status);

  op.rw.command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
  op.rw.vmo = vmo.get();
  op.rw.length = 4;
  op.rw.offset_dev = 5000;

  dev1->BlockImplQueue(&op, BlockOpCompleter, &result);
  sync_completion_wait(&result.completion, ZX_TIME_INFINITE);
  sync_completion_reset(&result.completion);

  EXPECT_EQ(result.op.command.opcode, BLOCK_OPCODE_WRITE);
  EXPECT_EQ(result.op.rw.length, 4);
  EXPECT_EQ(result.op.rw.offset_dev, 22528 + 5000);
  EXPECT_OK(result.status);

  op.trim.command = {.opcode = BLOCK_OPCODE_TRIM, .flags = 0};
  op.trim.length = 16;
  op.trim.offset_dev = 10000;

  dev0->BlockImplQueue(&op, BlockOpCompleter, &result);
  sync_completion_wait(&result.completion, ZX_TIME_INFINITE);
  sync_completion_reset(&result.completion);

  EXPECT_EQ(result.op.command.opcode, BLOCK_OPCODE_TRIM);
  EXPECT_EQ(result.op.trim.length, 16);
  EXPECT_EQ(result.op.trim.offset_dev, 2048 + 10000);
  EXPECT_OK(result.status);

  op.command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};

  dev1->BlockImplQueue(&op, BlockOpCompleter, &result);
  sync_completion_wait(&result.completion, ZX_TIME_INFINITE);
  sync_completion_reset(&result.completion);

  EXPECT_EQ(result.op.command.opcode, BLOCK_OPCODE_FLUSH);
  EXPECT_OK(result.status);
}

TEST_F(GptDeviceTest, BlockOpsOutOfBounds) {
  const fuchsia_hardware_gpt_metadata::GptInfo metadata = {{
      .partition_info = {{
          {{
              .name = "Linux filesystem",
              .options = {{
                  .type_guid_override = {{std::array<uint8_t, 16>{GUID_METADATA}}},
              }},
          }},
      }},
  }};

  fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());

  fake_parent_->SetMetadata(DEVICE_METADATA_GPT_INFO, encoded.value().data(),
                            encoded.value().size());

  ASSERT_OK(Bind());
  ASSERT_EQ(fake_parent_->child_count(), 3);

  PartitionDevice* dev0 = GetDev(0);
  PartitionDevice* dev1 = GetDev(1);

  block_info_t block_info = {};
  size_t block_op_size = 0;
  dev0->BlockImplQuery(&block_info, &block_op_size);
  EXPECT_EQ(block_op_size, sizeof(block_op_t));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(4 * block_info.block_size, 0, &vmo));

  block_op_t op = {};
  op.rw.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  op.rw.vmo = vmo.get();
  op.rw.length = 4;
  op.rw.offset_dev = 20481;

  BlockOpResult result;
  dev0->BlockImplQueue(&op, BlockOpCompleter, &result);
  sync_completion_wait(&result.completion, ZX_TIME_INFINITE);
  sync_completion_reset(&result.completion);

  EXPECT_NOT_OK(result.status);

  op.rw.command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
  op.rw.vmo = vmo.get();
  op.rw.length = 4;
  op.rw.offset_dev = 20478;

  dev0->BlockImplQueue(&op, BlockOpCompleter, &result);
  sync_completion_wait(&result.completion, ZX_TIME_INFINITE);
  sync_completion_reset(&result.completion);

  EXPECT_NOT_OK(result.status);

  op.trim.command = {.opcode = BLOCK_OPCODE_TRIM, .flags = 0};
  op.trim.length = 18434;
  op.trim.offset_dev = 0;

  dev1->BlockImplQueue(&op, BlockOpCompleter, &result);
  sync_completion_wait(&result.completion, ZX_TIME_INFINITE);
  sync_completion_reset(&result.completion);

  EXPECT_NOT_OK(result.status);
}

TEST_F(GptDeviceTest, GetInfo) {
  ASSERT_OK(Bind());
  fidl::WireResult result = volume_manager_fidl()->GetInfo();
  ASSERT_OK(result);
  ASSERT_OK(result.value().status);
  ASSERT_EQ(result.value().info->slice_count, kBlockCnt);
  ASSERT_EQ(result.value().info->slice_size, kBlockSz);
}

TEST_F(GptDeviceTest, AddPartition) {
  ASSERT_OK(Bind());
  fuchsia_hardware_block_partition::wire::Guid type = GUID_LINUX_FILESYSTEM;
  fuchsia_hardware_block_partition::wire::Guid instance;
  instance.value[0] = 0xff;
  fidl::WireResult result = volume_manager_fidl()->AllocatePartition(1, type, instance, "new", 0);
  ASSERT_OK(result);
  ASSERT_OK(result.value().status);

  // Ensure the changes persist.
  ASSERT_OK(Rebind());
  PartitionDevice* dev = GetDev(2);
  ASSERT_NOT_NULL(dev);

  char name[MAX_PARTITION_NAME_LENGTH];
  guid_t guid;

  ASSERT_OK(dev->BlockPartitionGetName(name, sizeof(name)));
  ASSERT_STREQ("new", name);

  ASSERT_OK(dev->BlockPartitionGetGuid(GUIDTYPE_TYPE, &guid));
  {
    uint8_t expected_guid[GPT_GUID_LEN] = GUID_LINUX_FILESYSTEM;
    EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), expected_guid, GPT_GUID_LEN);
  }
  ASSERT_OK(dev->BlockPartitionGetGuid(GUIDTYPE_INSTANCE, &guid));
  { EXPECT_BYTES_EQ(reinterpret_cast<uint8_t*>(&guid), &instance.value, GPT_GUID_LEN); }
}

TEST_F(GptDeviceTest, AddPartitionWithInvalidGuid) {
  ASSERT_OK(Bind());
  fuchsia_hardware_block_partition::wire::Guid type = {0};
  fuchsia_hardware_block_partition::wire::Guid instance;
  instance.value[0] = 0xff;
  fidl::WireResult result = volume_manager_fidl()->AllocatePartition(1, type, instance, "new", 0);
  ASSERT_OK(result);
  ASSERT_STATUS(ZX_ERR_INVALID_ARGS, result.value().status);
}

}  // namespace
}  // namespace gpt
