// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-spi.h"

#include <endian.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fpromise/bridge.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <string.h>
#include <threads.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <memory>

#include <bind/fuchsia/hardware/spiimpl/cpp/bind.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

#include "registers.h"

namespace spi {

constexpr char kDefaultRoleName[] = "fuchsia.devices.spi.drivers.aml-spi.transaction";

constexpr size_t kNelsonRadarBurstSize = 23224;

// The TX and RX buffer size to allocate for DMA (only if a BTI is provided). This value is set to
// support the Selina driver on Nelson.
constexpr size_t kDmaBufferSize = fbl::round_up<size_t, size_t>(kNelsonRadarBurstSize, PAGE_SIZE);

constexpr size_t kFifoSizeWords = 16;

constexpr size_t kReset6RegisterOffset = 0x1c;
constexpr uint32_t kSpi0ResetMask = 1 << 1;
constexpr uint32_t kSpi1ResetMask = 1 << 6;

#define dump_reg(reg) FDF_LOG(ERROR, "%-21s (+%02x): %08x", #reg, reg, mmio_.Read32(reg))

void AmlSpi::DumpState() {
  // skip registers with side-effects
  // dump_reg(AML_SPI_RXDATA);
  // dump_reg(AML_SPI_TXDATA);
  dump_reg(AML_SPI_CONREG);
  dump_reg(AML_SPI_INTREG);
  dump_reg(AML_SPI_DMAREG);
  dump_reg(AML_SPI_STATREG);
  dump_reg(AML_SPI_PERIODREG);
  dump_reg(AML_SPI_TESTREG);
  dump_reg(AML_SPI_DRADDR);
  dump_reg(AML_SPI_DWADDR);
  dump_reg(AML_SPI_LD_CNTL0);
  dump_reg(AML_SPI_LD_CNTL1);
  dump_reg(AML_SPI_LD_RADDR);
  dump_reg(AML_SPI_LD_WADDR);
  dump_reg(AML_SPI_ENHANCE_CNTL);
  dump_reg(AML_SPI_ENHANCE_CNTL1);
  dump_reg(AML_SPI_ENHANCE_CNTL2);
}

#undef dump_reg

bool SpiRequest::Start(SpiVmoStore& vmo_store) && {
  // Populate the buffers just before starting the request. They may depend on registered VMOs
  // that weren't available at the time this request was enqueued. Does nothing for requests that
  // do not have data.
  if (zx_status_t status = PopulateBuffers(vmo_store); status != ZX_OK) {
    Complete(status);
    return true;
  }
  auto start_cb = std::move(start_);
  return start_cb(std::move(*this));
}

zx_status_t SpiRequest::PopulateBuffers(SpiVmoStore& vmo_store) {
  zx::result<cpp20::span<const uint8_t>> tx_buffer =
      GetBuffer(vmo_store, tx_buffer_, fuchsia_hardware_sharedmemory::SharedVmoRight::kRead);
  if (tx_buffer.is_error()) {
    return tx_buffer.error_value();
  }

  zx::result<cpp20::span<uint8_t>> rx_buffer =
      GetBuffer(vmo_store, rx_buffer_, fuchsia_hardware_sharedmemory::SharedVmoRight::kWrite);
  if (rx_buffer.is_error()) {
    return rx_buffer.error_value();
  }

  txdata_ = fidl::VectorView<const uint8_t>::FromExternal(tx_buffer->data(), tx_buffer->size());
  rxdata_ = fidl::VectorView<uint8_t>::FromExternal(rx_buffer->data(), rx_buffer->size());

  return ZX_OK;
}

zx::result<cpp20::span<uint8_t>> SpiRequest::GetBuffer(
    SpiVmoStore& vmo_store, Buffer& buffer, fuchsia_hardware_sharedmemory::SharedVmoRight right) {
  if (std::holds_alternative<std::vector<uint8_t>>(buffer)) {
    return zx::ok(cpp20::span{std::get<std::vector<uint8_t>>(buffer).data(),
                              std::get<std::vector<uint8_t>>(buffer).size()});
  }
  if (std::holds_alternative<fuchsia_hardware_sharedmemory::wire::SharedVmoBuffer>(buffer)) {
    auto& vmo_buffer = std::get<fuchsia_hardware_sharedmemory::wire::SharedVmoBuffer>(buffer);

    vmo_store::StoredVmo<OwnedVmoInfo>* const vmo_info = vmo_store.GetVmo(vmo_buffer.vmo_id);
    if (!vmo_info) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }

    if (!(vmo_info->meta().rights & right)) {
      return zx::error(ZX_ERR_ACCESS_DENIED);
    }

    if (vmo_buffer.offset + vmo_buffer.size > vmo_info->meta().size) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }

    return zx::ok(
        vmo_info->data().subspan(vmo_info->meta().offset + vmo_buffer.offset, vmo_buffer.size));
  }
  return zx::ok(cpp20::span<uint8_t>{});
}

void AmlSpi::Exchange8(const uint8_t* txdata, uint8_t* out_rxdata, size_t size) {
  // transfer settings
  auto conreg = ConReg::Get().ReadFrom(&mmio_).set_bits_per_word(CHAR_BIT - 1).WriteTo(&mmio_);

  while (size > 0) {
    // Burst size in words (with one byte per word).
    const uint32_t burst_size = static_cast<uint32_t>(std::min(kFifoSizeWords, size));

    // fill fifo
    if (txdata) {
      for (uint32_t i = 0; i < burst_size; i++) {
        mmio_.Write32(txdata[i], AML_SPI_TXDATA);
      }
      txdata += burst_size;
    } else {
      for (uint32_t i = 0; i < burst_size; i++) {
        mmio_.Write32(UINT8_MAX, AML_SPI_TXDATA);
      }
    }

    // start burst
    StatReg::Get().FromValue(0).set_tc(1).WriteTo(&mmio_);
    conreg.set_burst_length(burst_size - 1).set_xch(1).WriteTo(&mmio_);

    WaitForTransferComplete();

    // The RX FIFO may not be full immediately after receiving the transfer complete interrupt.
    // Poll until the FIFO has at least one word that can be read.
    for (uint32_t i = 0; i < burst_size; i++) {
      while (StatReg::Get().ReadFrom(&mmio_).rx_fifo_empty()) {
      }

      const uint8_t data = mmio_.Read32(AML_SPI_RXDATA) & 0xff;
      if (out_rxdata) {
        out_rxdata[i] = data;
      }
    }

    if (out_rxdata) {
      out_rxdata += burst_size;
    }

    size -= burst_size;
  }
}

