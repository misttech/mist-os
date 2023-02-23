// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_CHILD_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_CHILD_H_

#include <fidl/fuchsia.hardware.spi/cpp/wire.h>
#include <fuchsia/hardware/spi/cpp/banjo.h>
#include <fuchsia/hardware/spiimpl/cpp/banjo.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <variant>

#include <ddktl/device.h>

// This header defines three classes: SpiChild, SpiFidlChild, and SpiBanjoChild. They are arranged
// in the node topology as follows:
//
//   spi --> SpiDevice (from spi.h)
//     spi-0-0 --> SpiChild
//       spi-fidl-0-0 --> SpiFidlChild
//       spi-banjo-0-0 --> SpiBanjoChild
//
// SpiDevice and SpiChild implement the actual SPI logic; SpiFidlChild and SpiBanjoChild serve the
// fuchsia.hardware.spi protocols over FIDL and Banjo, respectively, but delegate to their SpiChild
// parent for the SPI operations. SpiChild also exposes a /dev/class/spi entry.

namespace spi {

class SpiDevice;

class SpiChild;
using SpiChildType =
    ddk::Device<SpiChild, ddk::Messageable<fuchsia_hardware_spi::Controller>::Mixin,
                ddk::Unbindable>;

class SpiChild : public SpiChildType,
                 public ddk::SpiProtocol<SpiChild>,
                 public fidl::WireServer<fuchsia_hardware_spi::Device> {
 public:
  SpiChild(zx_device_t* parent, ddk::SpiImplProtocolClient spi, uint32_t chip_select,
           bool has_siblings, async_dispatcher_t* dispatcher)
      : SpiChildType(parent),
        spi_(spi),
        cs_(chip_select),
        has_siblings_(has_siblings),
        dispatcher_(dispatcher) {}

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  void OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) override;

  void TransmitVector(TransmitVectorRequestView request,
                      TransmitVectorCompleter::Sync& completer) override;
  void ReceiveVector(ReceiveVectorRequestView request,
                     ReceiveVectorCompleter::Sync& completer) override;
  void ExchangeVector(ExchangeVectorRequestView request,
                      ExchangeVectorCompleter::Sync& completer) override;

  void RegisterVmo(RegisterVmoRequestView request, RegisterVmoCompleter::Sync& completer) override;
  void UnregisterVmo(UnregisterVmoRequestView request,
                     UnregisterVmoCompleter::Sync& completer) override;

  void Transmit(TransmitRequestView request, TransmitCompleter::Sync& completer) override;
  void Receive(ReceiveRequestView request, ReceiveCompleter::Sync& completer) override;
  void Exchange(ExchangeRequestView request, ExchangeCompleter::Sync& completer) override;

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

  spi_protocol_ops_t& spi_protocol_ops() { return spi_protocol_ops_; }

 private:
  const ddk::SpiImplProtocolClient spi_;
  const uint32_t cs_;
  // False if this child is the only device on the bus.
  const bool has_siblings_;
  async_dispatcher_t* const dispatcher_;

  using Binding = struct {
    fidl::ServerBindingRef<fuchsia_hardware_spi::Device> binding;
    std::optional<ddk::UnbindTxn> unbind_txn;
  };
  std::optional<Binding> binding_;
};

class SpiFidlChild;
using SpiFidlChildType = ddk::Device<SpiFidlChild>;

// An SPI child device that serves the fuchsia.hardware.spi/Device FIDL
// protocol. Note that while SpiChild also serves this protocol, it does not
// expose it in its outgoing directory for its children to use, while
// SpiFidlChild does. Otherwise, it simply delegates all its FIDL methods to
// SpiChild.
//
// See SpiBanjoChild for the corresponding Banjo sibling device.
class SpiFidlChild : public SpiFidlChildType {
 public:
  SpiFidlChild(zx_device_t* parent, SpiChild* spi, async_dispatcher_t* dispatcher);

  void DdkRelease();

  zx_status_t ServeOutgoingDirectory(fidl::ServerEnd<fuchsia_io::Directory> server_end);

 private:
  // SpiChild is the parent of SpiFidlChild so it is guaranteed to outlive it,
  // and this pointer will always remain valid.
  SpiChild* spi_;
  component::OutgoingDirectory outgoing_;
};

class SpiBanjoChild;
using SpiBanjoChildType = ddk::Device<SpiBanjoChild, ddk::GetProtocolable>;

class SpiBanjoChild : public SpiBanjoChildType {
 public:
  SpiBanjoChild(zx_device_t* parent, SpiChild* spi) : SpiBanjoChildType(parent), spi_(spi) {}

  void DdkRelease() { delete this; }
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_protocol);

 private:
  // SpiChild is the parent of SpiBanjoChild so it is guaranteed to outlive it,
  // and this pointer will always remain valid.
  SpiChild* spi_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_CHILD_H_
