// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Fuchsia netdevice session.

mod buffer;

use std::fmt::Debug;
use std::mem::MaybeUninit;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, TryFromIntError};
use std::ops::Range;
use std::pin::Pin;
use std::sync::atomic::{self, AtomicUsize};
use std::sync::Arc;
use std::task::Waker;

use explicit::{PollExt as _, ResultExt as _};
use fidl_fuchsia_hardware_network::DelegatedRxLease;
use fidl_table_validation::ValidFidlTable;
use fuchsia_sync::Mutex;
use futures::future::{poll_fn, Future};
use futures::task::{Context, Poll};
use futures::{ready, Stream};
use {fidl_fuchsia_hardware_network as netdev, fuchsia_async as fasync};

use crate::error::{Error, Result};
use buffer::pool::{Pool, RxLeaseWatcher};
use buffer::{
    AllocKind, DescId, NETWORK_DEVICE_DESCRIPTOR_LENGTH, NETWORK_DEVICE_DESCRIPTOR_VERSION,
};
pub use buffer::{Buffer, Rx, Tx};

/// A session between network device client and driver.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

impl Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { inner } = self;
        let Inner {
            name,
            pool: _,
            proxy: _,
            rx: _,
            tx: _,
            tx_pending: _,
            rx_ready: _,
            tx_ready: _,
            tx_idle_listeners: _,
        } = &**inner;
        f.debug_struct("Session").field("name", &name).finish_non_exhaustive()
    }
}

impl Session {
    /// Creates a new session with the given `name` and `config`.
    pub async fn new(
        device: &netdev::DeviceProxy,
        name: &str,
        config: Config,
    ) -> Result<(Self, Task)> {
        let inner = Inner::new(device, name, config).await?;
        Ok((Session { inner: Arc::clone(&inner) }, Task { inner }))
    }

    /// Sends a [`Buffer`] to the network device in this session.
    pub fn send(&self, buffer: Buffer<Tx>) -> Result<()> {
        self.inner.send(buffer)
    }

    /// Receives a [`Buffer`] from the network device in this session.
    pub async fn recv(&self) -> Result<Buffer<Rx>> {
        self.inner.recv().await
    }

    /// Allocates a [`Buffer`] that may later be queued to the network device.
    ///
    /// The returned buffer will have at least `num_bytes` as size.
    pub async fn alloc_tx_buffer(&self, num_bytes: usize) -> Result<Buffer<Tx>> {
        self.inner.pool.alloc_tx_buffer(num_bytes).await
    }

    /// Waits for at least one TX buffer to be available and returns an iterator
    /// of buffers with `num_bytes` as capacity.
    ///
    /// The returned iterator is guaranteed to yield at least one item (though
    /// it might be an error if the requested size cannot meet the device
    /// requirement).
    ///
    /// # Note
    ///
    /// Given a `Buffer<Tx>` is returned to the pool when it's dropped, the
    /// returned iterator will seemingly yield infinite items if the yielded
    /// `Buffer`s are dropped while iterating.
    pub async fn alloc_tx_buffers(
        &self,
        num_bytes: usize,
    ) -> Result<impl Iterator<Item = Result<Buffer<Tx>>> + '_> {
        self.inner.pool.alloc_tx_buffers(num_bytes).await
    }

    /// Attaches [`Session`] to a port.
    pub async fn attach(&self, port: Port, rx_frames: &[netdev::FrameType]) -> Result<()> {
        // NB: Need to bind the future returned by `proxy.attach` to a variable
        // otherwise this function's (`Session::attach`) returned future becomes
        // not `Send` and we get unexpected compiler errors at a distance.
        //
        // The dyn borrow in the signature of `proxy.attach` seems to be the
        // cause of the compiler's confusion.
        let fut = self.inner.proxy.attach(&port.into(), rx_frames);
        let () = fut.await?.map_err(|raw| Error::Attach(port, zx::Status::from_raw(raw)))?;
        Ok(())
    }

    /// Detaches a port from the [`Session`].
    pub async fn detach(&self, port: Port) -> Result<()> {
        let () = self
            .inner
            .proxy
            .detach(&port.into())
            .await?
            .map_err(|raw| Error::Detach(port, zx::Status::from_raw(raw)))?;
        Ok(())
    }

    /// Blocks until there are no more tx buffers in flight to the backing
    /// device.
    ///
    /// Note that this method does not prevent new buffers from being allocated
    /// and sent, it is up to the caller to prevent any races. This future will
    /// resolve as soon as it observes a tx idle event. That is, there are no
    /// frames in flight to the backing device at all and the session currently
    /// owns all allocated tx buffers.
    ///
    /// The synchronization guarantee provided by this method is that any
    /// buffers previously given to [`Session::send`] will be accounted as
    /// pending until the device has replied back.
    pub async fn wait_tx_idle(&self) {
        self.inner.tx_idle_listeners.wait().await;
    }

    /// Returns a stream of delegated rx leases from the device.
    ///
    /// Leases are yielded from the stream whenever the corresponding receive
    /// buffer is dropped or reused for tx, which marks the end of processing
    /// the marked buffer for the delegated lease.
    ///
    /// See [`fidl_fuchsia_hardware_network::DelegatedRxLease`] for more
    /// details.
    ///
    /// # Panics
    ///
    /// Panics if the session was not created with
    /// [`fidl_fuchsia_hardware_network::SessionFlags::RECEIVE_RX_POWER_LEASES`]
    /// or if `watch_rx_leases` has already been called for this session.
    pub fn watch_rx_leases(&self) -> impl Stream<Item = Result<RxLease>> + Send + Sync {
        let inner = Arc::clone(&self.inner);
        let watcher = RxLeaseWatcher::new(Arc::clone(&inner.pool));
        futures::stream::try_unfold((inner, watcher), |(inner, mut watcher)| async move {
            let DelegatedRxLease {
                hold_until_frame,
                handle,
                __source_breaking: fidl::marker::SourceBreaking,
            } = match inner.proxy.watch_delegated_rx_lease().await {
                Ok(lease) => lease,
                Err(e) => {
                    if e.is_closed() {
                        return Ok(None);
                    } else {
                        return Err(Error::Fidl(e));
                    }
                }
            };
            let hold_until_frame = hold_until_frame.ok_or(Error::InvalidLease)?;
            let handle = RxLease { handle: handle.ok_or(Error::InvalidLease)? };

            watcher.wait_until(hold_until_frame).await;
            Ok(Some((handle, (inner, watcher))))
        })
    }
}