void AmlSpi::Exchange64(const uint8_t* txdata, uint8_t* out_rxdata, size_t size) {
  constexpr size_t kBytesPerWord = sizeof(uint64_t);
  constexpr size_t kMaxBytesPerBurst = kBytesPerWord * kFifoSizeWords;

  auto conreg = ConReg::Get()
                    .ReadFrom(&mmio_)
                    .set_bits_per_word((kBytesPerWord * CHAR_BIT) - 1)
                    .WriteTo(&mmio_);

  while (size >= kBytesPerWord) {
    // Burst size in 64-bit words.
    const uint32_t burst_size_words =
        static_cast<uint32_t>(std::min(kMaxBytesPerBurst, size) / kBytesPerWord);

    if (txdata) {
      const uint64_t* tx = reinterpret_cast<const uint64_t*>(txdata);
      for (uint32_t i = 0; i < burst_size_words; i++) {
        uint64_t value;
        memcpy(&value, &tx[i], sizeof(value));
        value = be64toh(value);
        // The controller interprets each FIFO entry as a number when they are actually just
        // bytes. To make sure the bytes come out in the intended order, treat them as big-endian,
        // and convert to little-endian for the controller.
        mmio_.Write32(value >> 32, AML_SPI_TXDATA);
        mmio_.Write32(value & UINT32_MAX, AML_SPI_TXDATA);
      }
      txdata += burst_size_words * kBytesPerWord;
    } else {
      for (uint32_t i = 0; i < burst_size_words; i++) {
        mmio_.Write32(UINT32_MAX, AML_SPI_TXDATA);
        mmio_.Write32(UINT32_MAX, AML_SPI_TXDATA);
      }
    }

    StatReg::Get().FromValue(0).set_tc(1).WriteTo(&mmio_);
    conreg.set_burst_length(burst_size_words - 1).set_xch(1).WriteTo(&mmio_);

    WaitForTransferComplete();

    // Same as Exchange8 -- poll until the FIFO has a word that can be read.
    for (uint32_t i = 0; i < burst_size_words; i++) {
      while (StatReg::Get().ReadFrom(&mmio_).rx_fifo_empty()) {
      }

      uint64_t value = mmio_.Read32(AML_SPI_RXDATA);
      value = (value << 32) | mmio_.Read32(AML_SPI_RXDATA);
      value = be64toh(value);

      if (out_rxdata) {
        memcpy(reinterpret_cast<uint64_t*>(out_rxdata) + i, &value, sizeof(value));
      }
    }

    if (out_rxdata) {
      out_rxdata += burst_size_words * kBytesPerWord;
    }

    size -= burst_size_words * kBytesPerWord;
  }

  Exchange8(txdata, out_rxdata, size);
}

void AmlSpi::WaitForTransferComplete() {
  auto statreg = StatReg::Get().FromValue(0);
  while (!statreg.ReadFrom(&mmio_).tc()) {
    interrupt_.wait(nullptr);
  }

  statreg.WriteTo(&mmio_);
}

void AmlSpi::WaitForDmaTransferComplete() {
  auto statreg = StatReg::Get().FromValue(0);
  while (!statreg.te()) {
    interrupt_.wait(nullptr);
    // Clear the transfer complete bit (all others are read-only).
    statreg.set_reg_value(0).set_tc(1).WriteTo(&mmio_).ReadFrom(&mmio_);
  }

  // Wait for the enable bit in DMAREG to be cleared. The TX FIFO empty interrupt apparently
  // indicates this, however in some cases enable is still set after receiving it. Returning
  // without waiting for enable to be cleared leads to data loss, so just poll after the interrupt
  // to make sure.
  while (DmaReg::Get().ReadFrom(&mmio_).enable()) {
  }
}

void AmlSpi::InitRegisters() {
  ConReg::Get().FromValue(0).WriteTo(&mmio_);

  TestReg::Get().FromValue(0).set_dlyctl(config_.delay_control).set_clk_free_en(1).WriteTo(&mmio_);

  ConReg::Get()
      .ReadFrom(&mmio_)
      .set_data_rate(config_.use_enhanced_clock_mode ? 0 : config_.clock_divider_register_value)
      .set_drctl(0)
      .set_ssctl(0)
      .set_smc(0)
      .set_xch(0)
      .set_mode(ConReg::kModeMaster)
      .WriteTo(&mmio_);

  auto enhance_cntl = EnhanceCntl::Get().FromValue(0);
  if (config_.use_enhanced_clock_mode) {
    enhance_cntl.set_clk_cs_delay_enable(1)
        .set_cs_oen_enhance_enable(1)
        .set_clk_oen_enhance_enable(1)
        .set_mosi_oen_enhance_enable(1)
        .set_spi_clk_select(1)  // Use this register instead of CONREG.
        .set_enhance_clk_div(config_.clock_divider_register_value)
        .set_clk_cs_delay(0);
  }
  enhance_cntl.WriteTo(&mmio_);

  EnhanceCntl1::Get().FromValue(0).WriteTo(&mmio_);

  ConReg::Get().ReadFrom(&mmio_).set_en(1).WriteTo(&mmio_);
}

bool AmlSpi::HandleRequest(SpiRequest request) {
  if (shutdown_) {
    request.Cancel();
    return true;
  }

  if (current_request_) {
    request_queue_.push(std::move(request));
    return false;
  }

  return std::move(request).Start(*registered_vmos(request.cs()));
}

void AmlSpi::ServiceRequestQueue() {
  ZX_DEBUG_ASSERT_MSG(!current_request_, "Current request set when servicing the queue");

  // Nothing to do if we are already servicing the queue.
  if (executing_synchronous_request_) {
    return;
  }

  executing_synchronous_request_ = true;

  for (bool request_completed = true; !request_queue_.empty() && request_completed;) {
    request_completed = HandleRequest(std::move(request_queue_.front()));
    request_queue_.pop();
  }

  // A request is now executing asynchronously, or there are no requests left.
  executing_synchronous_request_ = false;
}

