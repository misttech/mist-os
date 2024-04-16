// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MISC_GOLDFISH_PIPE_DEVICE_H_
#define SRC_GRAPHICS_DRIVERS_MISC_GOLDFISH_PIPE_DEVICE_H_

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/io-buffer.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>
#include <threads.h>
#include <zircon/types.h>

#include <map>
#include <memory>
#include <optional>

#include <ddktl/device.h>
#include <fbl/mutex.h>

#include "src/devices/lib/acpi/client.h"

namespace goldfish {

class PipeDevice;
using DeviceType = ddk::Device<PipeDevice>;

// |PipeDevice| is the "root" ACPI device that creates pipes and executes pipe
// operations. It could create multiple |PipeChildDevice| instances using
// |CreateChildDevice| method, each having its own properties so that they can
// be bound to different drivers, but sharing the same parent |PipeDevice|.
class PipeDevice : public DeviceType {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  explicit PipeDevice(zx_device_t* parent, acpi::Client client, async_dispatcher_t* dispatcher);
  ~PipeDevice();

  zx_status_t Bind();
  zx_status_t CreateChildDevice(cpp20::span<const zx_device_prop_t> props, const char* dev_name);

  // Device protocol implementation.
  void DdkRelease();

  zx_status_t Create(int32_t* out_id, zx::vmo* out_vmo);
  zx_status_t SetEvent(int32_t id, zx::event pipe_event);
  void Destroy(int32_t id);
  void Open(int32_t id);
  void Exec(int32_t id);
  zx_status_t GetBti(zx::bti* out_bti);

  int IrqHandler();

 private:
  struct Pipe {
    Pipe(zx_paddr_t paddr, zx::pmt pmt, zx::event pipe_event);
    ~Pipe();

    void SignalEvent(uint32_t flags) const;

    const zx_paddr_t paddr;
    zx::pmt pmt;
    zx::event pipe_event;
  };

  acpi::Client acpi_fidl_;
  zx::interrupt irq_;
  zx::bti bti_;
  ddk::IoBuffer io_buffer_;
  thrd_t irq_thread_{};
  int32_t next_pipe_id_ TA_GUARDED(pipes_lock_) = 1;

  fbl::Mutex mmio_lock_;
  std::optional<fdf::MmioBuffer> mmio_ TA_GUARDED(mmio_lock_);

  fbl::Mutex pipes_lock_;
  // TODO(https://fxbug.dev/42107181): This should be std::unordered_map.
  using PipeMap = std::map<int32_t, std::unique_ptr<Pipe>>;
  PipeMap pipes_ TA_GUARDED(pipes_lock_);
  async_dispatcher_t* const dispatcher_;

  DISALLOW_COPY_ASSIGN_AND_MOVE(PipeDevice);
};

class PipeChildDevice;
using PipeChildDeviceType =
    ddk::Device<PipeChildDevice, ddk::Unbindable,
                ddk::Messageable<fuchsia_hardware_goldfish::Controller>::Mixin>;

// |PipeChildDevice| is created by |PipeDevice| and serves the
// |fuchsia.hardware.goldfish.GoldfishPipe| FIDL protocol by forwarding all the
// FIDL requests to the parent device.
class PipeChildDevice : public PipeChildDeviceType,
                        public fidl::WireServer<fuchsia_hardware_goldfish_pipe::GoldfishPipe> {
 public:
  PipeChildDevice(PipeDevice* parent, async_dispatcher_t* dispatcher);
  ~PipeChildDevice() override = default;

  zx_status_t Bind(cpp20::span<const zx_device_prop_t> props, const char* dev_name);

  // Device protocol implementation.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  // fuchsia.hardware.goldfish.Controller APIs.
  void OpenSession(OpenSessionRequestView request, OpenSessionCompleter::Sync& completer) override;

  // fuchsia.hardware.goldfish.pipe.GoldfishPipe APIs.
  void Create(CreateCompleter::Sync& completer) override;
  void SetEvent(SetEventRequestView request, SetEventCompleter::Sync& completer) override;
  void Destroy(DestroyRequestView request, DestroyCompleter::Sync& completer) override;
  void Open(OpenRequestView request, OpenCompleter::Sync& completer) override;
  void Exec(ExecRequestView request, ExecCompleter::Sync& completer) override;
  void GetBti(GetBtiCompleter::Sync& completer) override;

 private:
  PipeDevice* const parent_;
  async_dispatcher_t* const dispatcher_;
  component::OutgoingDirectory outgoing_;

  fidl::ServerBindingGroup<fuchsia_hardware_goldfish_pipe::GoldfishPipe> pipe_bindings_;
  fidl::ServerBindingGroup<fuchsia_hardware_goldfish::PipeDevice> bindings_;
  std::optional<ddk::UnbindTxn> unbind_txn_;
};

}  // namespace goldfish

#endif  // SRC_GRAPHICS_DRIVERS_MISC_GOLDFISH_PIPE_DEVICE_H_
