// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEVICE_INTERFACE_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEVICE_INTERFACE_H_

#include <fuchsia/hardware/network/cpp/banjo.h>
#include <fuchsia/hardware/network/driver/cpp/banjo.h>
#include <lib/fit/function.h>

#include "data_structs.h"
#include "definitions.h"
#include "device_port.h"
#include "port_watcher.h"
#include "public/locks.h"
#include <object/fifo_dispatcher.h>

namespace network::testing {
class NetworkDeviceTest;
class FakeNetworkDeviceImpl;
namespace banjo {
class FakeNetworkDeviceImpl;
}
}  // namespace network::testing

namespace network::internal {
class RxQueue;
class RxSessionTransaction;

class TxQueue;

class Session;
class AttachedPort;
using SessionList = fbl::SizedDoublyLinkedList<std::unique_ptr<Session>>;

// Contains information about a filled Rx descriptor.
//
// Used to convey fulfilled rx frames in terms of descriptor indices.
struct SessionRxBuffer {
  uint16_t descriptor;
  uint32_t offset;
  uint32_t length;
};

// Helper struct containing information on an incoming complete rx frame.
//
// Used to cache common calculation and reduce number of arguments in functions.
struct RxFrameInfo {
  const buffer_metadata_t& meta;
  uint8_t port_id_salt;
  cpp20::span<const SessionRxBuffer> buffers;
  uint32_t total_length;
};

enum class DeviceStatus { STARTING, STARTED, STOPPING, STOPPED };

enum class PendingDeviceOperation { NONE, START, STOP };

class DeviceInterface : public ddk::NetworkDeviceIfcProtocol<DeviceInterface> {
 public:
  static zx::result<std::unique_ptr<DeviceInterface>> Create(
      const network_device_impl_protocol_t* device_impl);
  ~DeviceInterface();

  // Public NetworkDevice API.
  void Teardown(fit::callback<void()> callback);  // override;
  // zx_status_t Bind(fidl::ServerEnd<netdev::Device> req) override;
  // zx_status_t BindPort(uint8_t port_id, fidl::ServerEnd<netdev::Port> req) override;

  // NetworkDeviceIfc implementation.
  void NetworkDeviceIfcPortStatusChanged(uint8_t id, const port_status_t* new_status);
  void NetworkDeviceIfcAddPort(uint8_t id, const network_port_protocol_t* port,
                               network_device_ifc_add_port_callback callback, void* cookie);
  void NetworkDeviceIfcRemovePort(uint8_t id);
  void NetworkDeviceIfcCompleteRx(const rx_buffer_t* rx_list, size_t rx_count);
  void NetworkDeviceIfcCompleteTx(const tx_result_t* tx_list, size_t tx_count);
  void NetworkDeviceIfcDelegateRxLease(const delegated_rx_lease_t* delegated) {}

  uint16_t rx_fifo_depth() const;
  uint16_t tx_fifo_depth() const;

  // Returns the device-owned buffer count threshold at which we should trigger RxQueue work. If the
  // number of buffers on device is less than or equal to the threshold, we should attempt to fetch
  // more buffers.
  uint16_t rx_notify_threshold() const { return device_info_.rx_threshold; }

  TxQueue& tx_queue() { return *tx_queue_; }

  SharedLock& control_lock() __TA_RETURN_CAPABILITY(control_lock_) { return control_lock_; }
  fbl::Mutex& rx_lock() __TA_RETURN_CAPABILITY(rx_lock_) { return rx_lock_; }
  fbl::Mutex& tx_lock() __TA_RETURN_CAPABILITY(tx_lock_) { return tx_lock_; }
  const device_impl_info_t& info() { return device_info_; }

  // Loads rx path descriptors from the primary session into a session transaction.
  zx_status_t LoadRxDescriptors(RxSessionTransaction& transact) __TA_REQUIRES_SHARED(control_lock_);

  // Operates workflow for when a session is started. If the session is eligible to take over the
  // primary spot, it'll be elected the new primary session. If there was no primary session before,
  // the data path will be started BEFORE the new session is elected as primary,
  void SessionStarted(Session& session) __TA_RELEASE(control_lock_);
  // Operates workflow for when a session is stopped. If there's another session that is eligible to
  // take over the primary spot, it'll be elected the new primary session. Otherwise, the data path
  // will be stopped.
  void SessionStopped(Session& session) __TA_RELEASE(control_lock_);