struct Inner {
    pool: Arc<Pool>,
    proxy: netdev::SessionProxy,
    name: String,
    rx: fasync::Fifo<DescId<Rx>>,
    tx: fasync::Fifo<DescId<Tx>>,
    // Pending tx descriptors to be sent.
    tx_pending: Pending<Tx>,
    rx_ready: Mutex<ReadyBuffer<DescId<Rx>>>,
    tx_ready: Mutex<ReadyBuffer<DescId<Tx>>>,
    tx_idle_listeners: TxIdleListeners,
}

impl Inner {
    /// Creates a new session.
    async fn new(device: &netdev::DeviceProxy, name: &str, config: Config) -> Result<Arc<Self>> {
        let (pool, descriptors, data) = Pool::new(config)?;

        let session_info = {
            // The following two constants are not provided by user, panic
            // instead of returning an error.
            let descriptor_length =
                u8::try_from(NETWORK_DEVICE_DESCRIPTOR_LENGTH / std::mem::size_of::<u64>())
                    .expect("descriptor length in 64-bit words not representable by u8");
            netdev::SessionInfo {
                descriptors: Some(descriptors),
                data: Some(data),
                descriptor_version: Some(NETWORK_DEVICE_DESCRIPTOR_VERSION),
                descriptor_length: Some(descriptor_length),
                descriptor_count: Some(config.num_tx_buffers.get() + config.num_rx_buffers.get()),
                options: Some(config.options),
                ..Default::default()
            }
        };

        let (client, netdev::Fifos { rx, tx }) = device
            .open_session(name, session_info)
            .await?
            .map_err(|raw| Error::Open(name.to_owned(), zx::Status::from_raw(raw)))?;
        let proxy = client.into_proxy();
        let rx = fasync::Fifo::from_fifo(rx);
        let tx = fasync::Fifo::from_fifo(tx);

        Ok(Arc::new(Self {
            pool,
            proxy,
            name: name.to_owned(),
            rx,
            tx,
            tx_pending: Pending::new(Vec::new()),
            rx_ready: Mutex::new(ReadyBuffer::new(config.num_rx_buffers.get().into())),
            tx_ready: Mutex::new(ReadyBuffer::new(config.num_tx_buffers.get().into())),
            tx_idle_listeners: TxIdleListeners::new(),
        }))
    }

    /// Polls to submit available rx descriptors from pool to driver.
    ///
    /// Returns the number of rx descriptors that are submitted.
    fn poll_submit_rx(&self, cx: &mut Context<'_>) -> Poll<Result<usize>> {
        self.pool.rx_pending.poll_submit(&self.rx, cx)
    }

    /// Polls completed rx descriptors from the driver.
    ///
    /// Returns the the head of a completed rx descriptor chain.
    fn poll_complete_rx(&self, cx: &mut Context<'_>) -> Poll<Result<DescId<Rx>>> {
        let mut rx_ready = self.rx_ready.lock();
        rx_ready.poll_with_fifo(cx, &self.rx).map_err(|status| Error::Fifo("read", "rx", status))
    }

