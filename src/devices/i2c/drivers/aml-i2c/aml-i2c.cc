// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-i2c.h"

#ifdef DFV1
#include <lib/ddk/debug.h>  // nogncheck
#else
#include <lib/driver/compat/cpp/logging.h>  // nogncheck
#endif

#include <lib/trace/event.h>
#include <zircon/errors.h>
#include <zircon/threads.h>

#include <soc/aml-common/aml-i2c.h>

#include "aml-i2c-regs.h"

namespace {

constexpr zx_signals_t kErrorSignal = ZX_USER_SIGNAL_0;
constexpr zx_signals_t kTxnCompleteSignal = ZX_USER_SIGNAL_1;

constexpr size_t kMaxTransferSize = 512;

}  // namespace

namespace aml_i2c {

void AmlI2c::SetTargetAddr(uint16_t addr) const {
  addr &= 0x7f;
  TargetAddr::Get().ReadFrom(&regs_iobuff_).set_target_address(addr).WriteTo(&regs_iobuff_);
}

int AmlI2c::IrqThread() const {
  while (true) {
    zx_status_t status = irq_.wait(nullptr);
    if (status == ZX_ERR_CANCELED) {
      break;
    }
    if (status != ZX_OK) {
      zxlogf(DEBUG, "interrupt error: %s", zx_status_get_string(status));
      continue;
    }
    if (Control::Get().ReadFrom(&regs_iobuff_).error()) {
      event_.signal(0, kErrorSignal);
    } else {
      event_.signal(0, kTxnCompleteSignal);
    }
  }
  return ZX_OK;
}

#if 0
zx_status_t AmlI2c::DumpState() {
  printf("control reg      : %08x\n", regs_iobuff_.Read32(kControlReg));
  printf("target addr reg  : %08x\n", regs_iobuff_.Read32(kTargetAddrReg));
  printf("token list0 reg  : %08x\n", regs_iobuff_.Read32(kTokenList0Reg));
  printf("token list1 reg  : %08x\n", regs_iobuff_.Read32(kTokenList1Reg));
  printf("token wdata0     : %08x\n", regs_iobuff_.Read32(kWriteData0Reg));
  printf("token wdata1     : %08x\n", regs_iobuff_.Read32(kWriteData1Reg));
  printf("token rdata0     : %08x\n", regs_iobuff_.Read32(kReadData0Reg));
  printf("token rdata1     : %08x\n", regs_iobuff_.Read32(kReadData1Reg));

  return ZX_OK;
}
#endif

void AmlI2c::StartXfer() const {
  // First have to clear the start bit before setting (RTFM)
  Control::Get()
      .ReadFrom(&regs_iobuff_)
      .set_start(0)
      .WriteTo(&regs_iobuff_)
      .set_start(1)
      .WriteTo(&regs_iobuff_);
}

zx_status_t AmlI2c::WaitTransferComplete() const {
  constexpr zx_signals_t kSignalMask = kTxnCompleteSignal | kErrorSignal;

  uint32_t observed;
  zx_status_t status = event_.wait_one(kSignalMask, zx::deadline_after(timeout_), &observed);
  if (status != ZX_OK) {
    return status;
  }
  event_.signal(observed, 0);
  if (observed & kErrorSignal) {
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

zx_status_t AmlI2c::Write(cpp20::span<uint8_t> src, const bool stop) const {
  TRACE_DURATION("i2c", "aml-i2c Write");
  ZX_DEBUG_ASSERT(src.size() <= kMaxTransferSize);

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrWr);

  auto remaining = src.size();
  auto offset = 0;
  while (remaining > 0) {
    const bool is_last_iter = remaining <= WriteData::kMaxWriteBytesPerTransfer;
    const size_t tx_size = is_last_iter ? remaining : WriteData::kMaxWriteBytesPerTransfer;
    for (uint32_t i = 0; i < tx_size; i++) {
      tokens.Push(TokenList::Token::kData);
    }

    if (is_last_iter && stop) {
      tokens.Push(TokenList::Token::kStop);
    }

    tokens.WriteTo(&regs_iobuff_);

    WriteData wdata = WriteData::Get().FromValue(0);
    for (uint32_t i = 0; i < tx_size; i++) {
      wdata.Push(src[offset + i]);
    }

    wdata.WriteTo(&regs_iobuff_);

    StartXfer();
    // while (Control::Get().ReadFrom(&regs_iobuff_).status()) ;;    // wait for idle
    zx_status_t status = WaitTransferComplete();
    if (status != ZX_OK) {
      return status;
    }

    remaining -= tx_size;
    offset += tx_size;
  }

  return ZX_OK;
}

zx_status_t AmlI2c::Read(cpp20::span<uint8_t> dst, const bool stop) const {
  ZX_DEBUG_ASSERT(dst.size() <= kMaxTransferSize);
  TRACE_DURATION("i2c", "aml-i2c Read");

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrRd);

  size_t remaining = dst.size();
  size_t offset = 0;
  while (remaining > 0) {
    const bool is_last_iter = remaining <= ReadData::kMaxReadBytesPerTransfer;
    const size_t rx_size = is_last_iter ? remaining : ReadData::kMaxReadBytesPerTransfer;

    for (uint32_t i = 0; i < (rx_size - 1); i++) {
      tokens.Push(TokenList::Token::kData);
    }
    if (is_last_iter) {
      tokens.Push(TokenList::Token::kDataLast);
      if (stop) {
        tokens.Push(TokenList::Token::kStop);
      }
    } else {
      tokens.Push(TokenList::Token::kData);
    }

    tokens.WriteTo(&regs_iobuff_);

    // clear registers to prevent data leaking from last xfer
    ReadData rdata = ReadData::Get().FromValue(0).WriteTo(&regs_iobuff_);

    StartXfer();

    zx_status_t status = WaitTransferComplete();
    if (status != ZX_OK) {
      return status;
    }

    // while (Control::Get().ReadFrom(&regs_iobuff_).status()) ;;    // wait for idle

    rdata.ReadFrom(&regs_iobuff_);

    for (size_t i = 0; i < rx_size; i++) {
      dst[offset + i] = rdata.Pop();
    }

    remaining -= rx_size;
    offset += rx_size;
  }

  return ZX_OK;
}

void AmlI2c::StartIrqThread() {
  thrd_create_with_name(
      &irqthrd_, [](void* ctx) { return reinterpret_cast<AmlI2c*>(ctx)->IrqThread(); }, this,
      "i2c_irq_thread");
}

zx_status_t AmlI2c::SetClockDelay(const aml_i2c_delay_values& delay,
                                  const fdf::MmioBuffer& regs_iobuff) {
  if (delay.quarter_clock_delay > Control::kQtrClkDlyMax ||
      delay.clock_low_delay > TargetAddr::kSclLowDelayMax) {
    zxlogf(ERROR, "invalid clock delay");
    return ZX_ERR_INVALID_ARGS;
  }

  if (delay.quarter_clock_delay > 0) {
    Control::Get()
        .ReadFrom(&regs_iobuff)
        .set_qtr_clk_dly(delay.quarter_clock_delay)
        .WriteTo(&regs_iobuff);
  }

  if (delay.clock_low_delay > 0) {
    TargetAddr::Get()
        .FromValue(0)
        .set_scl_low_dly(delay.clock_low_delay)
        .set_use_cnt_scl_low(1)
        .WriteTo(&regs_iobuff);
  }

  return ZX_OK;
}

void AmlI2c::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_i2cimpl::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(ERROR, "Unknown method %lu", metadata.method_ordinal);
}

