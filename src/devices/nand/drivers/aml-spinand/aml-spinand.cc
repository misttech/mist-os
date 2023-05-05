// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-spinand.h"

#include <fuchsia/hardware/nandinfo/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstddef>

#include <ddktl/device.h>

namespace amlspinand {

zx_status_t AmlSpiNand::Bind() {
  zx_status_t status;

  // detect the nand chip
  flash_chip_ = DetermineFlashChip();
  if (!flash_chip_.has_value()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto data_len = flash_chip_->mem_org.pagesize + flash_chip_->mem_org.oobsize;
  nand_dev_.databuf = std::make_unique<uint8_t[]>(data_len);
  nand_dev_.oobbuf = nand_dev_.databuf.get() + flash_chip_->mem_org.pagesize;
  nand_dev_.flags = flash_chip_->flags;

  status = SpiNandInitCfgCache();
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandInitCfgCache failed");
    return status;
  }
  status = SpiNandInitQuadEnable();
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandInitQuadEnable failed");
    return status;
  }
  status = SpiNandUpdateCfg(kCfgOtpEnable, 0);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandUpdateCfg failed");
    return status;
  }
  status = SpiNandWriteRegOp(kBlockLockReg, kBlockAllUnlocked);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandWriteRegOp failed");
    return status;
  }

  return DdkAdd("aml-spinand");
}

void AmlSpiNand::DdkUnbind(ddk::UnbindTxn txn) { txn.Reply(); }

zx_status_t AmlSpiNand::RawNandGetNandInfo(nand_info_t *nand_info) {
  nand_info->page_size = flash_chip_->mem_org.pagesize;
  nand_info->pages_per_block = flash_chip_->mem_org.pages_per_eraseblock;
  nand_info->num_blocks = flash_chip_->mem_org.ntargets * flash_chip_->mem_org.luns_per_target *
                          flash_chip_->mem_org.planes_per_lun *
                          flash_chip_->mem_org.eraseblocks_per_lun;
  nand_info->oob_size = flash_chip_->mem_org.oobsize;
  nand_info->ecc_bits = flash_chip_->ecc_req.strength;
  nand_info->nand_class = NAND_CLASS_PARTMAP;
  memset(&nand_info->partition_guid, 0, sizeof(nand_info->partition_guid));

  return ZX_OK;
}

zx_status_t AmlSpiNand::RawNandEraseBlock(uint32_t nand_page) {
  if (nand_page % flash_chip_->mem_org.pages_per_eraseblock) {
    zxlogf(ERROR, "NAND block %u must be a erasesize_pages (%u) multiple", nand_page,
           flash_chip_->mem_org.pages_per_eraseblock);
    return ZX_ERR_INVALID_ARGS;
  }

  zx_status_t status;
  status = SpiNandWriteEnableOp();
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandErase->SpiNandWriteEnableOp failed");
    return status;
  }

  status = SpiNandEraseOp(nand_page);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandErase->SpiNandWriteEnableOp failed");
    return status;
  }

  uint8_t result;
  status = SpiNandWait(&result);
  if (status != ZX_OK && (result & kStatusEraseFailed)) {
    zxlogf(ERROR, "SpiNandErase->SpiNandWait failed");
    status = ZX_ERR_IO;
  }

  return status;
}

zx_status_t AmlSpiNand::RawNandWritePageHwecc(const uint8_t *data, size_t data_size,
                                              const uint8_t *oob, size_t oob_size,
                                              uint32_t nand_page) {
  // size in pages
  uint32_t flash_size = flash_chip_->mem_org.ntargets * flash_chip_->mem_org.luns_per_target *
                        flash_chip_->mem_org.planes_per_lun *
                        flash_chip_->mem_org.eraseblocks_per_lun *
                        flash_chip_->mem_org.pages_per_eraseblock;
  if (nand_page > flash_size) {
    zxlogf(ERROR, "Write page 0x%x goes beyond chip size of 0x%x", nand_page, flash_size);
    return ZX_ERR_OUT_OF_RANGE;
  }

  zx_status_t status;
  // Enable ecc function
  status = SpiNandUpdateCfg(kCfgEccEnable, kCfgEccEnable);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandUpdateCfg failed");
    return status;
  }

  status = SpiNandWritePage(OobOps::kOpsAutoOob, nand_page, data, data_size, oob, oob_size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandWritePage failed");
  }

  return status;
}

