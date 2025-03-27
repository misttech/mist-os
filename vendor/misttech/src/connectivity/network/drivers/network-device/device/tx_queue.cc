// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tx_queue.h"

#include <trace.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include "device_interface.h"
#include "fbl/auto_lock.h"
#include "lib/fit/defer.h"
#include "session.h"
#include "zircon/status.h"

#define LOCAL_TRACE 0

namespace network::internal {

struct tx_thread_wrapper_data {
  uint16_t capacity;
  std::unique_ptr<tx_buffer_t[]> buffers;
  std::unique_ptr<BufferParts<buffer_region_t>[]> buffer_parts;
  TxQueue* queue;
};

int tx_thread_wrapper(void* arg) {
  struct tx_thread_wrapper_data* thread_data = (struct tx_thread_wrapper_data*)arg;
  cpp20::span<tx_buffer_t> buffers(thread_data->buffers.get(), thread_data->capacity);
  thread_data->queue->Thread(std::move(buffers));
  delete thread_data;
  return 0;
}

// constexpr char kTxSchedulerRole[] = "fuchsia.devices.network.core.tx";

std::atomic<uint32_t> TxQueue::num_instances_ = 0;

TxQueue::~TxQueue() {
  // running_ is tied to the lifetime of the watch thread, it's cleared in`TxQueue::JoinThread`.
  // This assertion protects us from destruction paths where `TxQueue::JoinThread` is not called.
  ZX_ASSERT_MSG(!running_, "TxQueue destroyed without disposing of port and thread first.");
}

void TxQueue::JoinThread() {
  if (!thread_) {
    return;
  }

  if (zx_status_t status = EnqueueUserPacket(kQuitKey); status != ZX_OK) {
    TRACEF("TxQueue::JoinThread failed to send quit key: %s", zx_status_get_string(status));
  }

  // Mark the queue as not running anymore.
  running_ = false;

  ZX_ASSERT(thread_->Join(nullptr, ZX_TIME_INFINITE) == ZX_OK);
  thread_ = nullptr;

  // dispatcher_.ShutdownAsync();
  // dispatcher_shutdown_.Wait();
  // dispatcher_.reset();
  //--num_instances_;
}

zx_status_t TxQueue::EnqueueUserPacket(uint64_t key) {
  zx_port_packet_t packet;
  packet.type = ZX_PKT_TYPE_USER;
  packet.key = key;
  packet.status = ZX_OK;
  return port_->QueueUser(packet);
}

void TxQueue::SessionTransaction::Commit() {
  // when we destroy a session transaction, we commit all the queued buffers to
  // the device.
  if (queued_ != 0) {
    // Send buffers in batches of at most |MAX_TX_BUFFERS| at a time to stay within the FIDL
    // channel maximum.
    tx_buffer_t* buffers = buffers_.data();
    while (queued_ > 0) {
      const uint32_t batch = std::min(static_cast<uint32_t>(queued_), MAX_TX_BUFFERS);
      queue_->parent_->QueueTx(cpp20::span(buffers, batch));
      buffers += batch;
      queued_ -= batch;
    }
  }
}

tx_buffer_t* TxQueue::SessionTransaction::GetBuffer() {
  ZX_ASSERT(available_ != 0);
  return &buffers_[queued_];
}

void TxQueue::SessionTransaction::Push(uint16_t descriptor) {
  ZX_ASSERT(available_ != 0);
  session_->TxTaken();
  buffers_[queued_].id = queue_->Enqueue(session_, descriptor);
  available_--;
  queued_++;
}

TxQueue::SessionTransaction::SessionTransaction(cpp20::span<tx_buffer_t> buffers, TxQueue* parent,
                                                Session* session)
    : buffers_(buffers), queue_(parent), session_(session), queued_(0) {
  // only get available slots after lock is acquired:
  // 0 available slots if parent is not enabled.
  available_ = queue_->parent_->IsDataPlaneOpen() ? queue_->in_flight_->available() : 0;
}

zx::result<std::unique_ptr<TxQueue>> TxQueue::Create(DeviceInterface* parent) {
  // The Tx queue capacity is based on the underlying device's tx queue capacity.
  auto capacity = parent->info().tx_depth;

  fbl::AllocChecker ac;
  std::unique_ptr<TxQueue> queue(new (&ac) TxQueue(parent));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  fbl::AutoLock lock(&queue->parent_->tx_lock());

  zx::result return_queue = RingQueue<uint32_t>::Create(capacity);
  if (return_queue.is_error()) {
    return return_queue.take_error();
  }
  queue->return_queue_ = std::move(return_queue.value());

  zx::result in_flight = IndexedSlab<InFlightBuffer>::Create(capacity);
  if (in_flight.is_error()) {
    return in_flight.take_error();
  }
  queue->in_flight_ = std::move(in_flight.value());
  std::unique_ptr<tx_buffer_t[]> tx_buffers(new (&ac) tx_buffer_t[capacity]);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  // TODO(https://github.com/llvm/llvm-project/issues/54497): remove unnecessary
  // type extraction once clang-format can deal with [] in template parameter.
  using BufferParts_t = BufferParts<buffer_region_t>[];
  std::unique_ptr<BufferParts_t> buffer_parts(new (&ac) BufferParts<buffer_region_t>[capacity]);
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  for (int i = 0; i < capacity; ++i) {
    tx_buffers[i].data_list = new (&ac) buffer_region_t[capacity];
    if (!ac.check()) {
      return zx::error(ZX_ERR_NO_MEMORY);
    }
  }

  KernelHandle<PortDispatcher> port;
  zx_rights_t rights;
  if (zx_status_t status = PortDispatcher::Create(0, &port, &rights); status != ZX_OK) {
    TRACEF("failed to create tx queue port");
    return zx::error(status);
  }
  queue->port_ = port.release();

  // Make sure the driver framework allows for the creation of the necessary threads. Keep track of
  // the number of TX queue instances globally.
  /*
  uint32_t instances = ++num_instances_;
  if (zx_status_t status =
          fdf_env_set_thread_limit(kTxSchedulerRole, strlen(kTxSchedulerRole), instances);
      status != ZX_OK && status != ZX_ERR_OUT_OF_RANGE) {
    // ZX_ERR_OUT_OF_RANGE indicates that the value is less than the current value. This can happen
    // if a number of threads have recently shut down or two TX queues are being created at the same
    // time and the loading of the atomic and setting of the limit are interleaved. It's safe to
    // ignore that in this context, the important part is that there are enough threads.
    LOGF_ERROR("failed to update thread limit: %s", zx_status_get_string(status));
    return zx::error(status);
  }*/

  // In order to ensure that the async::PostTask below works this has to be a synchronized
  // dispatcher that allows synchronous calls. Any other combination of dispatcher and options could
  // lead to the async::PostTask call being inlined, meaning the task would run on the calling
  // thread, blocking it indefinitely. Use a unique owner to ensure inlining of calls from inside
  // the task. Calls to a dispatcher with the same owner might not be inlined.
  /*auto dispatcher = fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      queue.get(), fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "netdevice:tx_watch",
      [queue = queue.get()](fdf_dispatcher_t*) { queue->dispatcher_shutdown_.Signal(); },
      kTxSchedulerRole);
  if (dispatcher.is_error()) {
    LOGF_ERROR("Failed to create tx dispatcher: %s", dispatcher.status_string());
    return dispatcher.take_error();
  }
  queue->dispatcher_ = std::move(dispatcher.value());

  async::PostTask(queue->dispatcher_.async_dispatcher(),
                  [queue = queue.get(), tx_buffers = std::move(tx_buffers),
                   buffer_parts = std::move(buffer_parts),
                   capacity]() { queue->Thread(cpp20::span(&tx_buffers[0], capacity)); });*/

  auto thread_data = new (&ac) tx_thread_wrapper_data();
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  thread_data->capacity = capacity;
  thread_data->buffers = std::move(tx_buffers);
  thread_data->buffer_parts = std::move(buffer_parts);
  thread_data->queue = queue.get();

  queue->thread_ =
      Thread::Create("netdevice:tx_watch", tx_thread_wrapper, thread_data, DEFAULT_PRIORITY);
  ZX_ASSERT_MSG(queue->thread_, "Thread::Create failed.");
  queue->thread_->Resume();

  queue->running_ = true;

  return zx::ok(std::move(queue));
}

void TxQueue::Thread(cpp20::span<tx_buffer_t> buffers) {
  for (;;) {
    zx_port_packet_t packet;
    zx_status_t status = port_->Dequeue(Deadline::infinite(), &packet);
    if (status != ZX_OK) {
      TRACEF("tx thread port wait failed: %s\n", zx_status_get_string(status));
      return;
    }
    switch (packet.type) {
      case ZX_PKT_TYPE_USER:
        switch (packet.key) {
          case kResumeKey:
            if (status = UpdateFifoWatches(); status != ZX_OK) {
              TRACEF("failed to install FIFO watches: %s\n", zx_status_get_string(status));
              return;
            }
            break;
          case kQuitKey:
            return;
          default:
            ZX_PANIC("unexpected user packet key %ld", packet.key);
        }
        break;
      case ZX_PKT_TYPE_SIGNAL_ONE:
        if (status = HandleFifoSignal(buffers, packet.key, packet.signal.observed);
            status != ZX_OK) {
          TRACEF("failed to handle FIFO signal: %s\n", zx_status_get_string(status));
          return;
        }
        break;
      default:
        ZX_PANIC("unexpected packet type %d", packet.type);
    }
  }
}

zx_status_t TxQueue::UpdateFifoWatches() {
  fbl::AutoLock lock(&parent_->tx_lock());
  for (auto it = sessions_.begin(); it != sessions_.end(); it++) {
    SessionWaiter& waiter = *it;
    Session& session = *waiter.session;

    if (session.IsPaused()) {
      if (!waiter.wait_installed) {
        continue;
      }
      zx_status_t status =
          port_->CancelQueued(session.tx_fifo().get(), it.key()) == true ? ZX_OK : ZX_ERR_NOT_FOUND;
      switch (status) {
        case ZX_OK:
          waiter.wait_installed = false;
          continue;
        default:
          TRACEF("failed to cancel FIFO wait for session %s: %s\n", session.name(),
                 zx_status_get_string(status));
          return status;
      }
    }

    if (waiter.wait_installed) {
      continue;
    }

    HandleOwner fifo_hdl = Handle::Make(session.tx_fifo(), ZX_DEFAULT_FIFO_RIGHTS);
    if (zx_status_t status = port_->MakeObserver(0, fifo_hdl.get(), it.key(),
                                                 ZX_FIFO_PEER_CLOSED | ZX_FIFO_READABLE);
        status != ZX_OK) {
      TRACEF("failed to start FIFO wait for session %s: %s\n", session.name(),
             zx_status_get_string(status));
      return status;
    }
    fifo_hdl.release();

    waiter.wait_installed = true;
  }

  return ZX_OK;
}

zx_status_t TxQueue::HandleFifoSignal(cpp20::span<tx_buffer_t> buffers, SessionKey key,
                                      zx_signals_t signals) {
  fbl::AutoLock lock(&parent_->tx_lock());
  SessionWaiter* find_session = sessions_.Get(key);
  // Session already removed from Tx queue, packet was lingering in the port.
  if (find_session == nullptr) {
    return ZX_OK;
  }

  SessionWaiter& waiter = *find_session;
  waiter.wait_installed = false;
  Session& session = *waiter.session;
  const fbl::RefPtr<FifoDispatcher>& fifo = session.tx_fifo();
  SessionTransaction transaction(buffers, this, &session);

  // TA really doesn't like defers or its interplay with AutoLock.
  auto defer = fit::defer([&transaction, &lock]() __TA_NO_THREAD_SAFETY_ANALYSIS {
    // Committing the transaction must not be holding any locks, because we call
    // into the device.
    lock.release();
    transaction.Commit();
  });

  if (signals & ZX_FIFO_READABLE) {
    session.AssertParentTxLock(*parent_);
    zx_status_t status = session.FetchTx(transaction);
    switch (status) {
      case ZX_OK:
      case ZX_ERR_SHOULD_WAIT:
        break;
      case ZX_ERR_PEER_CLOSED:
        // FIFO is closed, don't reinstall the wait.
      case ZX_ERR_IO_OVERRUN:
        // We've run out of device buffers, don't reinstall the wait until we're
        // notified of new buffers.
        return ZX_OK;
      default:
        TRACEF("unexpected error for session %s: %s; killing it\n", session.name(),
               zx_status_get_string(status));
        session.Kill();
        return ZX_OK;
    }
  }

  if (signals & ZX_FIFO_PEER_CLOSED) {
    // FIFO is closed, don't reinstall the wait.
    return ZX_OK;
  }

  HandleOwner fifo_hdl = Handle::Make(fifo, ZX_DEFAULT_FIFO_RIGHTS);
  if (zx_status_t status =
          port_->MakeObserver(0, fifo_hdl.get(), key, ZX_FIFO_PEER_CLOSED | ZX_FIFO_READABLE);
      status != ZX_OK) {
    TRACEF("failed to start FIFO wait for session %s: %s\n", session.name(),
           zx_status_get_string(status));
    return status;
  }
  fifo_hdl.release();

  waiter.wait_installed = true;

  return ZX_OK;
}

void TxQueue::Resume() {
  if (!running_) {
    return;
  }

  if (zx_status_t status = EnqueueUserPacket(kResumeKey); status != ZX_OK) {
    TRACEF("TxQueue::Resume failed: %s\n", zx_status_get_string(status));
  }
}

uint32_t TxQueue::Enqueue(Session* session, uint16_t descriptor) {
  return in_flight_->Push(InFlightBuffer(session, ZX_OK, descriptor));
}

bool TxQueue::ReturnBuffers() {
  size_t count = 0;

  uint16_t desc_buffer[kMaxFifoDepth];
  ZX_ASSERT(return_queue_->count() <= kMaxFifoDepth);

  uint16_t* descs = desc_buffer;
  Session* cur_session = nullptr;
  // record if the queue was full, meaning that sessions may have halted fetching tx frames due
  // to overruns:
  bool was_full = in_flight_->available() == 0;
  while (return_queue_->count()) {
    auto& buff = in_flight_->Get(return_queue_->Peek());
    if (cur_session != nullptr && buff.session != cur_session) {
      // dispatch accumulated to session. This should only happen if we have outstanding
      // return buffers from different sessions.
      cur_session->ReturnTxDescriptors(desc_buffer, count);
      count = 0;
      descs = desc_buffer;
    }
    cur_session = buff.session;
    cur_session->MarkTxReturnResult(buff.descriptor_index, buff.result);
    *descs++ = buff.descriptor_index;
    count++;
    // pop from return queue and free space in in_flight queue:
    in_flight_->Free(return_queue_->Pop());
  }

  if (cur_session && count != 0) {
    cur_session->ReturnTxDescriptors(desc_buffer, count);
  }

  return was_full;
}

void TxQueue::CompleteTxList(const cpp20::span<const tx_result_t>& tx_results) {
  fbl::AutoLock lock(&parent_->tx_lock());
  for (const auto& tx : tx_results) {
    InFlightBuffer& buff = in_flight_->Get(tx.id);
    buff.result = tx.status;
    return_queue_->Push(tx.id);
  }
  bool was_full = ReturnBuffers();
  parent_->NotifyTxReturned(was_full);
  // parent_->NotifyTxComplete();
}

TxQueue::SessionKey TxQueue::AddSession(Session* session) {
  sessions_.Grow();
  std::optional new_key = sessions_.Push(SessionWaiter{
      .session = session,
      .wait_installed = false,
  });
  // We grew the slab before pushing, we must have space.
  ZX_ASSERT(new_key.has_value());

  TxQueue::SessionKey key = new_key.value();

  if (zx_status_t status = EnqueueUserPacket(kResumeKey); status != ZX_OK) {
    TRACEF("failed to notify of new session %s with key %ld: %s\n", session->name(), key,
           zx_status_get_string(status));
  }

  return key;
}

void TxQueue::RemoveSession(SessionKey key) {
  std::optional s = sessions_.Erase(key);
  ZX_ASSERT_MSG(s.has_value(), "attempted to remove unknown session %ld", key);
}

}  // namespace network::internal