bool AmlSpi::StartExchange(SpiRequest request) {
  ZX_DEBUG_ASSERT_MSG(!request.txdata().is_null() || !request.rxdata().is_null(),
                      "txdata and rxdata are both null");
  ZX_DEBUG_ASSERT_MSG(request.txdata().empty() || request.rxdata().empty() ||
                          request.txdata().count() == request.rxdata().count(),
                      "txdata and rxdata have different sizes");

  current_request_.emplace(std::move(request));

  const bool use_dma = UseDma(current_request_->size());

  // There seems to be a hardware issue where transferring an odd number of bytes corrupts the TX
  // FIFO, but only for subsequent transfers that use 64-bit words. Resetting the IP avoids the
  // problem. DMA transfers do not seem to be affected.
  if (need_reset_ && reset_ && !use_dma && current_request_->size() >= sizeof(uint64_t)) {
    reset_->WriteRegister32(kReset6RegisterOffset, reset_mask_, reset_mask_)
        .Then([this](auto& result) mutable {
          if (!result.ok() || result.value().is_error()) {
            FDF_LOG(WARNING, "Failed to reset SPI controller");
          }

          InitRegisters();  // The registers must be reinitialized after resetting the IP.
          need_reset_ = false;

          AssertCs();
        });
    return false;
  }

  // reset both fifos
  auto testreg = TestReg::Get().ReadFrom(&mmio_).set_fiforst(3).WriteTo(&mmio_);
  do {
    testreg.ReadFrom(&mmio_);
  } while ((testreg.rxcnt() != 0) || (testreg.txcnt() != 0));

  // Resetting seems to leave an extra word in the RX FIFO, so do an extra read just in case.
  mmio_.Read32(AML_SPI_RXDATA);
  mmio_.Read32(AML_SPI_RXDATA);

  return AssertCs();
}

bool AmlSpi::AssertCs() {
  ZX_DEBUG_ASSERT_MSG(current_request_, "Current request not set");
  ZX_DEBUG_ASSERT_MSG(current_request_->cs() < chips_.size(), "Invalid chip select");

  IntReg::Get().FromValue(0).set_tcen(1).WriteTo(&mmio_);

  if (gpio(current_request_->cs()).is_valid()) {
    gpio(current_request_->cs())
        ->SetBufferMode(fuchsia_hardware_gpio::BufferMode::kOutputLow)
        .Then([this](auto& result) mutable {
          ZX_ASSERT_MSG(result.ok(), "error: %s", result.FormatDescription().c_str());
          ZX_ASSERT_MSG(result->is_ok(), "error: %s", zx_status_get_string(result->error_value()));
          Exchange();
        });
    return false;
  }

  return Exchange();
}

bool AmlSpi::Exchange() {
  ZX_DEBUG_ASSERT_MSG(current_request_, "Current request not set");
  ZX_DEBUG_ASSERT_MSG(current_request_->cs() < chips_.size(), "Invalid chip select");

  zx_status_t status = ZX_OK;

  if (UseDma(current_request_->size())) {
    status = ExchangeDma(current_request_->txdata().data(), current_request_->rxdata().data(),
                         current_request_->size());
  } else if (reset_) {
    // Only use 64-bit words if we will be able to reset the controller.
    Exchange64(current_request_->txdata().data(), current_request_->rxdata().data(),
               current_request_->size());
  } else {
    Exchange8(current_request_->txdata().data(), current_request_->rxdata().data(),
              current_request_->size());
  }

  IntReg::Get().FromValue(0).WriteTo(&mmio_);

  if (current_request_->size() % 2 == 1) {
    need_reset_ = true;
  }

  if (gpio(current_request_->cs()).is_valid()) {
    gpio(current_request_->cs())
        ->SetBufferMode(fuchsia_hardware_gpio::BufferMode::kOutputHigh)
        .Then([this, status](auto& result) mutable {
          ZX_ASSERT_MSG(result.ok(), "error: %s", result.FormatDescription().c_str());
          ZX_ASSERT_MSG(result->is_ok(), "error: %s", zx_status_get_string(result->error_value()));
          CompleteExchange(status);
        });
    return false;
  }

  CompleteExchange(status);
  return true;
}

void AmlSpi::CompleteExchange(zx_status_t status) {
  ZX_DEBUG_ASSERT_MSG(current_request_, "Current request not set");

  std::exchange(current_request_, {})->Complete(status);

  if (prepare_stop_completer_) {
    // We were waiting for this transfer to complete in order to continue shutdown.
    (*prepare_stop_completer_)(zx::ok());
  } else {
    ServiceRequestQueue();
  }
}

void AmlSpi::TransmitVector(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVectorRequest* request,
                            fdf::Arena& arena, TransmitVectorCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  std::vector<uint8_t> txdata(request->data.cbegin(), request->data.cend());

  SpiRequest::StartCallback start_cb = fit::bind_member<&AmlSpi::StartExchange>(this);
  SpiRequest::CompleteCallback complete_cb =
      [completer = completer.ToAsync()](const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');
        completer.buffer(arena).Reply(zx::make_result(status));
      };
  SpiRequest spi_request{request->chip_select, std::move(txdata), std::nullopt, std::move(start_cb),
                         std::move(complete_cb)};

  HandleRequest(std::move(spi_request));
}

void AmlSpi::ReceiveVector(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVectorRequest* request,
                           fdf::Arena& arena, ReceiveVectorCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  SpiRequest::StartCallback start_cb = fit::bind_member<&AmlSpi::StartExchange>(this);
  SpiRequest::CompleteCallback complete_cb =
      [completer = completer.ToAsync()](const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');
        if (status == ZX_OK) {
          completer.buffer(arena).ReplySuccess(request.rxdata());
        } else {
          completer.buffer(arena).ReplyError(status);
        }
      };
  SpiRequest spi_request{request->chip_select, std::nullopt, std::vector<uint8_t>(request->size),
                         std::move(start_cb), std::move(complete_cb)};

  HandleRequest(std::move(spi_request));
}

void AmlSpi::ExchangeVector(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVectorRequest* request,
                            fdf::Arena& arena, ExchangeVectorCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  std::vector<uint8_t> txdata(request->txdata.cbegin(), request->txdata.cend());

  SpiRequest::StartCallback start_cb = fit::bind_member<&AmlSpi::StartExchange>(this);
  SpiRequest::CompleteCallback complete_cb =
      [completer = completer.ToAsync()](const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');
        if (status == ZX_OK) {
          completer.buffer(arena).ReplySuccess(request.rxdata());
        } else {
          completer.buffer(arena).ReplyError(status);
        }
      };
  SpiRequest spi_request{request->chip_select, std::move(txdata),
                         std::vector<uint8_t>(request->txdata.count()), std::move(start_cb),
                         std::move(complete_cb)};

  HandleRequest(std::move(spi_request));
}

