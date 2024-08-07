// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod simple8b_rle;

pub(crate) use simple8b_rle::Simple8bRleRingBuffer;

use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct RingBuffer<T> {
    buffer: VecDeque<T>,
    capacity: usize,
}

impl<T> RingBuffer<T> {
    pub const fn new(capacity: usize) -> Self {
        Self { buffer: VecDeque::new(), capacity }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn push(&mut self, value: T) -> Option<T> {
        let mut popped = None;
        if self.len() >= self.capacity() {
            popped = self.buffer.pop_front();
        }
        self.buffer.push_back(value);
        popped
    }

    pub fn pop(&mut self) -> Option<T> {
        self.buffer.pop_back()
    }

    pub fn back(&self) -> Option<&T> {
        self.buffer.back()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_empty() {
        let mut ring_buffer = RingBuffer::<u64>::new(3);
        assert_eq!(ring_buffer.len(), 0);
        assert_eq!(ring_buffer.capacity(), 3);
        assert_eq!(ring_buffer.back(), None);
        assert_eq!(ring_buffer.pop(), None);
    }

    #[test]
    fn test_ring_buffer_one_item() {
        let mut ring_buffer = RingBuffer::new(3);
        ring_buffer.push(1337u64);

        assert_eq!(ring_buffer.len(), 1);
        assert_eq!(ring_buffer.back(), Some(&1337));
        assert_eq!(ring_buffer.pop(), Some(1337));
        assert_eq!(ring_buffer.back(), None);
    }

    #[test]
    fn test_ring_buffer_full() {
        let mut ring_buffer = RingBuffer::new(3);
        ring_buffer.push(1u64);
        ring_buffer.push(2);
        ring_buffer.push(1337);

        assert_eq!(ring_buffer.len(), 3);

        // Length doesn't change when pushing new item past the capacity, and
        // the oldest item is evicted.
        assert_eq!(ring_buffer.push(42), Some(1));
        assert_eq!(ring_buffer.len(), 3);

        assert_eq!(ring_buffer.pop(), Some(42));
        assert_eq!(ring_buffer.pop(), Some(1337));
        assert_eq!(ring_buffer.pop(), Some(2));
    }
}