    /// Polls to submit tx descriptors that are pending to the driver.
    ///
    /// Returns the number of tx descriptors that are successfully submitted.
    fn poll_submit_tx(&self, cx: &mut Context<'_>) -> Poll<Result<usize>> {
        self.tx_pending.poll_submit(&self.tx, cx)
    }

    /// Polls completed tx descriptors from the driver then puts them in pool.
    fn poll_complete_tx(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let result = {
            let mut tx_ready = self.tx_ready.lock();
            // TODO(https://github.com/rust-lang/rust/issues/63569): Provide entire
            // chain of completed descriptors to the pool at once when slice of
            // MaybeUninit is stabilized.
            tx_ready.poll_with_fifo(cx, &self.tx).map(|r| match r {
                Ok(desc) => self.pool.tx_completed(desc),
                Err(status) => Err(Error::Fifo("read", "tx", status)),
            })
        };

        match &result {
            Poll::Ready(Ok(())) => self.tx_idle_listeners.tx_complete(),
            Poll::Pending | Poll::Ready(Err(_)) => {}
        }
        result
    }

    /// Sends the [`Buffer`] to the driver.
    fn send(&self, mut buffer: Buffer<Tx>) -> Result<()> {
        buffer.pad()?;
        buffer.commit();
        self.tx_idle_listeners.tx_started();
        self.tx_pending.extend(std::iter::once(buffer.leak()));
        Ok(())
    }

    /// Receives a [`Buffer`] from the driver.
    ///
    /// Waits until there is completed rx buffers from the driver.
    async fn recv(&self) -> Result<Buffer<Rx>> {
        poll_fn(|cx| -> Poll<Result<Buffer<Rx>>> {
            let head = ready!(self.poll_complete_rx(cx))?;
            Poll::Ready(self.pool.rx_completed(head))
        })
        .await
    }
}

/// The backing task that drives the session.
///
/// A session will stop making progress if this task is not polled continuously.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Task {
    inner: Arc<Inner>,
}

impl Future for Task {
    type Output = Result<()>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = &Pin::into_inner(self).inner;
        loop {
            let mut all_pending = true;
            // TODO(https://fxbug.dev/42158458): poll once for all completed
            // descriptors if this becomes a performance bottleneck.
            while inner.poll_complete_tx(cx)?.is_ready_checked::<()>() {
                all_pending = false;
            }
            if inner.poll_submit_rx(cx)?.is_ready_checked::<usize>() {
                all_pending = false;
            }
            if inner.poll_submit_tx(cx)?.is_ready_checked::<usize>() {
                all_pending = false;
            }
            if all_pending {
                return Poll::Pending;
            }
        }
    }
}

/// Session configuration.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Buffer stride on VMO, in bytes.
    buffer_stride: NonZeroU64,
    /// Number of rx descriptors to allocate.
    num_rx_buffers: NonZeroU16,
    /// Number of tx descriptors to allocate.
    num_tx_buffers: NonZeroU16,
    /// Session flags.
    options: netdev::SessionFlags,
    /// Buffer layout.
    buffer_layout: BufferLayout,
}

/// Describes the buffer layout that [`Pool`] needs to know.
#[derive(Debug, Clone, Copy)]
struct BufferLayout {
    /// Minimum tx buffer data length.
    min_tx_data: usize,
    /// Minimum tx buffer head length.
    min_tx_head: u16,
    /// Minimum tx buffer tail length.
    min_tx_tail: u16,
    /// The length of a buffer.
    length: usize,
}

/// Network device base info with all required fields.
#[derive(Debug, Clone, ValidFidlTable)]
#[fidl_table_src(netdev::DeviceBaseInfo)]
#[fidl_table_strict]
pub struct DeviceBaseInfo {
    /// Maximum number of items in rx FIFO (per session).
    pub rx_depth: u16,
    /// Maximum number of items in tx FIFO (per session).
    pub tx_depth: u16,
    /// Alignment requirement for buffers in the data VMO.
    pub buffer_alignment: u32,
    /// Maximum supported length of buffers in the data VMO, in bytes.
    #[fidl_field_type(optional)]
    pub max_buffer_length: Option<NonZeroU32>,
    /// The minimum rx buffer length required for device.
    pub min_rx_buffer_length: u32,
    /// The minimum tx buffer length required for the device.
    pub min_tx_buffer_length: u32,
    /// The number of bytes the device requests be free as `head` space in a tx buffer.
    pub min_tx_buffer_head: u16,
    /// The amount of bytes the device requests be free as `tail` space in a tx buffer.
    pub min_tx_buffer_tail: u16,
    /// Maximum descriptor chain length accepted by the device.
    pub max_buffer_parts: u8,
    /// Available rx acceleration flags for this device.
    #[fidl_field_type(default)]
    pub rx_accel: Vec<netdev::RxAcceleration>,
    /// Available tx acceleration flags for this device.
    #[fidl_field_type(default)]
    pub tx_accel: Vec<netdev::TxAcceleration>,
}