zx_status_t AmlSpiNand::RawNandReadPageHwecc(uint32_t nand_page, uint8_t *data, size_t data_size,
                                             size_t *data_actual, uint8_t *oob, size_t oob_size,
                                             size_t *oob_actual, uint32_t *ecc_correct) {
  // size in pages
  uint32_t flash_size = flash_chip_->mem_org.ntargets * flash_chip_->mem_org.luns_per_target *
                        flash_chip_->mem_org.planes_per_lun *
                        flash_chip_->mem_org.eraseblocks_per_lun *
                        flash_chip_->mem_org.pages_per_eraseblock;
  if (nand_page > flash_size) {
    zxlogf(ERROR, "Read page 0x%x goes beyond chip size of 0x%x", nand_page, flash_size);
    return ZX_ERR_OUT_OF_RANGE;
  }

  zx_status_t status;
  // Enable ecc function
  status = SpiNandUpdateCfg(kCfgEccEnable, kCfgEccEnable);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandUpdateCfg failed");
    return status;
  }

  uint8_t ecc_bitflips = 0;
  status = SpiNandReadPage(OobOps::kOpsAutoOob, nand_page, data, data_size, data_actual, oob,
                           oob_size, oob_actual, &ecc_bitflips);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandReadPage failed");
    return status;
  }

  if (ecc_correct != nullptr) {
    *ecc_correct = ecc_bitflips;
  }
  return status;
}

uint32_t AmlSpiNand::PageToRow(uint32_t page_idx) {
  auto block_idx = page_idx / flash_chip_->mem_org.pages_per_eraseblock;
  auto block_offset = page_idx % flash_chip_->mem_org.pages_per_eraseblock;

  auto row = (block_idx << flash_chip_->block_addr_shift | block_offset);

  return row;
}

zx_status_t AmlSpiNand::SpiNandExecOp(const SpiOp op) {
  zx_status_t status;
  uint8_t *tx_buf = nullptr;
  uint8_t *rx_buf = nullptr;

  TransferMsg msg;
  msg.rx_mode = device_config_.rx_mode;
  msg.tx_mode = device_config_.tx_mode;

  msg.cmd = op.cmd.opcode;
  msg.addr = op.addr.val;
  msg.addr_len = op.addr.nbytes;

  if (op.placeholder.nbytes > 0) {
    msg.placeholder = true;
  } else {
    msg.placeholder = false;
  }

  if (op.data.nbytes) {
    if (op.data.dir == SpiDataDir::kSpiDataIn) {
      rx_buf = op.data.buf_in;
    } else {
      tx_buf = op.data.buf_out;
    }
  }

  status = flash_controller_->Xfer(msg, op.data.nbytes, tx_buf, rx_buf);
  if (status != ZX_OK) {
    zxlogf(ERROR, "spi transfer failed");
  }

  return status;
}

zx_status_t AmlSpiNand::SpiNandReadRegOp(uint8_t reg, uint8_t *val) {
  SpiOp op;
  zx_status_t status;

  status = SpiNandExecOp(op.GetFeature(reg, val));
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandExecOp failed");
  }

  return status;
}

zx_status_t AmlSpiNand::SpiNandWriteRegOp(uint8_t reg, uint8_t val) {
  SpiOp op;

  return SpiNandExecOp(op.SetFeature(reg, &val));
}

zx_status_t AmlSpiNand::SpiNandReadStatus(uint8_t *status) {
  return SpiNandReadRegOp(kStatusReg, status);
}

zx_status_t AmlSpiNand::SpiNandWait(uint8_t *data) {
  zx_status_t status;
  uint8_t reg_val;

  uint32_t retry = 400;
  do {
    status = SpiNandReadStatus(&reg_val);
    if (status != ZX_OK) {
      zxlogf(ERROR, "SpiNandReadStatus failed");
      return status;
    }
    if (!(reg_val & kStatusBusy)) {
      break;
    }
  } while (retry--);

  if (data != nullptr) {
    *data = reg_val;
  }

  return reg_val & kStatusBusy ? ZX_ERR_TIMED_OUT : ZX_OK;
}

