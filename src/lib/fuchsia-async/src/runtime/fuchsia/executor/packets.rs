// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use rustc_hash::FxHashMap as HashMap;
use std::{ops::Deref, sync::Arc, u64, usize};

use super::common::EHandle;

/// A trait for handling the arrival of a packet on a `zx::Port`.
///
/// This trait should be implemented by users who wish to write their own
/// types which receive asynchronous notifications from a `zx::Port`.
/// Implementors of this trait generally contain a `futures::task::AtomicWaker` which
/// is used to wake up the task which can make progress due to the arrival of
/// the packet.
///
/// `PacketReceiver`s should be registered with a `Core` using the
/// `register_receiver` method on `Core`, `Handle`, or `Remote`.
/// Upon registration, users will receive a `ReceiverRegistration`
/// which provides `key` and `port` methods. These methods can be used to wait on
/// asynchronous signals.
///
/// Note that `PacketReceiver`s may receive false notifications intended for a
/// previous receiver, and should handle these gracefully.
pub trait PacketReceiver: Send + Sync + 'static {
    /// Receive a packet when one arrives.
    fn receive_packet(&self, packet: zx::Packet);
}

// Simple slab::Slab replacement that doesn't re-use keys
// TODO(https://fxbug.dev/42119369): figure out how to safely cancel async waits so we can re-use keys again.
pub(crate) struct PacketReceiverMap<T> {
    next_key: usize,
    pub mapping: HashMap<usize, T>,
}

impl<T> PacketReceiverMap<T> {
    pub fn new() -> Self {
        Self { next_key: 0, mapping: HashMap::default() }
    }

    pub fn get(&self, key: usize) -> Option<&T> {
        self.mapping.get(&key)
    }

    pub fn insert(&mut self, val: T) -> usize {
        let key = self.next_key;
        self.next_key = self.next_key.checked_add(1).expect("ran out of keys");
        self.mapping.insert(key, val);
        key
    }

    pub fn remove(&mut self, key: usize) -> T {
        self.mapping.remove(&key).unwrap_or_else(|| panic!("invalid key"))
    }

    pub fn contains(&self, key: usize) -> bool {
        self.mapping.contains_key(&key)
    }
}

/// A registration of a `PacketReceiver`.
/// When dropped, it will automatically deregister the `PacketReceiver`.
// NOTE: purposefully does not implement `Clone`.
#[derive(Debug)]
pub struct ReceiverRegistration<T: PacketReceiver> {
    pub(super) receiver: Arc<T>,
    pub(super) ehandle: EHandle,
    pub(super) key: u64,
}

impl<T> ReceiverRegistration<T>
where
    T: PacketReceiver,
{
    /// The key with which `Packet`s destined for this receiver should be sent on the `zx::Port`.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// The internal `PacketReceiver`.
    pub fn receiver(&self) -> &T {
        &*self.receiver
    }

    /// The `zx::Port` on which packets destined for this `PacketReceiver` should be queued.
    pub fn port(&self) -> &zx::Port {
        self.ehandle.port()
    }
}

impl<T: PacketReceiver> Deref for ReceiverRegistration<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.receiver()
    }
}

impl<T> Drop for ReceiverRegistration<T>
where
    T: PacketReceiver,
{
    fn drop(&mut self) {
        self.ehandle.deregister_receiver(self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_receiver_map_does_not_reuse_keys() {
        #[derive(Debug, Copy, Clone, PartialEq)]
        struct DummyPacketReceiver {
            id: i32,
        }
        let mut map = PacketReceiverMap::<DummyPacketReceiver>::new();
        let e1 = DummyPacketReceiver { id: 1 };
        assert_eq!(map.insert(e1), 0);
        assert_eq!(map.insert(e1), 1);

        // Still doesn't reuse IDs after one is removed
        map.remove(1);
        assert_eq!(map.insert(e1), 2);
    }
}