/// Network device information with all required fields.
#[derive(Debug, Clone, ValidFidlTable)]
#[fidl_table_src(netdev::DeviceInfo)]
#[fidl_table_strict]
pub struct DeviceInfo {
    /// Minimum descriptor length, in 64-bit words.
    pub min_descriptor_length: u8,
    /// Accepted descriptor version.
    pub descriptor_version: u8,
    /// Device base info.
    pub base_info: DeviceBaseInfo,
}

/// Basic session configuration that can be given to [`DeviceInfo`] to generate
/// [`Config`]s.
#[derive(Debug, Copy, Clone)]
pub struct DerivableConfig {
    /// The desired default buffer length for the session.
    pub default_buffer_length: usize,
    /// Create a primary session.
    pub primary: bool,
    /// Enable rx lease watching.
    pub watch_rx_leases: bool,
}

impl DerivableConfig {
    /// A sensibly common default buffer length to be used in
    /// [`DerivableConfig`]. Provided to ease test writing.
    ///
    /// Chosen to be the next power of two after the default Ethernet MTU.
    ///
    /// This is the value of the buffer length in the `Default` impl.
    pub const DEFAULT_BUFFER_LENGTH: usize = 2048;
    /// The value returned by the `Default` impl.
    pub const DEFAULT: Self = Self {
        default_buffer_length: Self::DEFAULT_BUFFER_LENGTH,
        primary: true,
        watch_rx_leases: false,
    };
}

impl Default for DerivableConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl DeviceInfo {
    /// Create a new session config from the device information.
    ///
    /// This method also does the boundary checks so that data_length/offset fields read
    /// from descriptors are safe to convert to [`usize`].
    pub fn make_config(&self, config: DerivableConfig) -> Result<Config> {
        let DeviceInfo {
            min_descriptor_length,
            descriptor_version,
            base_info:
                DeviceBaseInfo {
                    rx_depth,
                    tx_depth,
                    buffer_alignment,
                    max_buffer_length,
                    min_rx_buffer_length,
                    min_tx_buffer_length,
                    min_tx_buffer_head,
                    min_tx_buffer_tail,
                    max_buffer_parts: _,
                    rx_accel: _,
                    tx_accel: _,
                },
        } = self;
        if NETWORK_DEVICE_DESCRIPTOR_VERSION != *descriptor_version {
            return Err(Error::Config(format!(
                "descriptor version mismatch: {} != {}",
                NETWORK_DEVICE_DESCRIPTOR_VERSION, descriptor_version
            )));
        }
        if NETWORK_DEVICE_DESCRIPTOR_LENGTH < usize::from(*min_descriptor_length) {
            return Err(Error::Config(format!(
                "descriptor length too small: {} < {}",
                NETWORK_DEVICE_DESCRIPTOR_LENGTH, min_descriptor_length
            )));
        }

        let DerivableConfig { default_buffer_length, primary, watch_rx_leases } = config;

        let num_rx_buffers =
            NonZeroU16::new(*rx_depth).ok_or_else(|| Error::Config("no RX buffers".to_owned()))?;
        let num_tx_buffers =
            NonZeroU16::new(*tx_depth).ok_or_else(|| Error::Config("no TX buffers".to_owned()))?;

        let max_buffer_length = max_buffer_length
            .and_then(|max| {
                // The error case is the case where max_buffer_length can't fix in a
                // usize, but we use it to compare it to usizes, so that's
                // equivalent to no limit.
                usize::try_from(max.get()).ok_checked::<TryFromIntError>()
            })
            .unwrap_or(usize::MAX);
        let min_buffer_length = usize::try_from(*min_rx_buffer_length)
            .ok_checked::<TryFromIntError>()
            .unwrap_or(usize::MAX);

        let buffer_length =
            usize::min(max_buffer_length, usize::max(min_buffer_length, default_buffer_length));

        let buffer_alignment = usize::try_from(*buffer_alignment).map_err(
            |std::num::TryFromIntError { .. }| {
                Error::Config(format!(
                    "buffer_alignment not representable within usize: {}",
                    buffer_alignment,
                ))
            },
        )?;

        let buffer_stride = buffer_length
            .checked_add(buffer_alignment - 1)
            .map(|x| x / buffer_alignment * buffer_alignment)
            .ok_or_else(|| {
                Error::Config(format!(
                    "not possible to align {} to {} under usize::MAX",
                    buffer_length, buffer_alignment,
                ))
            })?;

        if buffer_stride < buffer_length {
            return Err(Error::Config(format!(
                "buffer stride too small {} < {}",
                buffer_stride, buffer_length
            )));
        }

        if buffer_length < usize::from(*min_tx_buffer_head) + usize::from(*min_tx_buffer_tail) {
            return Err(Error::Config(format!(
                "buffer length {} does not meet minimum tx buffer head/tail requirement {}/{}",
                buffer_length, min_tx_buffer_head, min_tx_buffer_tail,
            )));
        }

        let num_buffers =
            rx_depth.checked_add(*tx_depth).filter(|num| *num != u16::MAX).ok_or_else(|| {
                Error::Config(format!(
                    "too many buffers requested: {} + {} > u16::MAX",
                    rx_depth, tx_depth
                ))
            })?;

        let buffer_stride =
            u64::try_from(buffer_stride).map_err(|std::num::TryFromIntError { .. }| {
                Error::Config(format!("buffer_stride too big: {} > u64::MAX", buffer_stride))
            })?;

        // This is following the practice of rust stdlib to ensure allocation
        // size never reaches isize::MAX.
        // https://doc.rust-lang.org/std/primitive.pointer.html#method.add-1.
        match buffer_stride.checked_mul(num_buffers.into()).map(isize::try_from) {
            None | Some(Err(std::num::TryFromIntError { .. })) => {
                return Err(Error::Config(format!(
                    "too much memory required for the buffers: {} * {} > isize::MAX",
                    buffer_stride, num_buffers
                )))
            }
            Some(Ok(_total)) => (),
        };

        let buffer_stride = NonZeroU64::new(buffer_stride)
            .ok_or_else(|| Error::Config("buffer_stride is zero".to_owned()))?;

        let min_tx_data = match usize::try_from(*min_tx_buffer_length)
            .map(|min_tx| (min_tx <= buffer_length).then_some(min_tx))
        {
            Ok(Some(min_tx_buffer_length)) => min_tx_buffer_length,
            // Either the conversion or the comparison failed.
            Ok(None) | Err(std::num::TryFromIntError { .. }) => {
                return Err(Error::Config(format!(
                    "buffer_length smaller than minimum TX requirement: {} < {}",
                    buffer_length, *min_tx_buffer_length
                )));
            }
        };

        let mut options = netdev::SessionFlags::empty();
        options.set(netdev::SessionFlags::PRIMARY, primary);
        options.set(netdev::SessionFlags::RECEIVE_RX_POWER_LEASES, watch_rx_leases);

        Ok(Config {
            buffer_stride,
            num_rx_buffers,
            num_tx_buffers,
            options,
            buffer_layout: BufferLayout {
                length: buffer_length,
                min_tx_head: *min_tx_buffer_head,
                min_tx_tail: *min_tx_buffer_tail,
                min_tx_data,
            },
        })
    }
}