zx_status_t AmlSpiNand::SpiNandResetOp() {
  zx_status_t status;

  SpiOp op;
  status = SpiNandExecOp(op.Reset());
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandExecOp failed");
    return status;
  }

  return SpiNandWait(nullptr);
}

zx_status_t AmlSpiNand::SpiNandReadIdOp(uint8_t *buf) {
  SpiOp op;
  zx_status_t status;

  status = SpiNandExecOp(op.ReadId(0, buf, kSpinandMaxIdLen));
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandExecOp(ReadId) failed");
  }

  return status;
}

zx_status_t AmlSpiNand::SpiNandInitCfgCache() {
  zx_status_t status;

  status = SpiNandReadRegOp(kCfgReg, &nand_dev_.cfg_cache);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandReadRegOp failed");
  }

  return status;
}

zx_status_t AmlSpiNand::SpiNandGetCfg(uint8_t *cfg) {
  *cfg = nand_dev_.cfg_cache;

  return ZX_OK;
}

zx_status_t AmlSpiNand::SpiNandSetCfg(uint8_t cfg) {
  zx_status_t status;

  if (nand_dev_.cfg_cache == cfg) {
    return ZX_OK;
  }

  status = SpiNandWriteRegOp(kCfgReg, cfg);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandSetCfg failed");
    return status;
  }

  nand_dev_.cfg_cache = cfg;
  return ZX_OK;
}

zx_status_t AmlSpiNand::SpiNandUpdateCfg(uint8_t mask, uint8_t val) {
  zx_status_t status;
  uint8_t cfg;

  status = SpiNandGetCfg(&cfg);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandGetCfg");
    return status;
  }

  cfg &= ~mask;
  cfg |= val;

  return SpiNandSetCfg(cfg);
}

zx_status_t AmlSpiNand::SpiNandDetect() {
  zx_status_t status;

  status = SpiNandResetOp();
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandResetOp failed");
    return status;
  }

  status = SpiNandReadIdOp(nand_dev_.id.data);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandReadIdOp failed");
    return status;
  }
  nand_dev_.id.len = kSpinandMaxIdLen;

  return status;
}

zx_status_t AmlSpiNand::SpiNandInitQuadEnable() {
  if (!(nand_dev_.flags & kSpinandHasQeBit)) {
    return ZX_OK;
  }

  bool enable = false;
  if (flash_chip_->read_op.data.buswidth == 4 || flash_chip_->write_op.data.buswidth == 4) {
    enable = true;
  }

  return SpiNandUpdateCfg(kCfgQuadEnable, enable ? kCfgQuadEnable : 0);
}

zx_status_t AmlSpiNand::SpiNandLoadPageOp(uint32_t addr) {
  auto row = PageToRow(addr);
  SpiOp op;

  return SpiNandExecOp(op.PageRead(row));
}

zx_status_t AmlSpiNand::SpiNandReadFromCacheOp(OobOps mode, uint8_t *data, size_t data_size,
                                               size_t *data_actual, uint8_t *oob, size_t oob_size,
                                               size_t *oob_actual) {
  SpiOp op = flash_chip_->read_op;
  zx_status_t status;
  size_t data_len;
  size_t oob_len;

  if (data != nullptr) {
    data_len = flash_chip_->mem_org.pagesize;
    op.data.buf_in = nand_dev_.databuf.get();
    op.data.nbytes = static_cast<uint32_t>(data_len);
  }

  if (oob != nullptr) {
    oob_len = flash_chip_->mem_org.oobsize;
    op.data.nbytes += flash_chip_->mem_org.oobsize;
    if (op.data.buf_in == nullptr) {
      op.data.buf_in = nand_dev_.oobbuf;
      op.addr.val = flash_chip_->mem_org.pagesize;
    }
  }

  status = SpiNandExecOp(op);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandReadFromCacheOp -> SpiNandExecOp failed");
    return status;
  }

  if (data != nullptr) {
    memcpy(data, nand_dev_.databuf.get(), std::min(data_size, data_len));
    if (data_actual != nullptr) {
      *data_actual = std::min(data_size, data_len);
    }
  }
  if (oob != nullptr) {
    uint32_t oob_offset = 0;
    if (mode == OobOps::kOpsAutoOob) {
      oob_offset = flash_chip_->oob_region.offset;
      oob_len = flash_chip_->oob_region.length;
    }
    memcpy(oob, nand_dev_.oobbuf + oob_offset, std::min(oob_size, oob_len));
    if (oob_actual != nullptr) {
      *oob_actual = std::min(oob_size, oob_len);
    }
  }

  return ZX_OK;
}

