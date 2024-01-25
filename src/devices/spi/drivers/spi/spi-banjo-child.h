// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_BANJO_CHILD_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_BANJO_CHILD_H_

#include <fidl/fuchsia.hardware.spi/cpp/fidl.h>
#include <fuchsia/hardware/spi/cpp/banjo.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <ddktl/device.h>

#include "spi-impl-client.h"

namespace spi {

class SpiBanjoChild;
using SpiBanjoChildType =
    ddk::Device<SpiBanjoChild, ddk::Messageable<fuchsia_hardware_spi::Controller>::Mixin,
                ddk::Unbindable>;

class SpiBanjoChild : public SpiBanjoChildType,
                      public ddk::SpiProtocol<SpiBanjoChild, ddk::base_protocol>,
                      public fidl::Server<fuchsia_hardware_spi::Device> {
 public:
  using ClientType = BanjoSpiImplClient*;

  SpiBanjoChild(zx_device_t* parent, BanjoSpiImplClient* spi, uint32_t chip_select,
                bool has_siblings, async_dispatcher_t* dispatcher)
      : SpiBanjoChildType(parent),
        spi_(spi),
        cs_(chip_select),
        has_siblings_(has_siblings),
        dispatcher_(dispatcher),
        outgoing_(dispatcher) {}

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  void OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) override;

  void TransmitVector(TransmitVectorRequest& request,
                      TransmitVectorCompleter::Sync& completer) override;
  void ReceiveVector(ReceiveVectorRequest& request,
                     ReceiveVectorCompleter::Sync& completer) override;
  void ExchangeVector(ExchangeVectorRequest& request,
                      ExchangeVectorCompleter::Sync& completer) override;

  void RegisterVmo(RegisterVmoRequest& request, RegisterVmoCompleter::Sync& completer) override;
  void UnregisterVmo(UnregisterVmoRequest& request,
                     UnregisterVmoCompleter::Sync& completer) override;

  void Transmit(TransmitRequest& request, TransmitCompleter::Sync& completer) override;
  void Receive(ReceiveRequest& request, ReceiveCompleter::Sync& completer) override;
  void Exchange(ExchangeRequest& request, ExchangeCompleter::Sync& completer) override;

  void CanAssertCs(CanAssertCsCompleter::Sync& completer) override;
  void AssertCs(AssertCsCompleter::Sync& completer) override;
  void DeassertCs(DeassertCsCompleter::Sync& completer) override;

  zx_status_t SpiTransmit(const uint8_t* txdata_list, size_t txdata_count);
  zx_status_t SpiReceive(uint32_t size, uint8_t* out_rxdata_list, size_t rxdata_count,
                         size_t* out_rxdata_actual);
  zx_status_t SpiExchange(const uint8_t* txdata_list, size_t txdata_count, uint8_t* out_rxdata_list,
                          size_t rxdata_count, size_t* out_rxdata_actual);

  void Bind(async_dispatcher_t* dispatcher,
            fidl::ServerEnd<fuchsia_hardware_spi::Device> server_end);

  zx_status_t ServeOutgoingDirectory(fidl::ServerEnd<fuchsia_io::Directory> server_end);

 private:
  BanjoSpiImplClient* spi_;
  const uint32_t cs_;
  // False if this child is the only device on the bus.
  const bool has_siblings_;
  async_dispatcher_t* const dispatcher_;

  using Binding = struct {
    fidl::ServerBindingRef<fuchsia_hardware_spi::Device> binding;
    std::optional<ddk::UnbindTxn> unbind_txn;
  };
  std::optional<Binding> binding_;
  component::OutgoingDirectory outgoing_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_BANJO_CHILD_H_