/// A port of the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Port {
    pub(crate) base: u8,
    pub(crate) salt: u8,
}

impl TryFrom<netdev::PortId> for Port {
    type Error = Error;
    fn try_from(netdev::PortId { base, salt }: netdev::PortId) -> Result<Self> {
        if base <= netdev::MAX_PORTS {
            Ok(Self { base, salt })
        } else {
            Err(Error::InvalidPortId(base))
        }
    }
}

impl From<Port> for netdev::PortId {
    fn from(Port { base, salt }: Port) -> Self {
        Self { base, salt }
    }
}

/// Pending descriptors to be sent to driver.
struct Pending<K: AllocKind> {
    inner: Mutex<(Vec<DescId<K>>, Option<Waker>)>,
}

impl<K: AllocKind> Pending<K> {
    fn new(descs: Vec<DescId<K>>) -> Self {
        Self { inner: Mutex::new((descs, None)) }
    }

    /// Extends the pending descriptors buffer.
    fn extend(&self, descs: impl IntoIterator<Item = DescId<K>>) {
        let mut guard = self.inner.lock();
        let (storage, waker) = &mut *guard;
        storage.extend(descs);
        if let Some(waker) = waker.take() {
            waker.wake();
        }
    }

    /// Submits the pending buffer to the driver through [`zx::Fifo`].
    ///
    /// It will return [`Poll::Pending`] if any of the following happens:
    ///   - There are no descriptors pending.
    ///   - The fifo is not ready for write.
    fn poll_submit(
        &self,
        fifo: &fasync::Fifo<DescId<K>>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<usize>> {
        let mut guard = self.inner.lock();
        let (storage, waker) = &mut *guard;
        if storage.is_empty() {
            *waker = Some(cx.waker().clone());
            return Poll::Pending;
        }

        // TODO(https://fxbug.dev/42107145): We're assuming that writing to the
        // FIFO here is a sufficient memory barrier for the other end to access
        // the data. That is currently true but not really guaranteed by the
        // API.
        let submitted = ready!(fifo.try_write(cx, &storage[..]))
            .map_err(|status| Error::Fifo("write", K::REFL.as_str(), status))?;
        let _drained = storage.drain(0..submitted);
        Poll::Ready(Ok(submitted))
    }
}

/// An intermediary buffer used to reduce syscall overhead by acting as a proxy
/// to read entries from a FIFO.
///
/// `ReadyBuffer` caches read entries from a FIFO in pre-allocated memory,
/// allowing different batch sizes between what is acquired from the FIFO and
/// what's processed by the caller.
struct ReadyBuffer<T> {
    // NB: A vector of `MaybeUninit` here allows us to give a transparent memory
    // layout to the FIFO object but still move objects out of our buffer
    // without needing a `T: Default` implementation. There's a small added
    // benefit of not paying for memory initialization on creation as well, but
    // that's mostly negligible given all allocation is performed upfront.
    data: Vec<MaybeUninit<T>>,
    available: Range<usize>,
}

impl<T> Drop for ReadyBuffer<T> {
    fn drop(&mut self) {
        let Self { data, available } = self;
        for initialized in &mut data[available.clone()] {
            // SAFETY: the available range keeps track of initialized buffers,
            // we must drop them on drop to uphold `MaybeUninit` expectations.
            unsafe { initialized.assume_init_drop() }
        }
        *available = 0..0;
    }
}

impl<T> ReadyBuffer<T> {
    fn new(capacity: usize) -> Self {
        let data = std::iter::from_fn(|| Some(MaybeUninit::uninit())).take(capacity).collect();
        Self { data, available: 0..0 }
    }