zx_status_t AmlSpiNand::SpiNandWriteEnableOp() {
  SpiOp op;

  return SpiNandExecOp(op.WrEnable(true));
}

zx_status_t AmlSpiNand::SpiNandWriteToCacheOp(OobOps mode, const uint8_t *data, size_t data_size,
                                              const uint8_t *oob, size_t oob_size) {
  SpiOp op = flash_chip_->write_op;
  size_t data_len = flash_chip_->mem_org.pagesize;
  size_t oob_len = flash_chip_->mem_org.oobsize;
  memset(nand_dev_.databuf.get(), 0xff, data_len + oob_len);

  if (data != nullptr) {
    memcpy(nand_dev_.databuf.get(), data, std::min(data_size, data_len));
    op.data.buf_out = nand_dev_.databuf.get();
    op.data.nbytes = static_cast<uint32_t>(data_len);
  }

  if (oob != nullptr) {
    uint32_t oob_offset = 0;
    if (mode == OobOps::kOpsAutoOob) {
      oob_offset = flash_chip_->oob_region.offset;
      oob_len = flash_chip_->oob_region.length;
    }
    memcpy(nand_dev_.oobbuf + oob_offset, oob, std::min(oob_size, oob_len));
    op.data.nbytes += flash_chip_->mem_org.oobsize;
    if (op.data.buf_out == nullptr) {
      op.data.buf_out = nand_dev_.oobbuf;
      op.addr.val = flash_chip_->mem_org.pagesize;
    }
  }

  zx_status_t status = SpiNandExecOp(op);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandWriteToCacheOp->SpiMemExecOp failed");
  }

  return status;
}

zx_status_t AmlSpiNand::SpiNandProgramOp(uint32_t addr) {
  auto row = PageToRow(addr);
  SpiOp op;

  return SpiNandExecOp(op.ProgExec(row));
}

zx_status_t AmlSpiNand::SpiNandEraseOp(uint32_t addr) {
  auto row = PageToRow(addr);
  SpiOp op;

  return SpiNandExecOp(op.BlockErase(row));
}

zx_status_t AmlSpiNand::SpiNandGetEccBitflips(uint8_t status, uint8_t *out_bitflips) {
  switch (status & kStatusEccMask) {
    case kStatusEccNoBitflips:
      *out_bitflips = 0;
      break;
    case kStatusEccHasBitflips:
      *out_bitflips = 7;
      break;
    case kToshibaStatusEcc8Bitflips:
      *out_bitflips = 8;
      break;
    case kStatusEccUncorrectErr:
      return ZX_ERR_BAD_STATE;
    default:
      return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t AmlSpiNand::SpiNandWritePage(OobOps mode, uint32_t nand_page, const uint8_t *data,
                                         size_t data_size, const uint8_t *oob, size_t oob_size) {
  zx_status_t status;

  status = SpiNandWriteEnableOp();
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandWriteEnableOp failed");
    return status;
  }

  status = SpiNandWriteToCacheOp(mode, data, data_size, oob, oob_size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandWriteEnableOp failed");
    return status;
  }
  status = SpiNandProgramOp(nand_page);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandWriteEnableOp failed");
    return status;
  }
  uint8_t result;
  status = SpiNandWait(&result);
  if (status != ZX_OK && (result & kStatusProgFailed)) {
    zxlogf(ERROR, "SpiNandWriteEnableOp failed");
    status = ZX_ERR_IO;
  }

  return status;
}

