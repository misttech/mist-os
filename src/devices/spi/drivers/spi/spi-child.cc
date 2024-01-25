// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi-child.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/trace-engine/types.h>
#include <zircon/errors.h>

#include <ddktl/fidl.h>

namespace spi {

template <typename T>
inline zx_status_t FidlStatus(const T& result) {
  if (result.ok()) {
    return result->is_ok() ? ZX_OK : result->error_value();
  }
  return result.status();
}

inline fidl::VectorView<uint8_t> VectorToFidl(std::vector<uint8_t>& data) {
  return fidl::VectorView<uint8_t>::FromExternal(data.data(), data.size());
}

inline std::vector<uint8_t> FidlToVector(const fidl::VectorView<uint8_t>& data) {
  return std::vector<uint8_t>(data.cbegin(), data.cend());
}

inline fuchsia_hardware_sharedmemory::wire::SharedVmoBuffer BufferToWire(
    const fuchsia_hardware_sharedmemory::SharedVmoBuffer& buffer) {
  return {buffer.vmo_id(), buffer.offset(), buffer.size()};
}

void SpiChild::OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) {
  Bind(dispatcher_, std::move(request->session));
}

void SpiChild::TransmitVector(TransmitVectorRequest& request,
                              TransmitVectorCompleter::Sync& completer) {
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->TransmitVector(cs_, VectorToFidl(request.data()));
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::TransmitVector: %s", zx_status_get_string(status));
    completer.Reply(status);
  } else {
    completer.Reply(ZX_OK);
  }
}

void SpiChild::ReceiveVector(ReceiveVectorRequest& request,
                             ReceiveVectorCompleter::Sync& completer) {
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->ReceiveVector(cs_, request.size());
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::ReceiveVector: %s", zx_status_get_string(status));
    completer.Reply({status, std::vector<uint8_t>()});
  } else if (result->value()->data.count() != request.size()) {
    zxlogf(ERROR, "Expected %u bytes != received %zu bytes", request.size(),
           result->value()->data.count());
    completer.Reply({ZX_ERR_INTERNAL, std::vector<uint8_t>()});
  } else {
    completer.Reply({ZX_OK, FidlToVector(result->value()->data)});
  }
}

void SpiChild::ExchangeVector(ExchangeVectorRequest& request,
                              ExchangeVectorCompleter::Sync& completer) {
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->ExchangeVector(cs_, VectorToFidl(request.txdata()));
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::ExchangeVector: %s", zx_status_get_string(status));
    completer.Reply({status, std::vector<uint8_t>()});
  } else if (result->value()->rxdata.count() != request.txdata().size()) {
    zxlogf(ERROR, "Expected %zu bytes != received %zu bytes", request.txdata().size(),
           result->value()->rxdata.count());
    completer.Reply({ZX_ERR_INTERNAL, std::vector<uint8_t>()});
  } else {
    completer.Reply({ZX_OK, FidlToVector(result->value()->rxdata)});
  }
}