    fn poll_with_fifo(
        &mut self,
        cx: &mut Context<'_>,
        fifo: &fuchsia_async::Fifo<T>,
    ) -> Poll<std::result::Result<T, zx::Status>>
    where
        T: fasync::FifoEntry,
    {
        let Self { data, available: Range { start, end } } = self;

        loop {
            // Always pop from available data first.
            if *start != *end {
                let desc = std::mem::replace(&mut data[*start], MaybeUninit::uninit());
                *start += 1;
                // SAFETY: Descriptor was in the initialized section, it was
                // initialized.
                let desc = unsafe { desc.assume_init() };
                return Poll::Ready(Ok(desc));
            }
            // Fetch more from the FIFO.
            let count = ready!(fifo.try_read(cx, &mut data[..]))?;
            *start = 0;
            *end = count;
        }
    }
}

struct TxIdleListeners {
    event: event_listener::Event,
    tx_in_flight: AtomicUsize,
}

impl TxIdleListeners {
    fn new() -> Self {
        Self { event: event_listener::Event::new(), tx_in_flight: AtomicUsize::new(0) }
    }

    /// Decreases the number of outstanding tx buffers by 1.
    ///
    /// Notifies any tx idle listeners if the number reaches 0.
    fn tx_complete(&self) {
        let Self { event, tx_in_flight } = self;
        let old_value = tx_in_flight.fetch_sub(1, atomic::Ordering::SeqCst);
        debug_assert_ne!(old_value, 0);
        if old_value == 1 {
            let _notified: usize = event.notify(usize::MAX);
        }
    }

    /// Increases the number of outstanding tx buffers by 1.
    fn tx_started(&self) {
        let Self { event: _, tx_in_flight } = self;
        let _: usize = tx_in_flight.fetch_add(1, atomic::Ordering::SeqCst);
    }