zx_status_t AmlSpiNand::SpiNandReadPage(OobOps mode, uint32_t nand_page, uint8_t *data,
                                        size_t data_size, size_t *data_actual, uint8_t *oob,
                                        size_t oob_size, size_t *oob_actual,
                                        uint8_t *out_bitflips) {
  zx_status_t status = SpiNandLoadPageOp(nand_page);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandLoadPageOp failed");
    return status;
  }

  uint8_t result;
  status = SpiNandWait(&result);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandWait failed");
    return status;
  }

  status = SpiNandReadFromCacheOp(mode, data, data_size, data_actual, oob, oob_size, oob_actual);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandReadFromCacheOp failed");
    return status;
  }

  return SpiNandGetEccBitflips(result, out_bitflips);
}

std::optional<FlashChipInfo> AmlSpiNand::DetermineFlashChip() {
  zx_status_t status = SpiNandDetect();
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiNandDetect failed");
    return std::nullopt;
  }

  zxlogf(INFO, "Found nand device with vendor: 0x%x device: 0x%x", nand_dev_.id.data[0],
         nand_dev_.id.data[1]);
  for (const auto &device : kFlashDevices) {
    if (device.vendor_id == nand_dev_.id.data[0] && device.device_id == nand_dev_.id.data[1]) {
      zxlogf(INFO, "%s %s SPI NAND was found", device.name.data(), device.model.data());
      return device;
    }
  }

  return std::nullopt;
}

void AmlSpiNand::PlatDataInit(uint32_t speed, uint32_t tx_bus_width, uint32_t rx_bus_width) {
  SpiRxMode rx_mode = SpiRxMode::kSpiRxDefault;
  SpiTxMode tx_mode = SpiTxMode::kSpiTxDefault;

  device_config_.max_hz = speed;
  flash_controller_->SetSpeed(device_config_.max_hz);

  switch (tx_bus_width) {
    case 1:
      break;
    case 2:
      tx_mode = SpiTxMode::kSpiTxDual;
      break;
    case 4:
      tx_mode = SpiTxMode::kSpiTxQuad;
      break;
    default:
      zxlogf(INFO, "spi tx_bus_width %d not supported", tx_bus_width);
      break;
  }

  switch (rx_bus_width) {
    case 1:
      break;
    case 2:
      rx_mode = SpiRxMode::kSpiRxDual;
      break;
    case 4:
      rx_mode = SpiRxMode::kSpiRxQuad;
      break;
    default:
      zxlogf(INFO, "spi rx_bus_width %d not supported", rx_bus_width);
      break;
  }

  device_config_.tx_mode = tx_mode;
  device_config_.rx_mode = rx_mode;
}

zx_status_t AmlSpiNand::Init() {
  // init the controller
  flash_controller_->Init();
  // init the configuration data
  PlatDataInit(kDefaultSpiNandFreq, kDefaultTxBusWidth, kDefaultRXBusWidth);

  return ZX_OK;
}

zx_status_t AmlSpiNand::Create(void *ctx, zx_device_t *parent) {
  zx_status_t status = ZX_OK;

  ddk::PDevFidl pdev(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "ZX_PROTOCOL_PDEV not available");
    return ZX_ERR_NO_RESOURCES;
  }

  std::optional<fdf::MmioBuffer> controller_mmio;
  status = pdev.MapMmio(0, &controller_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio spifc reg failed");
    return status;
  }

  std::optional<fdf::MmioBuffer> clk_mmio;
  status = pdev.MapMmio(1, &clk_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio nand reg failed");
    return status;
  }

  auto spifc = std::make_unique<AmlSpiFlashController>(std::move(controller_mmio.value()),
                                                       std::move(clk_mmio.value()));

  auto spinand = std::make_unique<AmlSpiNand>(parent, std::move(spifc));
  status = spinand->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "AML spi nand init failed");
    return status;
  }
  status = spinand->Bind();
  if (status != ZX_OK) {
    zxlogf(ERROR, "AML spi nand bind failed");
    return status;
  }

  [[maybe_unused]] auto *unused = spinand.release();
  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AmlSpiNand::Create;
  return ops;
}();
}  // namespace amlspinand

// clang-format off
ZIRCON_DRIVER(aml-spinand, amlspinand::driver_ops, "zircon", "0.1");
