// Copyright (c) 2022 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/data_plane.h"

#include <lib/async/cpp/task.h>

#include <wlan/common/ieee80211.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/align.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/debug.h"

namespace wlan::nxpfmac {

constexpr char kNetDeviceName[] = "nxpfmac-netdev";
constexpr char kRxWorkThreadName[] = "nxpfmac_rx_work";
constexpr uint32_t kMinRxBufferLength = IEEE80211_MSDU_SIZE_MAX;

// Maximum number of TX buffers we want to queue up.
constexpr uint32_t kTxDepth = 512;
// Maximum number of RX buffers we want to have available.
constexpr uint32_t kRxDepth = 512;
// The threshold for when the amount of RX buffers available to us should definitely be replenished.
constexpr uint32_t kRxThreshold = 128;

DataPlaneIfc::~DataPlaneIfc() = default;

zx_status_t DataPlane::Create(zx_device_t *parent, DataPlaneIfc *ifc, BusInterface *bus,
                              void *mlan_adapter, std::unique_ptr<DataPlane> *out_data_plane) {
  std::unique_ptr<DataPlane> data_plane(new DataPlane(parent, ifc, bus, mlan_adapter));

  const zx_status_t status = data_plane->Init();
  if (status != ZX_OK) {
    return status;
  }

  *out_data_plane = std::move(data_plane);
  return ZX_OK;
}

DataPlane::DataPlane(zx_device_t *parent, DataPlaneIfc *ifc, BusInterface *bus, void *mlan_adapter)
    : ifc_(ifc), network_device_(parent, this), bus_(bus), mlan_adapter_(mlan_adapter) {}

DataPlane::~DataPlane() {
  // First just check the completion without waiting, if the completion is signaled it means that
  // the device was already removed and if we try to call remove again that could cause problems.
  if (sync_completion_wait(&network_device_released_, 0) == ZX_OK) {
    // The device was already removed, we're done.
    return;
  }
  // The device is not yet removed, call remove ourselves.
  network_device_.Remove();
  sync_completion_wait(&network_device_released_, ZX_TIME_INFINITE);
}

zx_status_t DataPlane::Init() {
  zx_status_t status = rx_work_loop_.StartThread(kRxWorkThreadName);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to start RX work thread: %s", zx_status_get_string(status));
    return status;
  }