fuchsia_hardware_i2cimpl::Service::InstanceHandler AmlI2c::GetI2cImplInstanceHandler(
    fdf_dispatcher_t* dispatcher) {
  return fuchsia_hardware_i2cimpl::Service::InstanceHandler(
      {.device = i2cimpl_bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure)});
}

void AmlI2c::GetMaxTransferSize(fdf::Arena& arena, GetMaxTransferSizeCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(kMaxTransferSize);
}

void AmlI2c::SetBitrate(SetBitrateRequestView request, fdf::Arena& arena,
                        SetBitrateCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void AmlI2c::Transact(TransactRequestView request, fdf::Arena& arena,
                      TransactCompleter::Sync& completer) {
  TRACE_DURATION("i2c", "aml-i2c Transact");
  for (const auto& op : request->op) {
    if ((op.type.is_read_size() && op.type.read_size() > kMaxTransferSize) ||
        (op.type.is_write_data() && op.type.write_data().count() > kMaxTransferSize)) {
      completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
  }

  std::vector<fuchsia_hardware_i2cimpl::wire::ReadData> reads;
  for (const auto& op : request->op) {
    SetTargetAddr(op.address);

    zx_status_t status;
    if (op.type.is_read_size()) {
      if (op.type.read_size() > 0) {
        auto dst = fidl::VectorView<uint8_t>{arena, op.type.read_size()};
        status = Read(dst.get(), op.stop);
        reads.push_back({dst});
      } else {
        // Avoid allocating an empty vector because allocating 0 bytes causes an asan error.
        status = Read({}, op.stop);
        reads.push_back({});
      }
    } else {
      status = Write(op.type.write_data().get(), op.stop);
    }
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
  }

  if (reads.empty()) {
    // Avoid allocating an empty vector because allocating 0 bytes causes an asan error.
    completer.buffer(arena).ReplySuccess({});
  } else {
    completer.buffer(arena).ReplySuccess({arena, reads});
  }
}

zx::result<std::unique_ptr<AmlI2c>> AmlI2c::Create(ddk::PDevFidl& pdev,
                                                   const aml_i2c_delay_values& delay) {
  pdev_device_info_t info;
  zx_status_t status = pdev.GetDeviceInfo(&info);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_get_device_info failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  if (info.mmio_count != 1 || info.irq_count != 1) {
    zxlogf(ERROR, "Invalid mmio_count (%u) or irq_count (%u)", info.mmio_count, info.irq_count);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::optional<fdf::MmioBuffer> regs_iobuff;
  status = pdev.MapMmio(0, &regs_iobuff);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_map_mmio_buffer failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = SetClockDelay(delay, *regs_iobuff);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  zx::interrupt irq;
  status = pdev.GetInterrupt(0, 0, &irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_get_interrupt failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx::event event;
  status = zx::event::create(0, &event);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx_event_create failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  auto aml_i2c =
      std::make_unique<AmlI2c>(std::move(irq), std::move(event), *std::move(regs_iobuff));
  return zx::ok(std::move(aml_i2c));
}

}  // namespace aml_i2c