void AmlSpi::RegisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplRegisterVmoRequest* request,
                         fdf::Arena& arena, RegisterVmoCompleter::Sync& completer) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  if (request->rights.has_unknown_bits()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }

  const bool vmo_right_read = (request->rights & SharedVmoRight::kRead) == SharedVmoRight::kRead;
  const bool vmo_right_write = (request->rights & SharedVmoRight::kWrite) == SharedVmoRight::kWrite;

  vmo_store::StoredVmo<OwnedVmoInfo> stored_vmo(std::move(request->vmo.vmo),
                                                OwnedVmoInfo{
                                                    .offset = request->vmo.offset,
                                                    .size = request->vmo.size,
                                                    .rights = request->rights,
                                                });
  const zx_vm_option_t map_opts =
      (vmo_right_read ? ZX_VM_PERM_READ : 0) | (vmo_right_write ? ZX_VM_PERM_WRITE : 0);
  zx_status_t status = stored_vmo.Map(map_opts);
  if (status != ZX_OK) {
    return completer.buffer(arena).ReplyError(status);
  }

  SpiRequest::CompleteCallback complete_cb =
      [this, vmo_id = request->vmo_id, stored_vmo = std::move(stored_vmo),
       completer = completer.ToAsync()](const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');

        if (status != ZX_OK) {
          return completer.buffer(arena).ReplyError(status);
        }

        status = registered_vmos(request.cs())->RegisterWithKey(vmo_id, std::move(stored_vmo));
        completer.buffer(arena).Reply(zx::make_result(status));
      };

  // RegisterVmo, UnregisterVmo, and ReleaseRegisteredVmos requests are put on the queue as well.
  // They may reference VMOs used by transfers earlier in the queue, so we have to wait for them to
  // complete first.
  HandleRequest(SpiRequest{request->chip_select, std::move(complete_cb)});
}

void AmlSpi::UnregisterVmo(fuchsia_hardware_spiimpl::wire::SpiImplUnregisterVmoRequest* request,
                           fdf::Arena& arena, UnregisterVmoCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  SpiRequest::CompleteCallback complete_cb =
      [this, vmo_id = request->vmo_id, completer = completer.ToAsync()](
          const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');

        if (status != ZX_OK) {
          return completer.buffer(arena).ReplyError(status);
        }

        vmo_store::StoredVmo<OwnedVmoInfo>* const vmo_info =
            registered_vmos(request.cs())->GetVmo(vmo_id);
        if (!vmo_info) {
          return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
        }

        zx::vmo out_vmo;
        status = vmo_info->vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &out_vmo);
        if (status != ZX_OK) {
          return completer.buffer(arena).ReplyError(status);
        }

        auto result = registered_vmos(request.cs())->Unregister(vmo_id);
        if (result.is_error()) {
          completer.buffer(arena).Reply(result.take_error());
        } else {
          completer.buffer(arena).ReplySuccess(std::move(out_vmo));
        }
      };

  HandleRequest(SpiRequest{request->chip_select, std::move(complete_cb)});
}

void AmlSpi::ReleaseRegisteredVmos(
    fuchsia_hardware_spiimpl::wire::SpiImplReleaseRegisteredVmosRequest* request, fdf::Arena& arena,
    ReleaseRegisteredVmosCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return;
  }

  SpiRequest::CompleteCallback complete_cb = [this](const SpiRequest& request, zx_status_t status) {
    if (status == ZX_OK) {
      registered_vmos(request.cs()) = std::make_unique<SpiVmoStore>(vmo_store::Options{});
    }
  };

  HandleRequest(SpiRequest{request->chip_select, std::move(complete_cb)});
}

void AmlSpi::TransmitVmo(fuchsia_hardware_spiimpl::wire::SpiImplTransmitVmoRequest* request,
                         fdf::Arena& arena, TransmitVmoCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  SpiRequest::StartCallback start_cb = fit::bind_member<&AmlSpi::StartExchange>(this);
  SpiRequest::CompleteCallback complete_cb =
      [completer = completer.ToAsync()](const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');
        completer.buffer(arena).Reply(zx::make_result(status));
      };
  SpiRequest spi_request{request->chip_select, request->buffer, std::nullopt, std::move(start_cb),
                         std::move(complete_cb)};

  HandleRequest(std::move(spi_request));
}

void AmlSpi::ReceiveVmo(fuchsia_hardware_spiimpl::wire::SpiImplReceiveVmoRequest* request,
                        fdf::Arena& arena, ReceiveVmoCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  SpiRequest::StartCallback start_cb = fit::bind_member<&AmlSpi::StartExchange>(this);
  SpiRequest::CompleteCallback complete_cb =
      [completer = completer.ToAsync()](const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');
        completer.buffer(arena).Reply(zx::make_result(status));
      };
  SpiRequest spi_request{request->chip_select, std::nullopt, request->buffer, std::move(start_cb),
                         std::move(complete_cb)};

  HandleRequest(std::move(spi_request));
}