  status = network_device_.Init(kNetDeviceName);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to initialize network device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

network_device_ifc_protocol_t DataPlane::NetDevIfcProto() {
  return network_device_.NetDevIfcProto();
}

void DataPlane::DeferRxWork() {
  const zx_status_t status = async::PostTask(rx_work_loop_.dispatcher(),
                                             [this] { mlan_rx_process(mlan_adapter_, nullptr); });

  if (status != ZX_OK) {
    NXPF_THROTTLE_ERR("Error deferring RX work, could not schedule work: %s",
                      zx_status_get_string(status));
  }
}

void DataPlane::FlushRxWork() {
  // Post a task on the RX work dispatcher and wait for it to run. Once the task has finished
  // running that means that all previously scheduled RX work has completed.
  sync_completion_t completion;
  zx_status_t status = async::PostTask(rx_work_loop_.dispatcher(),
                                       [&completion] { sync_completion_signal(&completion); });
  if (status != ZX_OK) {
    NXPF_THROTTLE_ERR("Failed to post flush RX work task: %s", zx_status_get_string(status));
    return;
  }
  status = sync_completion_wait(&completion, ZX_TIME_INFINITE);
  if (status != ZX_OK) {
    NXPF_THROTTLE_ERR("Failed to wait for RX work to flush: %s", zx_status_get_string(status));
    return;
  }
}

zx_status_t DataPlane::SendFrame(wlan::drivers::components::Frame &&frame,
                                 mlan_buf_type frame_type) {
  zx_status_t status = SendFrameImpl(std::move(frame), frame_type);
  if (status != ZX_OK) {
    // SendFrameImpl will have logged the error and completed the TX.
    return status;
  }

  status = bus_->TriggerMainProcess();
  if (status != ZX_OK) {
    NXPF_ERR("Failed to trigger main process to initiate transmission: %s",
             zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t DataPlane::SendFrames(cpp20::span<wlan::drivers::components::Frame> frames,
                                  mlan_buf_type frame_type) {
  size_t successful_sends = 0;
  for (auto &frame : frames) {
    if (likely(SendFrameImpl(std::move(frame), frame_type) == ZX_OK)) {
      ++successful_sends;
    }
  }
  if (likely(successful_sends > 0)) {
    const zx_status_t status = bus_->TriggerMainProcess();
    if (status != ZX_OK) {
      NXPF_ERR("Failed to trigger main process to initiate transmission: %s",
               zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}
void DataPlane::CompleteTx(wlan::drivers::components::Frame &&frame, zx_status_t status,
                           mlan_buf_type buf_type) {
  if (likely(frame.Size() >= sizeof(ethhdr))) {
    auto eth = reinterpret_cast<ethhdr *>(frame.Data());
    if (unlikely(eth->h_proto == 0x8e88)) {
      ifc_->OnEapolTransmitted(std::move(frame), status);
      return;
    }
  }
  if (buf_type != MLAN_BUF_TYPE_DATA) {
    // Only complete data buffers, ignore other types of buffers, they were not sent by netdevice.
    return;
  }

  network_device_.CompleteTx(cpp20::span<wlan::drivers::components::Frame>(&frame, 1), status);
}

void DataPlane::CompleteRx(wlan::drivers::components::Frame &&frame) {
  if (likely(frame.Size() >= sizeof(ethhdr))) {
    auto eth = reinterpret_cast<ethhdr *>(frame.Data());
    if (unlikely(eth->h_proto == 0x8e88)) {
      NXPF_INFO("Received EAPOL response");
      ifc_->OnEapolReceived(std::move(frame));
      return;
    }
  }

  network_device_.CompleteRx(std::move(frame));
}

std::optional<wlan::drivers::components::Frame> DataPlane::AcquireFrame() {
  std::lock_guard lock(rx_frames_);
  return rx_frames_.Acquire();
}

void DataPlane::NetDevRelease() { sync_completion_signal(&network_device_released_); }

zx_status_t DataPlane::NetDevInit() { return ZX_OK; }

void DataPlane::NetDevStart(StartTxn txn) { txn.Reply(ZX_OK); }

void DataPlane::NetDevStop(StopTxn txn) { txn.Reply(); }

void DataPlane::NetDevGetInfo(device_impl_info_t *out_info) {
  // Query the bus for some if this information.
  const uint16_t rx_headroom = bus_->GetRxHeadroom();
  const uint16_t tx_headroom = bus_->GetTxHeadroom();
  const uint32_t buffer_alignment = bus_->GetBufferAlignment();

  *out_info = {
      .tx_depth = kTxDepth,
      .rx_depth = kRxDepth,
      .rx_threshold = kRxThreshold,
      .max_buffer_parts = 1,
      .max_buffer_length = ZX_PAGE_SIZE,
      .buffer_alignment = buffer_alignment,
      // TODO(https://fxbug.dev/110577): Revisit this minimum size when ASMDU support is added.
      .min_rx_buffer_length = align<uint32_t>(kMinRxBufferLength + rx_headroom, ZX_PAGE_SIZE),
      // Make sure there's enough headroom for a frame object, an mlan_buffer and the TX headroom
      // required by the bus. The tx headroom needs to start on an aligned address so make sure the
      // preceeding data is aligned to the bus's alignment requirement.
      .tx_head_length = static_cast<uint16_t>(
          align<uint32_t>(sizeof(wlan::drivers::components::Frame) + sizeof(mlan_buffer),
                          buffer_alignment) +
          tx_headroom),
      .tx_tail_length = 0,
  };
}

void DataPlane::NetDevQueueTx(cpp20::span<wlan::drivers::components::Frame> frames) {
  // The return status of SendFrames doesn't really matter here. The failed frames are completed by
  // SendFrames and it will also log any error messages.
  SendFrames(frames, MLAN_BUF_TYPE_DATA);
}

void DataPlane::NetDevQueueRxSpace(const rx_space_buffer_t *buffers_list, size_t buffers_count,
                                   uint8_t *vmo_addrs[]) {
  std::lock_guard lock(rx_frames_);
  rx_frames_.Store(buffers_list, buffers_count, vmo_addrs);
}

zx_status_t DataPlane::NetDevPrepareVmo(uint8_t vmo_id, zx::vmo vmo, uint8_t *mapped_address,
                                        size_t mapped_size) {
  zx_status_t status = bus_->PrepareVmo(vmo_id, std::move(vmo));
  if (status != ZX_OK) {
    NXPF_ERR("Failed to prepare VMO: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

void DataPlane::NetDevReleaseVmo(uint8_t vmo_id) {
  zx_status_t status = bus_->ReleaseVmo(vmo_id);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to release VMO: %s", zx_status_get_string(status));
  }
}

void DataPlane::NetDevSetSnoopEnabled(bool snoop) {}

zx_status_t DataPlane::SendFrameImpl(wlan::drivers::components::Frame &&frame,
                                     mlan_buf_type frame_type) {
  constexpr size_t kStaticHeaderRoom =
      sizeof(wlan::drivers::components::Frame) + sizeof(mlan_buffer);
  ZX_ASSERT(frame.Headroom() >= kStaticHeaderRoom);
  uint8_t *const frame_start = frame.Data() - frame.Headroom();
  uint8_t *const mlan_buffer_start = frame_start + sizeof(wlan::drivers::components::Frame);

  // Use placement new to copy the frame object into the headroom reserved for this purpose. Keep
  // the pointer in the mlan_buffer's pdesc.
  auto frame_ptr = new (frame_start) wlan::drivers::components::Frame(std::move(frame));

  auto mbuf = reinterpret_cast<mlan_buffer *>(mlan_buffer_start);
  memset(mbuf, 0, sizeof(*mbuf));

  const uint32_t packet_headroom = frame.Headroom() - kStaticHeaderRoom;

  mbuf->bss_index = frame.PortId();
  mbuf->data_len = frame.Size();
  mbuf->data_offset = packet_headroom;
  mbuf->pbuf = frame.Data() - packet_headroom;
  mbuf->buf_type = frame_type;
  mbuf->pdesc = frame_ptr;
  mbuf->priority = frame.Priority();

  mlan_status ml_status = mlan_send_packet(mlan_adapter_, mbuf);
  if (ml_status != MLAN_STATUS_PENDING) {
    NXPF_ERR("Failed to send packet: %d", ml_status);
    network_device_.CompleteTx(cpp20::span<wlan::drivers::components::Frame>(frame_ptr, 1),
                               ZX_ERR_INTERNAL);
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

}  // namespace wlan::nxpfmac
