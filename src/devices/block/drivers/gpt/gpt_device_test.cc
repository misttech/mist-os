// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpt.metadata/cpp/fidl.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <fuchsia/hardware/block/partition/cpp/banjo.h>
#include <inttypes.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>
#include <zircon/hw/gpt.h>

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
  FakeBlockDevice() : proto_({&block_protocol_ops_, this}) {
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
    if (opcode == BLOCK_OPCODE_WRITE) {
      return ZX_OK;
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

  size_t part_size = sizeof(test_partition_table);
  size_t read_off = op->rw.offset_dev * bsize;
  size_t read_len = op->rw.length * bsize;

  if (read_len == 0) {
    return ZX_OK;
  }

  uint64_t vmo_addr = op->rw.offset_vmo * bsize;

  // Read initial part from header if in range.
  if (read_off < part_size) {
    size_t part_read_len = part_size - read_off;
    if (part_read_len > read_len) {
      part_read_len = read_len;
    }
    zx_vmo_write(op->rw.vmo, test_partition_table + read_off, vmo_addr, part_read_len);

    read_len -= part_read_len;
    read_off += part_read_len;
    vmo_addr += part_read_len;

    if (read_len == 0) {
      return ZX_OK;
    }
  }

  std::unique_ptr<uint8_t[]> zbuf(new uint8_t[bsize]);
  memset(zbuf.get(), 0, bsize);
  // Zero-fill remaining.
  for (; read_len > 0; read_len -= bsize) {
    zx_vmo_write(op->rw.vmo, zbuf.get(), vmo_addr, bsize);
    vmo_addr += bsize;
  }
  return ZX_OK;
}

class GptDeviceTest : public zxtest::Test {
 public:
  GptDeviceTest() = default;

  DISALLOW_COPY_ASSIGN_AND_MOVE(GptDeviceTest);

  void SetInfo(const block_info_t* info) { fake_block_device_.SetInfo(info); }

  void Init() {
    fake_parent_->AddProtocol(ZX_PROTOCOL_BLOCK, fake_block_device_.proto()->ops,
                              fake_block_device_.proto()->ctx);
  }
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();

  PartitionDevice* GetDev(size_t device_number) {
    ZX_ASSERT(fake_parent_->child_count() > device_number);
    auto iter = std::next(fake_parent_->children().begin(), device_number);
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
  FakeBlockDevice fake_block_device_;
};

TEST_F(GptDeviceTest, DeviceTooSmall) {
  Init();

  const block_info_t info = {20, 512, fuchsia_hardware_block::wire::kMaxTransferUnbounded, 0};
  SetInfo(&info);

  ASSERT_EQ(ZX_ERR_NO_SPACE, Bind(nullptr, fake_parent_.get()));
}

TEST_F(GptDeviceTest, ValidatePartitionGuid) {
  Init();
  ASSERT_OK(Bind(nullptr, fake_parent_.get()));
  ASSERT_EQ(fake_parent_->child_count(), 2);

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
  Init();
  ASSERT_OK(Bind(nullptr, fake_parent_.get()));
  ASSERT_EQ(fake_parent_->child_count(), 2);

  char name[MAX_PARTITION_NAME_LENGTH];

  auto iter = fake_parent_->children().begin();

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
  Init();

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

  ASSERT_OK(Bind(nullptr, fake_parent_.get()));
  ASSERT_EQ(fake_parent_->child_count(), 2);

  auto iter = fake_parent_->children().begin();

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
  Init();

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

  ASSERT_OK(Bind(nullptr, fake_parent_.get()));
  ASSERT_EQ(fake_parent_->child_count(), 2);

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
  Init();

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
  ASSERT_EQ(ZX_ERR_INTERNAL, Bind(nullptr, fake_parent_.get()));
}

TEST_F(GptDeviceTest, BlockOpsPropagate) {
  Init();

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

  ASSERT_OK(Bind(nullptr, fake_parent_.get()));
  ASSERT_EQ(fake_parent_->child_count(), 2);

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
  Init();

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

  ASSERT_OK(Bind(nullptr, fake_parent_.get()));
  ASSERT_EQ(fake_parent_->child_count(), 2);

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

}  // namespace
}  // namespace gpt