void AmlSpi::ExchangeVmo(fuchsia_hardware_spiimpl::wire::SpiImplExchangeVmoRequest* request,
                         fdf::Arena& arena, ExchangeVmoCompleter::Sync& completer) {
  if (request->chip_select >= chips_.size()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  if (request->tx_buffer.size != request->rx_buffer.size) {
    return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }

  SpiRequest::StartCallback start_cb = fit::bind_member<&AmlSpi::StartExchange>(this);
  SpiRequest::CompleteCallback complete_cb =
      [completer = completer.ToAsync()](const SpiRequest& request, zx_status_t status) mutable {
        fdf::Arena arena('SPII');
        completer.buffer(arena).Reply(zx::make_result(status));
      };
  SpiRequest spi_request{request->chip_select, request->tx_buffer, request->rx_buffer,
                         std::move(start_cb), std::move(complete_cb)};

  HandleRequest(std::move(spi_request));
}

zx_status_t AmlSpi::ExchangeDma(const uint8_t* txdata, uint8_t* out_rxdata, uint64_t size) {
  constexpr size_t kBytesPerWord = sizeof(uint64_t);

  if (txdata) {
    if (config_.client_reverses_dma_transfers) {
      memcpy(tx_buffer_.mapped.start(), txdata, size);
    } else {
      // Copy the TX data into the pinned VMO and reverse the endianness.
      auto* tx_vmo = static_cast<uint64_t*>(tx_buffer_.mapped.start());
      for (size_t offset = 0; offset < size; offset += kBytesPerWord) {
        uint64_t tmp;
        memcpy(&tmp, &txdata[offset], sizeof(tmp));
        *tx_vmo++ = be64toh(tmp);
      }
    }
  } else {
    memset(tx_buffer_.mapped.start(), 0xff, size);
  }

  zx_status_t status = tx_buffer_.vmo.op_range(ZX_VMO_OP_CACHE_CLEAN, 0, size, nullptr, 0);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to clean cache: %s", zx_status_get_string(status));
    return status;
  }

  if (out_rxdata) {
    status = rx_buffer_.vmo.op_range(ZX_VMO_OP_CACHE_CLEAN, 0, size, nullptr, 0);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to clean cache: %s", zx_status_get_string(status));
      return status;
    }
  }

  ConReg::Get().ReadFrom(&mmio_).set_bits_per_word((kBytesPerWord * CHAR_BIT) - 1).WriteTo(&mmio_);

  const fzl::PinnedVmo::Region tx_region = tx_buffer_.pinned.region(0);
  const fzl::PinnedVmo::Region rx_region = rx_buffer_.pinned.region(0);

  mmio_.Write32(static_cast<uint32_t>(tx_region.phys_addr), AML_SPI_DRADDR);
  mmio_.Write32(static_cast<uint32_t>(rx_region.phys_addr), AML_SPI_DWADDR);
  mmio_.Write32(0, AML_SPI_PERIODREG);

  DmaReg::Get().FromValue(0).WriteTo(&mmio_);

  // The SPI controller issues requests to DDR to fill the TX FIFO/drain the RX FIFO. The reference
  // driver uses requests up to the FIFO size (16 words) when that many words are remaining, or 2-8
  // word requests otherwise. 16-word requests didn't seem to work in testing, and only 8-word
  // requests are used by default here for simplicity.
  for (size_t words_remaining = size / kBytesPerWord; words_remaining > 0;) {
    const size_t transfer_size = DoDmaTransfer(words_remaining);

    // Enable the TX FIFO empty interrupt and set the start mode control bit on the first run
    // through the loop.
    if (words_remaining == (size / kBytesPerWord)) {
      IntReg::Get().FromValue(0).set_teen(1).WriteTo(&mmio_);
      ConReg::Get().ReadFrom(&mmio_).set_smc(1).WriteTo(&mmio_);
    }

    WaitForDmaTransferComplete();

    words_remaining -= transfer_size;
  }

  DmaReg::Get().ReadFrom(&mmio_).set_enable(0).WriteTo(&mmio_);
  IntReg::Get().FromValue(0).WriteTo(&mmio_);
  LdCntl0::Get().FromValue(0).WriteTo(&mmio_);
  ConReg::Get().ReadFrom(&mmio_).set_smc(0).WriteTo(&mmio_);

  if (out_rxdata) {
    status = rx_buffer_.vmo.op_range(ZX_VMO_OP_CACHE_CLEAN_INVALIDATE, 0, size, nullptr, 0);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to invalidate cache: %s", zx_status_get_string(status));
      return status;
    }

    if (config_.client_reverses_dma_transfers) {
      memcpy(out_rxdata, rx_buffer_.mapped.start(), size);
    } else {
      const auto* rx_vmo = static_cast<uint64_t*>(rx_buffer_.mapped.start());
      for (size_t offset = 0; offset < size; offset += kBytesPerWord) {
        uint64_t tmp = htobe64(*rx_vmo++);
        memcpy(&out_rxdata[offset], &tmp, sizeof(tmp));
      }
    }
  }

  return ZX_OK;
}

size_t AmlSpi::DoDmaTransfer(size_t words_remaining) {
  // These are the limits used by the reference driver, although request sizes up to the FIFO size
  // should work, and the read/write counters are 16 bits wide.
  constexpr size_t kDefaultRequestSizeWords = 8;
  constexpr size_t kMaxRequestCount = 0xfff;

  // TODO(https://fxbug.dev/42051588): It may be possible to complete the transfer in fewer
  // iterations by using request sizes 2-7 instead of 8, like the reference driver does.
  const size_t request_size =
      words_remaining < kFifoSizeWords ? words_remaining : kDefaultRequestSizeWords;
  const size_t request_count = std::min(words_remaining / request_size, kMaxRequestCount);

  LdCntl0::Get().FromValue(0).set_read_counter_enable(1).set_write_counter_enable(1).WriteTo(
      &mmio_);
  LdCntl1::Get()
      .FromValue(0)
      .set_dma_read_counter(static_cast<uint32_t>(request_count))
      .set_dma_write_counter(static_cast<uint32_t>(request_count))
      .WriteTo(&mmio_);

  DmaReg::Get()
      .FromValue(0)
      .set_enable(1)
      // No explanation for these -- see the reference driver.
      .set_urgent(1)
      .set_txfifo_threshold(static_cast<uint32_t>(kFifoSizeWords + 1 - request_size))
      .set_read_request_burst_size(static_cast<uint32_t>(request_size - 1))
      .set_rxfifo_threshold(static_cast<uint32_t>(request_size - 1))
      .set_write_request_burst_size(static_cast<uint32_t>(request_size - 1))
      .WriteTo(&mmio_);

  return request_size * request_count;
}

bool AmlSpi::UseDma(size_t size) const {
  // TODO(https://fxbug.dev/42051588): Support DMA transfers greater than the pre-allocated buffer
  // size.
  return size % sizeof(uint64_t) == 0 && size <= tx_buffer_.mapped.size() &&
         size <= rx_buffer_.mapped.size();
}

