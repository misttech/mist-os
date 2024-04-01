// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_CHILD_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_CHILD_H_

#include <fidl/fuchsia.hardware.spi/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/fidl.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <ddktl/device.h>

namespace spi {

class SpiDevice;

class SpiChild;
using SpiChildType =
    ddk::Device<SpiChild, ddk::Messageable<fuchsia_hardware_spi::Controller>::Mixin,
                ddk::Unbindable>;

class SpiChild : public SpiChildType, public fidl::WireServer<fuchsia_hardware_spi::Device> {
 public:
  SpiChild(zx_device_t* parent, fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl> spi,
           uint32_t chip_select, bool has_siblings, fdf::UnownedDispatcher fidl_dispatcher)
      : SpiChildType(parent),
        spi_(std::move(spi)),
        cs_(chip_select),
        has_siblings_(has_siblings),
        fidl_dispatcher_(std::move(fidl_dispatcher)),
        outgoing_(fidl_dispatcher_->async_dispatcher()) {}

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

  void Bind(fidl::ServerEnd<fuchsia_hardware_spi::Device> server_end);

  zx_status_t ServeOutgoingDirectory(fidl::ServerEnd<fuchsia_io::Directory> server_end);

 private:
  fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl> spi_;
  const uint32_t cs_;
  // False if this child is the only device on the bus.
  const bool has_siblings_;
  const fdf::UnownedDispatcher fidl_dispatcher_;

  using Binding = struct {
    fidl::ServerBindingRef<fuchsia_hardware_spi::Device> binding;
    std::optional<ddk::UnbindTxn> unbind_txn;
  };

  // These objects can only be accessed on the FIDL dispatcher. Making them optional allows them to
  // be destroyed manually in our unbind hook.
  std::optional<Binding> binding_;
  std::optional<component::OutgoingDirectory> outgoing_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_CHILD_H_