  // If a primary session exists, primary_rx_fifo returns a reference-counted pointer to the primary
  // session's Rx FIFO. Otherwise, the returned pointer is null.
  fbl::RefPtr<FifoDispatcher> primary_rx_fifo();

  // Commits all pending rx buffers in all active sessions.
  void CommitAllSessions() __TA_REQUIRES_SHARED(control_lock_) __TA_REQUIRES(rx_lock_);
  // Copies the received data described by `buff` to all sessions other than `owner`.
  void CopySessionData(const Session& owner, const RxFrameInfo& frame_info)
      __TA_REQUIRES_SHARED(control_lock_) __TA_REQUIRES(rx_lock_);
  // Notifies all listening sessions of a new tx transaction from session `owner` and descriptor
  // `owner_index`.
  void ListenSessionData(const Session& owner, cpp20::span<const uint16_t> descriptors)
      __TA_REQUIRES(tx_lock_) __TA_EXCLUDES(control_lock_, rx_lock_);

  // Notifies that a batch of Tx frames has been returned.
  //
  // If was_full is true, all active sessions are notified that device tx space has freed up.
  // Checks if dead sessions are ready to be destroyed due to buffers returning.
  void NotifyTxReturned(bool was_full);
  // Sends the provided space buffers in `rx` to the device implementation.
  void QueueRxSpace(cpp20::span<rx_space_buffer_t> rx)
      __TA_EXCLUDES(control_lock_, rx_lock_, tx_lock_);
  // Sends the provided transmit buffers in `tx` to the device implementation.
  void QueueTx(cpp20::span<tx_buffer_t> tx) __TA_EXCLUDES(control_lock_, rx_lock_, tx_lock_);
  bool IsDataPlaneOpen() __TA_REQUIRES_SHARED(control_lock_);

  // Called by sessions when they're no longer running. If the dead session has any outstanding
  // buffers with the device implementation, it'll be kept in `dead_sessions_` until all the buffers
  // are safely returned and we own all the buffers again.
  void NotifyDeadSession(Session& dead_session);

  // FIDL protocol implementation.
  device_info_t GetInfo();
  zx::result<
      std::tuple<Session*, std::pair<KernelHandle<FifoDispatcher>, KernelHandle<FifoDispatcher>>>>
  OpenSession(ktl::string_view name, struct session_info session_info);
  void GetPort(port_id_t port_id, DevicePort** out_port);
  PortWatcher* GetPortWatcher();

  // void Clone(CloneRequestView request, CloneCompleter::Sync& _completer) override;

  // Returns the current port salt for the provided base port ID.
  //
  // If the port with |base_id| does not currently exist, returns the value of
  // the previously existing port with the same |base_id| or the initial salt
  // value.
  uint8_t GetPortSalt(uint8_t base_id) __TA_REQUIRES_SHARED(control_lock_) {
    return ports_[base_id].salt;
  }

  // Notifies of |frame_length| bytes received on port with |base_id|.
  void NotifyPortRxFrame(uint8_t base_id, uint64_t frame_length)
      __TA_REQUIRES_SHARED(control_lock_);

  // Acquires a port for use in a Session.
  //
  // Sessions are notified of ports that are no longer safe to use by the DeviceInterface through
  // Session::DetachPort.
  //
  // NB: The validity of the returned AttachedPort is not really guaranteed by the type system, but
  // by the fact that DeviceInterface will detach all ports from sessions before continuing.
  zx::result<AttachedPort> AcquirePort(port_id_t port_id,
                                       cpp20::span<const frame_type_t> rx_frame_types)
      __TA_REQUIRES(control_lock_);

  // Event observer hook for Rx queue packets.
  void NotifyRxQueuePacket(uint64_t key);
  // Event observer hook for Tx complete.
  void NotifyTxComplete();

  // DiagnosticsService& diagnostics() { return diagnostics_; }

  // static void DropDelegatedRxLease(netdev::DelegatedRxLease lease);