fbl::Array<AmlSpi::ChipInfo> AmlSpiDriver::InitChips(const amlogic_spi::amlspi_config_t& config) {
  fbl::Array<AmlSpi::ChipInfo> chips(new AmlSpi::ChipInfo[config.cs_count], config.cs_count);
  if (!chips) {
    return chips;
  }

  for (uint32_t i = 0; i < config.cs_count; i++) {
    chips[i].registered_vmos = std::make_unique<SpiVmoStore>(vmo_store::Options{});

    uint32_t index = config.cs[i];
    if (index == amlogic_spi::amlspi_config_t::kCsClientManaged) {
      continue;
    }

    char fragment_name[32] = {};
    snprintf(fragment_name, 32, "gpio-cs-%d", index);
    zx::result client = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(fragment_name);
    if (client.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to GPIO device: %s", client.status_string());
      return fbl::Array<AmlSpi::ChipInfo>();
    }
    chips[i].gpio =
        fidl::WireClient(std::move(*client), fdf::Dispatcher::GetCurrent()->async_dispatcher());
  }

  return chips;
}

void AmlSpiDriver::Start(fdf::StartCompleter completer) {
  parent_.Bind(std::move(node()), dispatcher());

  {
    zx::result pdev_client =
        incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
    if (pdev_client.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_client.status_string());
      return completer(pdev_client.take_error());
    }

    pdev_.Bind(*std::move(pdev_client), dispatcher());
  }

  {
    zx::result compat_client = incoming()->Connect<fuchsia_driver_compat::Service::Device>("pdev");
    if (compat_client.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to compat: %s", compat_client.status_string());
      return completer(compat_client.take_error());
    }

    compat_.Bind(*std::move(compat_client), dispatcher());
  }

  fpromise::bridge<> bridge;

  auto task = compat::GetMetadataAsync<fuchsia_scheduler::RoleName>(
      fdf::Dispatcher::GetCurrent()->async_dispatcher(), incoming(),
      DEVICE_METADATA_SCHEDULER_ROLE_NAME,
      [this, start_completer = std::move(completer), completer = std::move(bridge.completer)](
          const zx::result<fuchsia_scheduler::RoleName>& result) mutable {
        OnGetSchedulerRoleName(std::move(start_completer), result);
        completer.complete_ok();
      },
      "pdev");

  auto promise = bridge.consumer.promise().then(
      [task = std::move(task)](fpromise::result<>& result) mutable {});
  executor_.schedule_task(std::move(promise));
}

void AmlSpiDriver::OnGetSchedulerRoleName(
    fdf::StartCompleter completer,
    const zx::result<fuchsia_scheduler::RoleName>& scheduler_role_name) {
  std::unordered_set<compat::MetadataKey> forward_metadata({DEVICE_METADATA_SPI_CHANNELS});

  std::vector<uint8_t> role_name;

  // If we have scheduler role metadata, forward it to our child and let them apply it with the
  // expectation that their calls to us will be inlined. If we don't have scheduler role metadata,
  // then add the default role name as metadata instead.
  if (scheduler_role_name.is_ok()) {
    forward_metadata.emplace(DEVICE_METADATA_SCHEDULER_ROLE_NAME);
  } else {
    const fuchsia_scheduler::RoleName role{kDefaultRoleName};

    fit::result result = fidl::Persist(role);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to persist scheduler role: %s",
              result.error_value().FormatDescription().c_str());
      return completer(zx::error(result.error_value().status()));
    }

    role_name = *std::move(result);
  }

  compat_server_.Begin(
      incoming(), outgoing(), node_name(), component::kDefaultInstance,
      [this, completer = std::move(completer),
       role_name = std::move(role_name)](zx::result<> result) mutable {
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to initialize compat server: %s", result.status_string());
          return completer(result.take_error());
        }

        if (!role_name.empty()) {
          zx_status_t status = compat_server_.inner().AddMetadata(
              DEVICE_METADATA_SCHEDULER_ROLE_NAME, role_name.data(), role_name.size());
          if (status != ZX_OK) {
            FDF_LOG(ERROR, "Failed to add role metadata: %s", zx_status_get_string(status));
            return completer(zx::error(status));
          }
        }

        OnCompatServerInitialized(std::move(completer));
      },
      compat::ForwardMetadata::Some(std::move(forward_metadata)));
}

void AmlSpiDriver::OnCompatServerInitialized(fdf::StartCompleter completer) {
  auto task =
      fpromise::join_promises(MapMmio(pdev_, 0), GetConfig(), GetInterrupt(), GetBti(),
                              GetSpiBusMetadata())
          .then([this, completer = std::move(completer)](
                    fpromise::result<std::tuple<
                        fpromise::result<fdf::MmioBuffer, zx_status_t>,
                        fpromise::result<amlogic_spi::amlspi_config_t, zx_status_t>,
                        fpromise::result<zx::interrupt, zx_status_t>,
                        fpromise::result<zx::bti, zx_status_t>,
                        fpromise::result<std::optional<std::vector<uint8_t>>, zx_status_t>>>&
                        results) mutable {
            if (results.is_error()) {
              FDF_LOG(ERROR, "Failed to get resources");
              return completer(zx::error(ZX_ERR_INTERNAL));
            }

            if (std::get<0>(results.value()).is_error()) {
              return completer(zx::error(std::get<0>(results.value()).error()));
            }
            auto& mmio = std::get<0>(results.value()).value();

            if (std::get<1>(results.value()).is_error()) {
              return completer(zx::error(std::get<1>(results.value()).error()));
            }
            const auto& config = std::get<1>(results.value()).value();

            if (std::get<2>(results.value()).is_error()) {
              return completer(zx::error(std::get<2>(results.value()).error()));
            }
            zx::interrupt interrupt = std::move(std::get<2>(results.value()).value());

            zx::bti bti{};
            if (std::get<3>(results.value()).is_ok()) {
              bti = std::move(std::get<3>(results.value()).value());
            }

            fpromise::result encoded_spi_bus_metadata = std::get<4>(results.value());
            if (encoded_spi_bus_metadata.is_error()) {
              return completer(zx::error(encoded_spi_bus_metadata.error()));
            }

            AddNode(std::move(mmio), config, std::move(interrupt), std::move(bti),
                    std::move(encoded_spi_bus_metadata.value()), std::move(completer));
          });
  executor_.schedule_task(std::move(task));
}