    async fn wait(&self) {
        let Self { event, tx_in_flight } = self;
        // This is _the correct way_ of holding an `event_listener::Listener`.
        // We check the condition before installing the listener in the fast
        // case, then we must check the condition again after creating the
        // listener in case we've raced with the condition updating. Finally we
        // must loop and check the condition again because we're not fully
        // guaranteed to not have spurious wakeups.
        //
        // See the event_listener crate documentation for more details.
        loop {
            if tx_in_flight.load(atomic::Ordering::SeqCst) == 0 {
                return;
            }

            event_listener::listener!(event => listener);

            if tx_in_flight.load(atomic::Ordering::SeqCst) == 0 {
                return;
            }

            listener.await;
        }
    }
}

/// An RAII lease possibly keeping the system from suspension.
///
/// Yielded from [`Session::watch_rx_leases`].
///
/// Dropping an `RxLease` relinquishes it.
#[derive(Debug)]
pub struct RxLease {
    handle: netdev::DelegatedRxLeaseHandle,
}

impl Drop for RxLease {
    fn drop(&mut self) {
        let Self { handle } = self;
        // Change detector in case we need any evolution on how to relinquish
        // leases.
        match handle {
            netdev::DelegatedRxLeaseHandle::Channel(_channel) => {
                // Dropping the channel is enough to relinquish the lease.
            }
            netdev::DelegatedRxLeaseHandle::__SourceBreaking { .. } => {}
        }
    }
}

impl RxLease {
    /// Peeks the internal lease.
    pub fn inner(&self) -> &netdev::DelegatedRxLeaseHandle {
        &self.handle
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::ops::Deref;
    use std::sync::Arc;
    use std::task::Poll;

    use assert_matches::assert_matches;
    use fuchsia_async::Fifo;
    use test_case::test_case;
    use zerocopy::{FromBytes, Immutable, IntoBytes};
    use zx::{AsHandleRef as _, HandleBased as _};

    use crate::session::DerivableConfig;

    use super::buffer::{
        AllocKind, DescId, NETWORK_DEVICE_DESCRIPTOR_LENGTH, NETWORK_DEVICE_DESCRIPTOR_VERSION,
    };
    use super::{
        BufferLayout, Config, DeviceBaseInfo, DeviceInfo, Error, Inner, Mutex, Pending, Pool,
        ReadyBuffer, Task, TxIdleListeners,
    };

    const DEFAULT_DEVICE_BASE_INFO: DeviceBaseInfo = DeviceBaseInfo {
        rx_depth: 1,
        tx_depth: 1,
        buffer_alignment: 1,
        max_buffer_length: None,
        min_rx_buffer_length: 0,
        min_tx_buffer_head: 0,
        min_tx_buffer_length: 0,
        min_tx_buffer_tail: 0,
        max_buffer_parts: fidl_fuchsia_hardware_network::MAX_DESCRIPTOR_CHAIN,
        rx_accel: Vec::new(),
        tx_accel: Vec::new(),
    };

    const DEFAULT_DEVICE_INFO: DeviceInfo = DeviceInfo {
        min_descriptor_length: 0,
        descriptor_version: 1,
        base_info: DEFAULT_DEVICE_BASE_INFO,
    };

    const DEFAULT_BUFFER_LENGTH: usize = 2048;

    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        min_descriptor_length: u8::MAX,
        ..DEFAULT_DEVICE_INFO
    }, format!("descriptor length too small: {} < {}", NETWORK_DEVICE_DESCRIPTOR_LENGTH, u8::MAX))]
    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        descriptor_version: 42,
        ..DEFAULT_DEVICE_INFO
    }, format!("descriptor version mismatch: {} != {}", NETWORK_DEVICE_DESCRIPTOR_VERSION, 42))]
    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        base_info: DeviceBaseInfo {
            tx_depth: 0,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, "no TX buffers")]
    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        base_info: DeviceBaseInfo {
            rx_depth: 0,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, "no RX buffers")]
    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        base_info: DeviceBaseInfo {
            tx_depth: u16::MAX,
            rx_depth: u16::MAX,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, format!("too many buffers requested: {} + {} > u16::MAX", u16::MAX, u16::MAX))]
    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        base_info: DeviceBaseInfo {
            min_tx_buffer_length: DEFAULT_BUFFER_LENGTH as u32 + 1,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, format!(
        "buffer_length smaller than minimum TX requirement: {} < {}",
        DEFAULT_BUFFER_LENGTH, DEFAULT_BUFFER_LENGTH + 1))]
    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        base_info: DeviceBaseInfo {
            min_tx_buffer_head: DEFAULT_BUFFER_LENGTH as u16 + 1,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, format!(
        "buffer length {} does not meet minimum tx buffer head/tail requirement {}/0",
        DEFAULT_BUFFER_LENGTH, DEFAULT_BUFFER_LENGTH + 1))]
    #[test_case(DEFAULT_BUFFER_LENGTH, DeviceInfo {
        base_info: DeviceBaseInfo {
            min_tx_buffer_tail: DEFAULT_BUFFER_LENGTH as u16 + 1,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, format!(
        "buffer length {} does not meet minimum tx buffer head/tail requirement 0/{}",
        DEFAULT_BUFFER_LENGTH, DEFAULT_BUFFER_LENGTH + 1))]
    #[test_case(0, DEFAULT_DEVICE_INFO, "buffer_stride is zero")]
    #[test_case(usize::MAX, DEFAULT_DEVICE_INFO,
    format!(
        "too much memory required for the buffers: {} * {} > isize::MAX",
        usize::MAX, 2))]
    #[test_case(usize::MAX, DeviceInfo {
        base_info: DeviceBaseInfo {
            buffer_alignment: 2,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, format!(
        "not possible to align {} to {} under usize::MAX",
        usize::MAX, 2))]
    fn configs_from_device_info_err(
        buffer_length: usize,
        info: DeviceInfo,
        expected: impl Deref<Target = str>,
    ) {
        let config = DerivableConfig { default_buffer_length: buffer_length, ..Default::default() };
        assert_matches!(
            info.make_config(config),
            Err(Error::Config(got)) if got.as_str() == expected.deref()
        );
    }

    #[test_case(DeviceInfo {
        base_info: DeviceBaseInfo {
            min_rx_buffer_length: DEFAULT_BUFFER_LENGTH as u32 + 1,
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, DEFAULT_BUFFER_LENGTH + 1; "default below min")]
    #[test_case(DeviceInfo {
        base_info: DeviceBaseInfo {
            max_buffer_length: NonZeroU32::new(DEFAULT_BUFFER_LENGTH as u32 - 1),
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, DEFAULT_BUFFER_LENGTH - 1; "default above max")]
    #[test_case(DeviceInfo {
        base_info: DeviceBaseInfo {
            min_rx_buffer_length: DEFAULT_BUFFER_LENGTH as u32 - 1,
            max_buffer_length: NonZeroU32::new(DEFAULT_BUFFER_LENGTH as u32 + 1),
            ..DEFAULT_DEVICE_BASE_INFO
        },
        ..DEFAULT_DEVICE_INFO
    }, DEFAULT_BUFFER_LENGTH; "default in bounds")]
    fn configs_from_device_buffer_length(info: DeviceInfo, expected_length: usize) {
        let config = info
            .make_config(DerivableConfig {
                default_buffer_length: DEFAULT_BUFFER_LENGTH,
                ..Default::default()
            })
            .expect("is valid");
        let Config {
            buffer_layout: BufferLayout { length, min_tx_data: _, min_tx_head: _, min_tx_tail: _ },
            buffer_stride: _,
            num_rx_buffers: _,
            num_tx_buffers: _,
            options: _,
        } = config;
        assert_eq!(length, expected_length);
    }

    fn make_fifos<K: AllocKind>() -> (Fifo<DescId<K>>, zx::Fifo<DescId<K>>) {
        let (handle, other_end) = zx::Fifo::create(1).unwrap();
        (Fifo::from_fifo(handle), other_end)
    }

    fn remove_rights<T: FromBytes + IntoBytes + Immutable>(
        fifo: Fifo<T>,
        rights_to_remove: zx::Rights,
    ) -> Fifo<T> {
        let fifo = zx::Fifo::from(fifo);
        let rights = fifo.as_handle_ref().basic_info().expect("can retrieve info").rights;

        let fifo = fifo.replace_handle(rights ^ rights_to_remove).expect("can replace");
        Fifo::from_fifo(fifo)
    }

    enum TxOrRx {
        Tx,
        Rx,
    }
    #[test_case(TxOrRx::Tx, zx::Rights::READ; "tx read")]
    #[test_case(TxOrRx::Tx, zx::Rights::WRITE; "tx write")]
    #[test_case(TxOrRx::Rx, zx::Rights::WRITE; "rx read")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn task_as_future_poll_error(which_fifo: TxOrRx, right_to_remove: zx::Rights) {
        // This is a regression test for https://fxbug.dev/42072513. The flake
        // that caused that bug occurred because the Zircon channel was closed
        // but the error returned by a failed attempt to write to it wasn't
        // being propagated upwards. This test produces a similar situation by
        // altering the right on the FIFOs the task uses so as to cause either
        // an attempt to write or to read to fail. For completeness, it
        // exercises all the FIFO polls that comprise Task::poll.
        let (pool, _descriptors, _data) = Pool::new(
            DEFAULT_DEVICE_INFO
                .make_config(DerivableConfig {
                    default_buffer_length: DEFAULT_BUFFER_LENGTH,
                    ..Default::default()
                })
                .expect("is valid"),
        )
        .expect("is valid");
        let (session_proxy, _session_server) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_hardware_network::SessionMarker>();

        let (rx, _rx_sender) = make_fifos();
        let (tx, _tx_receiver) = make_fifos();

        // Attenuate rights on one of the FIFOs.
        let (tx, rx) = match which_fifo {
            TxOrRx::Tx => (remove_rights(tx, right_to_remove), rx),
            TxOrRx::Rx => (tx, remove_rights(rx, right_to_remove)),
        };

        let buf = pool.alloc_tx_buffer(1).await.expect("can allocate");
        let inner = Arc::new(Inner {
            pool,
            proxy: session_proxy,
            name: "fake_task".to_string(),
            rx,
            tx,
            tx_pending: Pending::new(vec![]),
            rx_ready: Mutex::new(ReadyBuffer::new(10)),
            tx_ready: Mutex::new(ReadyBuffer::new(10)),
            tx_idle_listeners: TxIdleListeners::new(),
        });

        inner.send(buf).expect("can send");

        let mut task = Task { inner };

        // The task should not be able to continue because it can't read from or
        // write to one of the FIFOs.
        assert_matches!(futures::poll!(&mut task), Poll::Ready(Err(Error::Fifo(_, _, _))));
    }
}