  // Delegates a pending lease to the primary session.
  //
  // The lease is delegated if |completed_frame_index| is larger than the lease's
  // hold_until_frame value.
  //
  // The primary session receives the lease if one exists _and_ the session is
  // opted in to receive leases. Drops the pending lease immediately otherwise.
  // void TryDelegateRxLease(uint64_t completed_frame_index) __TA_REQUIRES_SHARED(control_lock_)
  //    __TA_REQUIRES(rx_lock_);

 private:
  friend testing::NetworkDeviceTest;
  friend testing::FakeNetworkDeviceImpl;
  friend testing::banjo::FakeNetworkDeviceImpl;

#if 0
  // Helper class to keep track of clients bound to DeviceInterface.
  class Binding : public fbl::DoublyLinkedListable<std::unique_ptr<Binding>> {
   public:
    static zx_status_t Bind(DeviceInterface* interface, fidl::ServerEnd<netdev::Device> channel)
        __TA_REQUIRES(interface->control_lock_);
    void Unbind();

   private:
    Binding() = default;
    std::optional<fidl::ServerBindingRef<netdev::Device>> binding_;
  };
  using BindingList = fbl::SizedDoublyLinkedList<std::unique_ptr<Binding>>;
#endif
  enum class TeardownState {
    RUNNING,
    BINDINGS,
    PORT_WATCHERS,
    PORTS,
    SESSIONS,
    DEVICE_IMPL,
    IFC_BINDING,
    BINDER,
    FINISHED
  };

  zx_status_t Init(const network_device_impl_protocol_t* device_impl);
  explicit DeviceInterface();

  // Starts the data path with the device implementation.
  void StartDevice() __TA_EXCLUDES(control_lock_, tx_lock_, rx_lock_);
  void StartDeviceLocked() __TA_RELEASE(control_lock_) __TA_EXCLUDES(tx_lock_, rx_lock_);
  // Stops the data path with the device implementation.
  //
  // If continue_teardown is provided, teardown continuation will be attempted before notifying the
  // underlying device of stoppage.
  void StopDevice(std::optional<TeardownState> continue_teardown = std::nullopt)
      __TA_RELEASE(control_lock_) __TA_EXCLUDES(tx_lock_, rx_lock_);
  // Starts the device implementation with `DeviceStarted` as its callback.
  void StartDeviceInner() __TA_EXCLUDES(control_lock_);
  // Stops the device implementation with `DeviceStopped` as its callback.
  void StopDeviceInner() __TA_EXCLUDES(control_lock_);
  // Helper inner function to stop a session.
  //
  // Returns true if the device should be stopped.
  bool SessionStoppedInner(Session& session) __TA_REQUIRES(control_lock_);

  // Callback given to the device implementation for the `Start` call. The data path is considered
  // open only once the device is started.
  void DeviceStarted() __TA_RELEASE(control_lock_);
  // Callback given to the device implementation for the `Stop` call. All outstanding buffers are
  // automatically reclaimed once the device is considered stopped. If a teardown is pending,
  // `DeviceStopped` will complete the teardown BEFORE all buffers are reclaimed and all the
  // sessions are destroyed.
  void DeviceStopped();

  PendingDeviceOperation SetDeviceStatus(DeviceStatus status) __TA_REQUIRES(control_lock_);

  // Notifies the device implementation that the VMO used by the provided session will no longer be
  // used. It is called right before sessions are destroyed.
  // ReleaseVMO acquires the vmos_lock_ internally, so we mark it as excluding the vmos_lock_.
  void ReleaseVmo(Session& session, fit::callback<void()>&& on_complete)
      __TA_REQUIRES(control_lock_);

  // Continues a teardown process, if one is running.
  //
  // The provided state is the expected state that the teardown process is in. If the given state is
  // not the current teardown state, no processing will happen. Otherwise, the teardown process will
  // continue if the pre-conditions to move between teardown states are met.
  //
  // Returns true if the teardown is completed and execution should be stopped.
  // ContinueTeardown is marked with many thread analysis lock exclusions so it can acquire those
  // locks internally and evaluate the teardown progress.
  bool ContinueTeardown(TeardownState state) __TA_RELEASE(control_lock_)
      __TA_EXCLUDES(tx_lock_, rx_lock_);