void AmlSpiDriver::AddNode(fdf::MmioBuffer mmio, const amlogic_spi::amlspi_config_t& config,
                           zx::interrupt interrupt, zx::bti bti,
                           std::optional<std::vector<uint8_t>> encoded_spi_bus_metadata,
                           fdf::StartCompleter completer) {
  // Setup SPI bus metadata server.
  {
    if (encoded_spi_bus_metadata.has_value()) {
      auto metadata = fidl::Unpersist<fuchsia_hardware_spi_businfo::SpiBusMetadata>(
          encoded_spi_bus_metadata.value());
      if (metadata.is_error()) {
        FDF_LOG(ERROR, "Failed to unpersist encoded SPI bus metadata: %s",
                metadata.error_value().status_string());
        return completer(zx::error(metadata.error_value().status()));
      }
      if (zx::result result = metadata_server_.SetMetadata(metadata.value()); result.is_error()) {
        FDF_LOG(ERROR, "Failed to set metadata for metadata server: %s", result.status_string());
        return completer(result.take_error());
      }
    }
    if (zx::result result = metadata_server_.Serve(*outgoing(), dispatcher()); result.is_error()) {
      FDF_LOG(ERROR, "Failed to serve metadata server: %s", result.status_string());
      return completer(result.take_error());
    }
  }

  // Stop DMA in case the driver is restarting and didn't shut down cleanly.
  DmaReg::Get().FromValue(0).WriteTo(&mmio);
  ConReg::Get().FromValue(0).WriteTo(&mmio);

  const uint32_t max_clock_div_reg_value =
      config.use_enhanced_clock_mode ? EnhanceCntl::kEnhanceClkDivMax : ConReg::kDataRateMax;
  if (config.clock_divider_register_value > max_clock_div_reg_value) {
    FDF_LOG(ERROR, "Metadata clock divider value is too large: %u",
            config.clock_divider_register_value);
    return completer(zx::error(ZX_ERR_INVALID_ARGS));
  }

  zx::result reset_register_client =
      incoming()->Connect<fuchsia_hardware_registers::Service::Device>("reset");
  if (reset_register_client.is_error()) {
    FDF_LOG(WARNING, "Did not bind the reset register client.");
  }

  AmlSpi::DmaBuffer tx_buffer, rx_buffer;
  // Supplying a BTI is optional.
  if (bti.is_valid()) {
    // DMA was stopped above, so it's safe to release any quarantined pages.
    bti.release_quarantine();

    zx_status_t status = AmlSpi::DmaBuffer::Create(bti, kDmaBufferSize, &tx_buffer);
    if (status != ZX_OK) {
      return completer(zx::error(status));
    }
    status = AmlSpi::DmaBuffer::Create(bti, kDmaBufferSize, &rx_buffer);
    if (status != ZX_OK) {
      return completer(zx::error(status));
    }
    FDF_LOG(DEBUG, "Got BTI and contiguous buffers, DMA may be used");
  }

  fbl::Array<AmlSpi::ChipInfo> chips = InitChips(config);
  if (!chips) {
    return completer(zx::error(ZX_ERR_NO_RESOURCES));
  }
  if (chips.empty()) {
    return completer(zx::ok());
  }

  const uint32_t reset_mask =
      config.bus_id == 0 ? kSpi0ResetMask : (config.bus_id == 1 ? kSpi1ResetMask : 0);

  fbl::AllocChecker ac;
  device_.reset(new (&ac) AmlSpi(std::move(mmio), *std::move(reset_register_client), reset_mask,
                                 std::move(chips), std::move(interrupt), config, std::move(bti),
                                 std::move(tx_buffer), std::move(rx_buffer)));
  if (!ac.check()) {
    return completer(zx::error(ZX_ERR_NO_MEMORY));
  }

  device_->InitRegisters();

  {
    fuchsia_hardware_spiimpl::Service::InstanceHandler handler({
        .device = fit::bind_member<&AmlSpi::Serve>(device_.get()),
    });
    auto result = outgoing()->AddService<fuchsia_hardware_spiimpl::Service>(std::move(handler));
    if (result.is_error()) {
      FDF_LOG(ERROR, "AddService failed: %s", result.status_string());
      return completer(zx::error(result.error_value()));
    }
  }

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return completer(controller_endpoints.take_error());
  }

  controller_.Bind(std::move(controller_endpoints->client), dispatcher());

  char devname[32];
  sprintf(devname, "aml-spi-%u", config.bus_id);

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia_hardware_spiimpl::SERVICE,
                                    bind_fuchsia_hardware_spiimpl::SERVICE_DRIVERTRANSPORT);

  std::vector offers = compat_server_.CreateOffers2(arena);
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_spiimpl::Service>(arena, component::kDefaultInstance));
  offers.push_back(metadata_server_.MakeOffer(arena));
  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, devname)
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  parent_->AddChild(args, std::move(controller_endpoints->server), {})
      .Then([completer = std::move(completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to add child failed: %s", result.status_string());
          return completer(zx::error(result.status()));
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to add child");
          return completer(zx::error(ZX_ERR_INTERNAL));
        }
        completer(zx::ok());
      });
}

fpromise::promise<amlogic_spi::amlspi_config_t, zx_status_t> AmlSpiDriver::GetConfig() {
  fpromise::bridge<amlogic_spi::amlspi_config_t, zx_status_t> bridge;

  auto task = compat::GetMetadataAsync<amlogic_spi::amlspi_config_t>(
      fdf::Dispatcher::GetCurrent()->async_dispatcher(), incoming(), DEVICE_METADATA_AMLSPI_CONFIG,
      [completer = std::move(bridge.completer)](
          zx::result<std::unique_ptr<amlogic_spi::amlspi_config_t>> result) mutable {
        if (result.is_ok()) {
          completer.complete_ok(**result);
        } else {
          FDF_LOG(ERROR, "Failed to get metadata: %s", zx_status_get_string(result.error_value()));
          completer.complete_error(result.error_value());
        }
      },
      "pdev");

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_INTERNAL))
      .then([task = std::move(task)](
                fpromise::result<amlogic_spi::amlspi_config_t, zx_status_t>& result) mutable {
        return result;
      });
}

fpromise::promise<zx::interrupt, zx_status_t> AmlSpiDriver::GetInterrupt() {
  fpromise::bridge<zx::interrupt, zx_status_t> bridge;

  pdev_->GetInterruptById(0, 0).Then(
      [completer = std::move(bridge.completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to get SPI interrupt failed: %s", result.status_string());
          completer.complete_error(result.status());
        } else if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to get SPI interrupt: %s",
                  zx_status_get_string(result->error_value()));
          completer.complete_error(result->error_value());
        } else {
          completer.complete_ok(std::move(result->value()->irq));
        }
      });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_INTERNAL));
}