void SpiChild::RegisterVmo(RegisterVmoRequest& request, RegisterVmoCompleter::Sync& completer) {
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->RegisterVmo(
      cs_, request.vmo_id(),
      {std::move(request.vmo().vmo()), request.vmo().offset(), request.vmo().size()},
      request.rights());
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::RegisterVmo: %s", zx_status_get_string(status));
    completer.Reply(fit::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

void SpiChild::UnregisterVmo(UnregisterVmoRequest& request,
                             UnregisterVmoCompleter::Sync& completer) {
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->UnregisterVmo(cs_, request.vmo_id());
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::UnregisterVmo: %s", zx_status_get_string(status));
    completer.Reply(fit::error(status));
  } else {
    completer.Reply(fit::ok(std::move(result->value()->vmo)));
  }
}

void SpiChild::Transmit(TransmitRequest& request, TransmitCompleter::Sync& completer) {
  TRACE_DURATION("spi", "Transmit", "cs", cs_, "size", request.buffer().size());
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->TransmitVmo(cs_, BufferToWire(request.buffer()));
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::TransmitVmo: %s", zx_status_get_string(status));
    completer.Reply(fit::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

void SpiChild::Receive(ReceiveRequest& request, ReceiveCompleter::Sync& completer) {
  TRACE_DURATION("spi", "Receive", "cs", cs_, "size", request.buffer().size());
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->ReceiveVmo(cs_, BufferToWire(request.buffer()));
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::ReceiveVmo: %s", zx_status_get_string(status));
    completer.Reply(fit::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

void SpiChild::Exchange(ExchangeRequest& request, ExchangeCompleter::Sync& completer) {
  if (request.tx_buffer().size() != request.rx_buffer().size()) {
    zxlogf(ERROR, "tx_buffer and rx_buffer size must match. %zu (tx) != %zu (rx)",
           request.tx_buffer().size(), request.rx_buffer().size());
    completer.Reply(fit::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  TRACE_DURATION("spi", "Exchange", "cs", cs_, "size", request.tx_buffer().size());
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->ExchangeVmo(cs_, BufferToWire(request.tx_buffer()),
                                                       BufferToWire(request.rx_buffer()));
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::ExchangeVmo: %s", zx_status_get_string(status));
    completer.Reply(fit::error(status));
  } else {
    completer.Reply(fit::ok());
  }
}

zx_status_t SpiChild::SpiTransmit(const uint8_t* txdata_list, size_t txdata_count) {
  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->TransmitVector(
      cs_,
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(txdata_list), txdata_count));
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::TransmitVector: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t SpiChild::SpiReceive(uint32_t size, uint8_t* out_rxdata_list, size_t rxdata_count,
                                 size_t* out_rxdata_actual) {
  if (size > rxdata_count) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->ReceiveVector(cs_, size);
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::ReceiveVector: %s", zx_status_get_string(status));
    return status;
  }
  if (result->value()->data.count() != size) {
    zxlogf(ERROR, "Expected %u bytes != received %zu bytes", size, result->value()->data.count());
    return ZX_ERR_INTERNAL;
  }

  *out_rxdata_actual = result->value()->data.count();
  memcpy(out_rxdata_list, result->value()->data.data(), *out_rxdata_actual);

  return ZX_OK;
}

zx_status_t SpiChild::SpiExchange(const uint8_t* txdata_list, size_t txdata_count,
                                  uint8_t* out_rxdata_list, size_t rxdata_count,
                                  size_t* out_rxdata_actual) {
  if (txdata_count != rxdata_count) {
    return ZX_ERR_INVALID_ARGS;
  }

  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->ExchangeVector(
      cs_,
      fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(txdata_list), txdata_count));
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "Couldn't complete SpiImpl::ExchangeVector: %s", zx_status_get_string(status));
    return status;
  }
  if (result.value()->rxdata.count() != rxdata_count) {
    zxlogf(ERROR, "Expected %zu bytes != received %zu bytes", rxdata_count,
           result->value()->rxdata.count());
    return ZX_ERR_INTERNAL;
  }

  *out_rxdata_actual = result.value()->rxdata.count();
  memcpy(out_rxdata_list, result.value()->rxdata.data(), *out_rxdata_actual);

  return ZX_OK;
}

void SpiChild::CanAssertCs(CanAssertCsCompleter::Sync& completer) {
  completer.Reply(!has_siblings_);
}

void SpiChild::AssertCs(AssertCsCompleter::Sync& completer) {
  if (has_siblings_) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->LockBus(cs_);
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::LockBus failed: %s", zx_status_get_string(status));
    completer.Reply(status);
  } else {
    completer.Reply(ZX_OK);
  }
}

void SpiChild::DeassertCs(DeassertCsCompleter::Sync& completer) {
  if (has_siblings_) {
    completer.Reply(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  fdf::Arena arena('SPI_');
  auto result = spi_.sync().buffer(arena)->UnlockBus(cs_);
  if (zx_status_t status = FidlStatus(result); status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::UnlockBus failed: %s", zx_status_get_string(status));
    completer.Reply(status);
  } else {
    completer.Reply(ZX_OK);
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

            fdf::Arena arena('SPI_');
            (void)self->spi_.sync().buffer(arena)->ReleaseRegisteredVmos(self->cs_);

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
  zx::result status = outgoing_.AddService<fuchsia_hardware_spi::Service>(
      fuchsia_hardware_spi::Service::InstanceHandler({
          .device =
              [this](fidl::ServerEnd<fuchsia_hardware_spi::Device> server_end) {
                Bind(dispatcher_, std::move(server_end));
              },
      }));
  if (status.is_error()) {
    return status.status_value();
  }

  return outgoing_.Serve(std::move(server_end)).status_value();
}

}  // namespace spi