  // Calls f with a const std::unique_ptr<DevicePort>& to the DevicePort referenced by port_id
  // or nullptr if no ports with that id are installed.
  //
  // Returns the value returned by the call to f.
  //
  // It is unsafe to use the provided DevicePort outside of the scope of the callback f.
  template <typename F>
  auto WithPort(uint8_t port_id, F f) __TA_REQUIRES_SHARED(control_lock_) {
    if (port_id >= ports_.size()) {
      const std::unique_ptr<DevicePort> null_port;
      return f(null_port);
    }
    return f(ports_[port_id].port);
  }
  void OnPortTeardownComplete(DevicePort& port);

  // Destroys all dead sessions that report they can be destroyed through `Session::CanDestroy`.
  void PruneDeadSessions() __TA_REQUIRES_SHARED(control_lock_);
  // Notifies all sessions that the transmit queue has available spots to take in transmit frames.
  void NotifyTxQueueAvailable() __TA_REQUIRES_SHARED(control_lock_);

  zx_status_t CanCreatePortWithId(uint8_t port_id) __TA_REQUIRES(control_lock_);

  device_impl_info_t device_info_;
  // DiagnosticsService diagnostics_;
  // const DeviceInterfaceDispatchers dispatchers_;
  // Only used to keep a network device shim alive during the device's lifetime.
  // std::unique_ptr<NetworkDeviceImplBinder> binder_;
  // std::optional<fdf::ServerBindingRef<netdriver::NetworkDeviceIfc>> ifc_binding_;
  ddk::NetworkDeviceImplProtocolClient device_impl_;
  std::array<rx_acceleration_t, MAX_ACCEL_FLAGS> accel_rx_;
  std::array<tx_acceleration_t, MAX_ACCEL_FLAGS> accel_tx_;

  std::unique_ptr<Session> primary_session_ __TA_GUARDED(control_lock_);
  SessionList sessions_ __TA_GUARDED(control_lock_);
  uint32_t active_primary_sessions_ __TA_GUARDED(control_lock_) = 0;

  struct PortSlot {
    std::unique_ptr<DevicePort> port;
    uint8_t salt;
  };
  std::array<PortSlot, MAX_PORTS> ports_ __TA_GUARDED(control_lock_);

  SessionList dead_sessions_ __TA_GUARDED(control_lock_);

  // We don't need to keep any data associated with the VMO ids, we use the slab to guarantee
  // non-overlapping unique identifiers within a set of valid IDs.
  DataVmoStore vmo_store_ __TA_GUARDED(control_lock_);
  // BindingList bindings_ __TA_GUARDED(control_lock_);
  PortWatcher::List port_watchers_ __TA_GUARDED(control_lock_);

  TeardownState teardown_state_ __TA_GUARDED(control_lock_) = TeardownState::RUNNING;
  fit::callback<void()> teardown_callback_ __TA_GUARDED(control_lock_);

  PendingDeviceOperation pending_device_op_ = PendingDeviceOperation::NONE;
  std::atomic_bool has_listen_sessions_ = false;

  std::unique_ptr<TxQueue> tx_queue_;
  std::unique_ptr<RxQueue> rx_queue_;

  DeviceStatus device_status_ __TA_GUARDED(control_lock_) = DeviceStatus::STOPPED;

  // std::optional<netdev::DelegatedRxLease> rx_lease_pending_ __TA_GUARDED(rx_lock_);

  fbl::Mutex rx_lock_;
  fbl::Mutex tx_lock_ __TA_ACQUIRED_AFTER(rx_lock_);
  SharedLock control_lock_ __TA_ACQUIRED_AFTER(tx_lock_, rx_lock_);

  // Event hooks used in tests:
  // EventHook<fit::function<void(const char*)>> evt_session_started_;
  // NB: This will be called with control_lock_ held.
  // EventHook<fit::function<void(const char*)>> evt_session_died_;
  // EventHook<fit::function<void(uint64_t)>> evt_rx_queue_packet_;
  // EventHook<fit::function<void()>> evt_tx_complete_;
};

}  // namespace network::internal

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEVICE_INTERFACE_H_