fpromise::promise<zx::bti, zx_status_t> AmlSpiDriver::GetBti() {
  fpromise::bridge<zx::bti, zx_status_t> bridge;

  pdev_->GetBtiById(0).Then([completer = std::move(bridge.completer)](auto& result) mutable {
    if (!result.ok()) {
      completer.complete_error(result.status());
    } else if (result->is_error()) {
      completer.complete_error(result->error_value());
    } else {
      completer.complete_ok(std::move(result->value()->bti));
    }
  });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_INTERNAL));
}

fpromise::promise<std::optional<std::vector<uint8_t>>, zx_status_t>
AmlSpiDriver::GetSpiBusMetadata() {
  fpromise::bridge<std::optional<std::vector<uint8_t>>, zx_status_t> bridge;

  pdev_->GetMetadata(fuchsia_hardware_spi_businfo::SpiBusMetadata::kSerializableName)
      .Then([completer = std::move(bridge.completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Failed to send GetMetadata request: %s", result.status_string());
          completer.complete_error(result.status());
          return;
        }

        if (result->is_error()) {
          if (result->error_value() == ZX_ERR_NOT_FOUND) {
            completer.complete_ok(std::nullopt);
            return;
          }
          FDF_LOG(ERROR, "Failed to get metadata: %s", zx_status_get_string(result->error_value()));
          completer.complete_error(result->error_value());
        }

        auto& metadata = result->value()->metadata;
        completer.complete_ok({{metadata.begin(), metadata.end()}});
      });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_INTERNAL));
}

zx_status_t AmlSpi::DmaBuffer::Create(const zx::bti& bti, size_t size, DmaBuffer* out_dma_buffer) {
  zx_status_t status = zx::vmo::create_contiguous(bti, size, 0, &out_dma_buffer->vmo);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create DMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  status = out_dma_buffer->pinned.Pin(out_dma_buffer->vmo, bti,
                                      ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE | ZX_BTI_CONTIGUOUS);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to pin DMA VMO: %s", zx_status_get_string(status));
    return status;
  }
  if (out_dma_buffer->pinned.region_count() != 1) {
    FDF_LOG(ERROR, "Invalid region count for contiguous VMO: %u",
            out_dma_buffer->pinned.region_count());
    return status;
  }

  status = out_dma_buffer->mapped.Map(out_dma_buffer->vmo);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map DMA VMO: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

void AmlSpi::PrepareStop(fdf::PrepareStopCompleter completer) {
  ZX_DEBUG_ASSERT_MSG(!prepare_stop_completer_, "Prepare stop completer already set");

  // Prevent new connections and requests.
  shutdown_ = true;

  bindings_.RemoveAll();

  // Cancel any requests on the queue.
  while (!request_queue_.empty()) {
    request_queue_.front().Cancel();
    request_queue_.pop();
  }

  // If there is a transfer in progress, set the stop completer so that it can be called after the
  // transfer finishes. Otherwise immediately call the completer.
  if (current_request_) {
    prepare_stop_completer_.emplace(std::move(completer));
  } else {
    completer(zx::ok());
  }
}

void AmlSpi::Stop() {
  ZX_DEBUG_ASSERT_MSG(!current_request_, "Transfer pending during stop hook");
  ZX_DEBUG_ASSERT_MSG(request_queue_.empty(), "Request queue not empty during stop hook");

  // There are no more pending requests -- stop DMA and disable the controller.
  DmaReg::Get().FromValue(0).WriteTo(&mmio_);
  ConReg::Get().FromValue(0).WriteTo(&mmio_);

  // DMA has been stopped, manually unbind in case destructors aren't run.
  tx_buffer_.pinned.Unpin();
  rx_buffer_.pinned.Unpin();
}

void AmlSpi::Serve(fdf::ServerEnd<fuchsia_hardware_spiimpl::SpiImpl> request) {
  if (shutdown_) {
    request.channel().close();
    return;
  }

  bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->get(), std::move(request), this,
                       fidl::kIgnoreBindingClosure);
}

void AmlSpi::OnAllClientsUnbound() {
  // Cancel any requests on the queue.
  while (!request_queue_.empty()) {
    request_queue_.front().Cancel();
    request_queue_.pop();
  }

  // If there's a transfer in progress, stash its registered VMOs to be released after the transfer
  // completes.
  if (current_request_) {
    current_request_->ReleaseVmosOnComplete(std::move(registered_vmos(current_request_->cs())));
  }

  // Release the rest of the registered VMOs.
  for (auto& chip : chips_) {
    chip.registered_vmos = std::make_unique<SpiVmoStore>(vmo_store::Options{});
  }
}

fpromise::promise<fdf::MmioBuffer, zx_status_t> AmlSpiDriver::MapMmio(
    fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id) {
  fpromise::bridge<fdf::MmioBuffer, zx_status_t> bridge;

  pdev->GetMmioById(mmio_id).Then(
      [mmio_id, completer = std::move(bridge.completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to get MMIO %u failed: %s", mmio_id, result.status_string());
          return completer.complete_error(result.status());
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to get MMIO %u: %s", mmio_id,
                  zx_status_get_string(result->error_value()));
          return completer.complete_error(result->error_value());
        }

        auto& mmio = *result->value();
        if (!mmio.has_offset() || !mmio.has_size() || !mmio.has_vmo()) {
          FDF_LOG(ERROR, "Invalid MMIO returned for ID %u", mmio_id);
          return completer.complete_error(ZX_ERR_BAD_STATE);
        }

        zx::result mmio_buffer = fdf::MmioBuffer::Create(
            mmio.offset(), mmio.size(), std::move(mmio.vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
        if (mmio_buffer.is_error()) {
          FDF_LOG(ERROR, "Failed to map MMIO %u: %s", mmio_id,
                  zx_status_get_string(mmio_buffer.error_value()));
          return completer.complete_error(mmio_buffer.error_value());
        }

        completer.complete_ok(*std::move(mmio_buffer));
      });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_BAD_STATE));
}

}  // namespace spi

FUCHSIA_DRIVER_EXPORT(spi::AmlSpiDriver);
