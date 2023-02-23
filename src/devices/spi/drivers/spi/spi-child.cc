// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi-child.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/trace-engine/types.h>

#include <variant>

#include <ddktl/fidl.h>
#include <fbl/vector.h>

namespace spi {

void SpiChild::OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) {
  Bind(dispatcher_, std::move(request->session));
}

void SpiChild::TransmitVector(TransmitVectorRequestView request,
                              TransmitVectorCompleter::Sync& completer) {
  size_t rx_actual;
  zx_status_t status =
      spi_.Exchange(cs_, request->data.data(), request->data.count(), nullptr, 0, &rx_actual);
  if (status == ZX_OK) {
    completer.Reply(ZX_OK);
  } else {
    completer.Reply(status);
  }
}

void SpiChild::ReceiveVector(ReceiveVectorRequestView request,
                             ReceiveVectorCompleter::Sync& completer) {
  fbl::Vector<uint8_t> rxdata;
  rxdata.reserve(request->size);
  size_t rx_actual;
  zx_status_t status = spi_.Exchange(cs_, nullptr, 0, rxdata.begin(), request->size, &rx_actual);
  if (status == ZX_OK && rx_actual == request->size) {
    auto rx_vector = fidl::VectorView<uint8_t>::FromExternal(rxdata.data(), request->size);
    completer.Reply(ZX_OK, rx_vector);
  } else {
    completer.Reply(status == ZX_OK ? ZX_ERR_INTERNAL : status, fidl::VectorView<uint8_t>());
  }
}

void SpiChild::ExchangeVector(ExchangeVectorRequestView request,
                              ExchangeVectorCompleter::Sync& completer) {
  fbl::Vector<uint8_t> rxdata;
  const size_t size = request->txdata.count();
  rxdata.reserve(size);
  size_t rx_actual;
  zx_status_t status =
      spi_.Exchange(cs_, request->txdata.data(), size, rxdata.begin(), size, &rx_actual);
  if (status == ZX_OK && rx_actual == size) {
    auto rx_vector = fidl::VectorView<uint8_t>::FromExternal(rxdata.data(), size);
    completer.Reply(ZX_OK, rx_vector);
  } else {
    completer.Reply(status == ZX_OK ? ZX_ERR_INTERNAL : status, fidl::VectorView<uint8_t>());
  }
}

void SpiChild::RegisterVmo(RegisterVmoRequestView request, RegisterVmoCompleter::Sync& completer) {
  zx_status_t status =
      spi_.RegisterVmo(cs_, request->vmo_id, std::move(request->vmo.vmo), request->vmo.offset,
                       request->vmo.size, static_cast<uint32_t>(request->rights));
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void SpiChild::UnregisterVmo(UnregisterVmoRequestView request,
                             UnregisterVmoCompleter::Sync& completer) {
  zx::vmo vmo;
  if (zx_status_t status = spi_.UnregisterVmo(cs_, request->vmo_id, &vmo); status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess(std::move(vmo));
}

void SpiChild::Transmit(TransmitRequestView request, TransmitCompleter::Sync& completer) {
  TRACE_DURATION("spi", "Transmit", "cs", cs_, "size", request->buffer.size);
  zx_status_t status =
      spi_.TransmitVmo(cs_, request->buffer.vmo_id, request->buffer.offset, request->buffer.size);
  completer.Reply(zx::make_result(status));
}

void SpiChild::Receive(ReceiveRequestView request, ReceiveCompleter::Sync& completer) {
  TRACE_DURATION("spi", "Receive", "cs", cs_, "size", request->buffer.size);
  zx_status_t status =
      spi_.ReceiveVmo(cs_, request->buffer.vmo_id, request->buffer.offset, request->buffer.size);
  completer.Reply(zx::make_result(status));
}

void SpiChild::Exchange(ExchangeRequestView request, ExchangeCompleter::Sync& completer) {
  if (request->tx_buffer.size != request->rx_buffer.size) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  TRACE_DURATION("spi", "Exchange", "cs", cs_, "size", request->tx_buffer.size);
  zx_status_t status = spi_.ExchangeVmo(cs_, request->tx_buffer.vmo_id, request->tx_buffer.offset,
                                        request->rx_buffer.vmo_id, request->rx_buffer.offset,
                                        request->tx_buffer.size);
  completer.Reply(zx::make_result(status));
}

zx_status_t SpiChild::SpiTransmit(const uint8_t* txdata_list, size_t txdata_count) {
  size_t actual;
  spi_.Exchange(cs_, txdata_list, txdata_count, nullptr, 0, &actual);
  return ZX_OK;
}
zx_status_t SpiChild::SpiReceive(uint32_t size, uint8_t* out_rxdata_list, size_t rxdata_count,
                                 size_t* out_rxdata_actual) {
  spi_.Exchange(cs_, nullptr, 0, out_rxdata_list, rxdata_count, out_rxdata_actual);
  return ZX_OK;
}

zx_status_t SpiChild::SpiExchange(const uint8_t* txdata_list, size_t txdata_count,
                                  uint8_t* out_rxdata_list, size_t rxdata_count,
                                  size_t* out_rxdata_actual) {
  spi_.Exchange(cs_, txdata_list, txdata_count, out_rxdata_list, rxdata_count, out_rxdata_actual);
  return ZX_OK;
}

void SpiChild::CanAssertCs(CanAssertCsCompleter::Sync& completer) {
  completer.Reply(!has_siblings_);
}

void SpiChild::AssertCs(AssertCsCompleter::Sync& completer) {
  if (has_siblings_) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
  } else {
    completer.Reply(spi_.LockBus(cs_));
  }
}

void SpiChild::DeassertCs(DeassertCsCompleter::Sync& completer) {
  if (has_siblings_) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
  } else {
    completer.Reply(spi_.UnlockBus(cs_));
  }
}

void SpiChild::Bind(async_dispatcher_t* dispatcher,
                    fidl::ServerEnd<fuchsia_hardware_spi::Device> server_end) {
  if (binding_.has_value()) {
    server_end.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }
  binding_ = Binding{
      .binding = fidl::BindServer(
          dispatcher, std::move(server_end), this,
          [](SpiChild* self, fidl::UnbindInfo, fidl::ServerEnd<fuchsia_hardware_spi::Device>) {
            std::optional opt = std::exchange(self->binding_, {});
            ZX_ASSERT(opt.has_value());

            self->spi_.ReleaseRegisteredVmos(self->cs_);

            // If the server is unbinding because DdkUnbind is being called, then reply to the
            // unbind transaction so that it completes.
            Binding& binding = opt.value();
            if (binding.unbind_txn.has_value()) {
              binding.unbind_txn.value().Reply();
            }
          }),
  };
}

void SpiChild::DdkUnbind(ddk::UnbindTxn txn) {
  if (binding_.has_value()) {
    Binding& binding = binding_.value();
    ZX_ASSERT(!binding.unbind_txn.has_value());
    binding.unbind_txn.emplace(std::move(txn));
    binding.binding.Unbind();
  } else {
    txn.Reply();
  }
}

void SpiChild::DdkRelease() { delete this; }

zx_status_t SpiChild::ServeOutgoingDirectory(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  zx::result status = outgoing_.AddUnmanagedProtocol<fuchsia_hardware_spi::Device>(
      [this](fidl::ServerEnd<fuchsia_hardware_spi::Device> server_end) {
        Bind(dispatcher_, std::move(server_end));
      });
  if (status.is_error()) {
    return status.status_value();
  }

  return outgoing_.Serve(std::move(server_end)).status_value();
}

}  // namespace spi
