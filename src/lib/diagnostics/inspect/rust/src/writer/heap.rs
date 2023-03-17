// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implements the buddy allocation algorithm for the [Inspect VMO][inspect-vmo]
//!
//! [inspect-vmo]: https://fuchsia.dev/fuchsia-src/reference/diagnostics/inspect/vmo-format

use crate::writer::Error;
use inspect_format::{
    constants, utils, Block, BlockIndex, BlockType, PtrEq, ReadBytes, WriteBytes,
};
use num_traits::ToPrimitive;
use std::{cmp::min, convert::TryInto};

/// The inspect heap.
#[derive(Debug)]
pub struct Heap<T> {
    container: T,
    current_size_bytes: usize,
    free_head_per_order: [BlockIndex; constants::NUM_ORDERS as usize],
    allocated_blocks: usize,
    deallocated_blocks: usize,
    failed_allocations: usize,
    header: Option<Block<T>>,
}

impl<T: ReadBytes + WriteBytes + PtrEq + Clone> Heap<T> {
    /// Creates a new heap on the underlying mapped VMO.
    pub fn new(container: T) -> Result<Self, Error> {
        let mut heap = Heap {
            container,
            current_size_bytes: 0,
            free_head_per_order: [BlockIndex::EMPTY; constants::NUM_ORDERS as usize],
            allocated_blocks: 0,
            deallocated_blocks: 0,
            failed_allocations: 0,
            header: None,
        };
        heap.grow_heap(constants::PAGE_SIZE_BYTES)?;
        Ok(heap)
    }

    /// Returns the current size of this heap in bytes.
    pub fn current_size(&self) -> usize {
        self.current_size_bytes
    }

    /// Returns the maximum size of this heap in bytes.
    pub fn maximum_size(&self) -> usize {
        self.container.len()
    }

    /// Returns the number of blocks allocated since the creation of this heap.
    pub fn total_allocated_blocks(&self) -> usize {
        self.allocated_blocks
    }

    /// Returns the number blocks deallocated since the creation of this heap.
    pub fn total_deallocated_blocks(&self) -> usize {
        self.deallocated_blocks
    }

    /// Returns the number of failed allocations since the creation of this heap.
    pub fn failed_allocations(&self) -> usize {
        self.failed_allocations
    }

    /// Allocates a new block of the given `min_size`.
    pub fn allocate_block(&mut self, min_size: usize) -> Result<Block<T>, Error> {
        let min_fit_order = utils::fit_order(min_size);
        if min_fit_order >= constants::NUM_ORDERS as usize {
            return Err(Error::InvalidBlockOrder(min_fit_order));
        }
        let min_fit_order = min_fit_order as u8;
        // Find free block with order >= min_fit_order
        let order_found = (min_fit_order..constants::NUM_ORDERS)
            .find(|&i| self.is_free_block(self.free_head_per_order[i as usize], i));
        let next_order = match order_found {
            Some(order) => order,
            None => {
                self.grow_heap(self.current_size_bytes + constants::PAGE_SIZE_BYTES)?;
                constants::NUM_ORDERS - 1
            }
        };
        let next_into = self.free_head_per_order[next_order as usize];
        let mut block = self.get_block(next_into).unwrap();
        while block.order() > min_fit_order {
            self.split_block(&mut block)?;
        }
        self.remove_free(&block)?;
        block.become_reserved().expect("Failed to reserve make block reserved");
        self.allocated_blocks += 1;
        Ok(block)
    }

    /// Marks the memory region pointed by the given `block` as free.
    pub fn free_block(&mut self, block: Block<T>) -> Result<(), Error> {
        let block_type = block.block_type();
        if block_type == BlockType::Free {
            return Err(Error::BlockAlreadyFree(block.index()));
        }
        let mut buddy_index = self.buddy(block.index(), block.order());
        let mut buddy_block = self.get_block(buddy_index).unwrap();
        let mut block_to_free = block;
        while buddy_block.block_type() == BlockType::Free
            && block_to_free.order() < constants::NUM_ORDERS - 1
            && block_to_free.order() == buddy_block.order()
        {
            self.remove_free(&buddy_block)?;
            if buddy_block.index() < block_to_free.index() {
                block_to_free.swap(&mut buddy_block)?;
            }
            block_to_free.set_order(block_to_free.order() + 1)?;
            buddy_index = self.buddy(block_to_free.index(), block_to_free.order());
            buddy_block = self.get_block(buddy_index).unwrap();
        }
        block_to_free.become_free(self.free_head_per_order[block_to_free.order() as usize]);
        self.free_head_per_order[block_to_free.order() as usize] = block_to_free.index();
        self.deallocated_blocks += 1;
        Ok(())
    }

    /// Returns the block at the given `index` or an error if the index is out of bounds.
    pub fn get_block(&self, index: BlockIndex) -> Result<Block<T>, Error> {
        let offset = index.offset();
        if offset >= self.current_size_bytes {
            return Err(Error::invalid_index(index, "offset exceeds current size"));
        }
        let block = Block::new(self.container.clone(), index);
        if self.current_size_bytes - offset < utils::order_to_size(block.order()) {
            return Err(Error::invalid_index(index, "order exceeds current size"));
        }
        Ok(block)
    }

    /// Returns a copy of the bytes stored in this Heap.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut result = vec![0u8; self.current_size_bytes];
        self.container.read_at(0, &mut result[..]);
        result
    }

    pub fn set_header_block(&mut self, header: &mut Block<T>) -> Result<(), Error> {
        header.set_header_vmo_size(self.current_size_bytes.try_into().unwrap())?;
        self.header = Some(header.clone());
        Ok(())
    }

    fn grow_heap(&mut self, requested_size: usize) -> Result<(), Error> {
        let container_size = self.container.len();
        if requested_size > container_size || requested_size > constants::MAX_VMO_SIZE {
            self.failed_allocations += 1;
            return Err(Error::HeapMaxSizeReached);
        }
        let new_size = min(container_size, requested_size);
        let min_index = BlockIndex::from_offset(self.current_size_bytes);
        let mut last_index = self.free_head_per_order[(constants::NUM_ORDERS - 1) as usize];
        let mut curr_index =
            BlockIndex::from_offset(new_size - new_size % constants::PAGE_SIZE_BYTES);
        loop {
            curr_index -= BlockIndex::from_offset(constants::MAX_ORDER_SIZE);
            Block::new_free(
                self.container.clone(),
                curr_index,
                constants::NUM_ORDERS - 1,
                last_index,
            )
            .expect("Failed to create free block");
            last_index = curr_index;
            if curr_index <= min_index {
                break;
            }
        }
        self.free_head_per_order[(constants::NUM_ORDERS - 1) as usize] = last_index;
        self.current_size_bytes = new_size;
        if let Some(header) = self.header.as_mut() {
            header.set_header_vmo_size(self.current_size_bytes.try_into().unwrap())?;
        }
        Ok(())
    }

    fn is_free_block(&self, index: BlockIndex, expected_order: u8) -> bool {
        if index.to_usize().unwrap() >= self.current_size_bytes / constants::MIN_ORDER_SIZE {
            return false;
        }
        match self.get_block(index) {
            Err(_) => false,
            Ok(block) => block.block_type() == BlockType::Free && block.order() == expected_order,
        }
    }

    fn remove_free(&mut self, block: &Block<T>) -> Result<bool, Error> {
        let order = block.order();
        let index = block.index();
        if order >= constants::NUM_ORDERS {
            return Ok(false);
        }
        let mut next_index = self.free_head_per_order[order as usize];
        if next_index == index {
            self.free_head_per_order[order as usize] = block.free_next_index()?;
            return Ok(true);
        }
        while self.is_free_block(next_index, order) {
            let mut curr_block = self.get_block(next_index).unwrap();
            next_index = curr_block.free_next_index()?;
            if next_index == index {
                curr_block.set_free_next_index(block.free_next_index()?)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn split_block(&mut self, block: &mut Block<T>) -> Result<(), Error> {
        if block.order() >= constants::NUM_ORDERS {
            return Err(Error::InvalidBlockOrderAtIndex(block.order(), block.index()));
        }
        self.remove_free(&block)?;
        let order = block.order();
        let buddy_index = self.buddy(block.index(), order - 1);
        block.set_order(order - 1)?;
        block.become_free(buddy_index);

        let mut buddy = Block::new(self.container.clone(), buddy_index);
        let buddy_order = order - 1;
        buddy.set_order(buddy_order)?;
        buddy.become_free(self.free_head_per_order[buddy_order as usize]);
        self.free_head_per_order[buddy_order as usize] = block.index();
        Ok(())
    }

    fn buddy(&self, index: BlockIndex, order: u8) -> BlockIndex {
        index ^ BlockIndex::from_offset(utils::order_to_size(order))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::snapshot::{BackingBuffer, BlockIterator};
    use inspect_format::{BlockHeader, BlockIndex, Container, Payload, WritableBlockContainer};

    struct BlockDebug {
        index: BlockIndex,
        order: u8,
        block_type: BlockType,
    }

    fn validate<T: PtrEq + WriteBytes + ReadBytes + Clone>(
        expected: &[BlockDebug],
        heap: &Heap<T>,
    ) {
        let buffer = BackingBuffer::Bytes(heap.bytes());
        let actual: Vec<BlockDebug> = BlockIterator::from(&buffer)
            .map(|block| BlockDebug {
                order: block.order(),
                index: block.index(),
                block_type: block.block_type(),
            })
            .collect();
        assert_eq!(expected.len(), actual.len());
        for (i, result) in actual.iter().enumerate() {
            assert_eq!(result.block_type, expected[i].block_type);
            assert_eq!(result.index, expected[i].index);
            assert_eq!(result.order, expected[i].order);
        }
    }

    #[fuchsia::test]
    fn new_heap() {
        let (container, _storage) = Container::read_and_write(4096).unwrap();
        let heap = Heap::new(container).unwrap();
        assert_eq!(heap.current_size_bytes, 4096);
        assert_eq!(heap.free_head_per_order, [BlockIndex::EMPTY; 8]);
        assert!(heap.header.is_none());

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
        assert_eq!(*heap.free_head_per_order[7], 0);
        assert_eq!(*heap.get_block(0.into()).unwrap().free_next_index().unwrap(), 128);
        assert_eq!(*heap.get_block(128.into()).unwrap().free_next_index().unwrap(), 0);
        assert_eq!(heap.failed_allocations, 0);
    }

    #[fuchsia::test]
    fn allocate_and_free() {
        let (container, _storage) = Container::read_and_write(4096).unwrap();
        let mut heap = Heap::new(container).unwrap();

        // Allocate some small blocks and ensure they are all in order.
        for i in 0..=5 {
            let block = heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap();
            assert_eq!(*block.index(), i);
        }

        // Free some blocks. Leaving some in the middle.
        assert!(heap.free_block(heap.get_block(2.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(4.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());

        // Allocate more small blocks and ensure we get the same ones in reverse
        // order.
        let b = heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap();
        assert_eq!(*b.index(), 0);
        let b = heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap();
        assert_eq!(*b.index(), 4);
        let b = heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap();
        assert_eq!(*b.index(), 2);

        // Free everything except the first two.
        assert!(heap.free_block(heap.get_block(4.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(2.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(3.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(5.into()).unwrap()).is_ok());

        let expected = [
            BlockDebug { index: 0.into(), order: 0, block_type: BlockType::Reserved },
            BlockDebug { index: 1.into(), order: 0, block_type: BlockType::Reserved },
            BlockDebug { index: 2.into(), order: 1, block_type: BlockType::Free },
            BlockDebug { index: 4.into(), order: 2, block_type: BlockType::Free },
            BlockDebug { index: 8.into(), order: 3, block_type: BlockType::Free },
            BlockDebug { index: 16.into(), order: 4, block_type: BlockType::Free },
            BlockDebug { index: 32.into(), order: 5, block_type: BlockType::Free },
            BlockDebug { index: 64.into(), order: 6, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
        assert!(heap.free_head_per_order.iter().enumerate().skip(2).all(|(i, &j)| (1 << i) == *j));
        let buffer = BackingBuffer::from(heap.bytes());
        assert!(BlockIterator::from(&buffer).skip(2).all(|b| *b.free_next_index().unwrap() == 0));

        // Ensure a large block takes the first free large one.
        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 128);

        // Free last small allocation, next large takes first half of the
        // buffer.
        assert!(heap.free_block(heap.get_block(1.into()).unwrap()).is_ok());
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 0);

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Reserved },
        ];
        validate(&expected, &heap);

        // Allocate twice in the first half, free in reverse order to ensure
        // freeing works left to right and right to left.
        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());
        let b = heap.allocate_block(1024).unwrap();
        assert_eq!(*b.index(), 0);
        let b = heap.allocate_block(1024).unwrap();
        assert_eq!(*b.index(), 64);
        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(64.into()).unwrap()).is_ok());

        // Ensure freed blocks are merged int a big one and that we can use all
        // space at 0.
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 0);
        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Reserved },
        ];
        validate(&expected, &heap);
        assert_eq!(*heap.free_head_per_order[7], 0);
        assert_eq!(*heap.get_block(0.into()).unwrap().free_next_index().unwrap(), 0);

        assert!(heap.free_block(heap.get_block(128.into()).unwrap()).is_ok());
        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
        assert_eq!(*heap.free_head_per_order[7], 128);
        assert_eq!(*heap.get_block(0.into()).unwrap().free_next_index().unwrap(), 0);
        assert_eq!(*heap.get_block(128.into()).unwrap().free_next_index().unwrap(), 0);
        assert_eq!(heap.failed_allocations, 0);
    }

    #[fuchsia::test]
    fn allocation_counters_work() {
        let (container, _storage) = Container::read_and_write(4096).unwrap();
        let mut heap = Heap::new(container).unwrap();

        let block_count_to_allocate: usize = 50;
        for _ in 0..block_count_to_allocate {
            heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap();
        }

        assert_eq!(heap.total_allocated_blocks(), block_count_to_allocate);

        let block_count_to_free: usize = 5;
        for i in 0..block_count_to_free {
            heap.free_block(heap.get_block((i as u32).into()).unwrap()).unwrap();
        }

        assert_eq!(heap.total_allocated_blocks(), block_count_to_allocate);
        assert_eq!(heap.total_deallocated_blocks(), block_count_to_free);

        for i in block_count_to_free..block_count_to_allocate {
            heap.free_block(heap.get_block((i as u32).into()).unwrap()).unwrap();
        }

        assert_eq!(heap.total_allocated_blocks(), block_count_to_allocate);
        assert_eq!(heap.total_deallocated_blocks(), block_count_to_allocate);
    }

    #[fuchsia::test]
    fn allocate_merge() {
        let (container, _storage) = Container::read_and_write(4096).unwrap();
        let mut heap = Heap::new(container).unwrap();
        for i in 0..=3 {
            let block = heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap();
            assert_eq!(*block.index(), i);
        }

        assert!(heap.free_block(heap.get_block(2.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(1.into()).unwrap()).is_ok());

        let expected = [
            BlockDebug { index: 0.into(), order: 1, block_type: BlockType::Free },
            BlockDebug { index: 2.into(), order: 0, block_type: BlockType::Free },
            BlockDebug { index: 3.into(), order: 0, block_type: BlockType::Reserved },
            BlockDebug { index: 4.into(), order: 2, block_type: BlockType::Free },
            BlockDebug { index: 8.into(), order: 3, block_type: BlockType::Free },
            BlockDebug { index: 16.into(), order: 4, block_type: BlockType::Free },
            BlockDebug { index: 32.into(), order: 5, block_type: BlockType::Free },
            BlockDebug { index: 64.into(), order: 6, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
        assert!(heap.free_head_per_order.iter().enumerate().skip(3).all(|(i, &j)| (1 << i) == *j));
        let buffer = BackingBuffer::from(heap.bytes());
        assert!(BlockIterator::from(&buffer).skip(3).all(|b| *b.free_next_index().unwrap() == 0));
        assert_eq!(*heap.free_head_per_order[1], 0);
        assert_eq!(*heap.free_head_per_order[0], 2);
        assert_eq!(*heap.get_block(0.into()).unwrap().free_next_index().unwrap(), 0);
        assert_eq!(*heap.get_block(2.into()).unwrap().free_next_index().unwrap(), 0);

        assert!(heap.free_block(heap.get_block(3.into()).unwrap()).is_ok());
        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
        assert_eq!(*heap.free_head_per_order[1], 0);
        assert_eq!(*heap.get_block(0.into()).unwrap().free_next_index().unwrap(), 128);
        assert_eq!(*heap.get_block(128.into()).unwrap().free_next_index().unwrap(), 0);
    }

    #[fuchsia::test]
    fn extend() {
        let (container, _storage) = Container::read_and_write(8 * 2048).unwrap();
        let mut heap = Heap::new(container).unwrap();

        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 0);
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 128);
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 256);

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 256.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 384.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
        assert_eq!(*heap.free_head_per_order[7], 384);
        assert_eq!(*heap.get_block(384.into()).unwrap().free_next_index().unwrap(), 0);

        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 384);
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 512);

        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(128.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(256.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(384.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(512.into()).unwrap()).is_ok());

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 256.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 384.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 512.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 640.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
        assert_eq!(heap.current_size_bytes, 2048 * 4 + 4096);
        assert_eq!(*heap.free_head_per_order[7], 512);
        assert_eq!(*heap.get_block(512.into()).unwrap().free_next_index().unwrap(), 384);
        assert_eq!(*heap.get_block(384.into()).unwrap().free_next_index().unwrap(), 256);
        assert_eq!(*heap.get_block(256.into()).unwrap().free_next_index().unwrap(), 128);
        assert_eq!(*heap.get_block(128.into()).unwrap().free_next_index().unwrap(), 0);
        assert_eq!(*heap.get_block(0.into()).unwrap().free_next_index().unwrap(), 640);
        assert_eq!(*heap.get_block(640.into()).unwrap().free_next_index().unwrap(), 0);
        assert_eq!(heap.failed_allocations, 0);
    }

    #[fuchsia::test]
    fn extend_error() {
        let (container, _storage) = Container::read_and_write(4 * 2048).unwrap();
        let mut heap = Heap::new(container).unwrap();

        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 0);
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 128);
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 256);

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 256.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 384.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);

        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 384);
        assert_eq!(heap.failed_allocations, 0);
        assert!(heap.allocate_block(2048).is_err());
        assert_eq!(heap.failed_allocations, 1);
        assert!(heap.allocate_block(2048).is_err());
        assert_eq!(heap.failed_allocations, 2);

        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(128.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(256.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(384.into()).unwrap()).is_ok());

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 256.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 384.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
    }

    #[fuchsia::test]
    fn extend_vmo_greater_max_size() {
        let (container, _storage) =
            Container::read_and_write(constants::MAX_VMO_SIZE + 2048).unwrap();
        let mut heap = Heap::new(container).unwrap();

        for n in 0_u32..(constants::MAX_VMO_SIZE / constants::MAX_ORDER_SIZE).try_into().unwrap() {
            let b = heap.allocate_block(2048).unwrap();
            assert_eq!(*b.index(), n * 128);
        }
        assert_eq!(heap.failed_allocations, 0);
        assert!(heap.allocate_block(2048).is_err());
        assert_eq!(heap.failed_allocations, 1);

        for n in 0_u32..(constants::MAX_VMO_SIZE / constants::MAX_ORDER_SIZE).try_into().unwrap() {
            assert!(heap.free_block(heap.get_block((n * 128).into()).unwrap()).is_ok());
        }
    }

    #[fuchsia::test]
    fn dont_reinterpret_upper_block_contents() {
        let (container, _storage) = Container::read_and_write(4096).unwrap();
        let mut heap = Heap::new(container.clone()).unwrap();

        // Allocate 3 blocks.
        assert_eq!(*heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap().index(), 0);
        let b1 = heap.allocate_block(utils::order_to_size(1)).unwrap();
        assert_eq!(*b1.index(), 2);
        assert_eq!(*heap.allocate_block(utils::order_to_size(1)).unwrap().index(), 4);

        // Write garbage to the second half of the order 1 block in index 2.
        Block::new(container, 3.into()).write(BlockHeader(0xffffffff), Payload(0xffffffff));

        // Free order 1 block in index 2.
        assert!(heap.free_block(b1).is_ok());

        // Allocate small blocks in free order 0 blocks.
        assert_eq!(*heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap().index(), 1);
        assert_eq!(*heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap().index(), 2);

        // This should succeed even if the bytes in this region were garbage.
        assert_eq!(*heap.allocate_block(constants::MIN_ORDER_SIZE).unwrap().index(), 3);

        let expected = [
            BlockDebug { index: 0.into(), order: 0, block_type: BlockType::Reserved },
            BlockDebug { index: 1.into(), order: 0, block_type: BlockType::Reserved },
            BlockDebug { index: 2.into(), order: 0, block_type: BlockType::Reserved },
            BlockDebug { index: 3.into(), order: 0, block_type: BlockType::Reserved },
            BlockDebug { index: 4.into(), order: 1, block_type: BlockType::Reserved },
            BlockDebug { index: 6.into(), order: 1, block_type: BlockType::Free },
            BlockDebug { index: 8.into(), order: 3, block_type: BlockType::Free },
            BlockDebug { index: 16.into(), order: 4, block_type: BlockType::Free },
            BlockDebug { index: 32.into(), order: 5, block_type: BlockType::Free },
            BlockDebug { index: 64.into(), order: 6, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
    }

    #[fuchsia::test]
    fn update_header_vmo_size() {
        let (container, _storage) = Container::read_and_write(3 * 4096).unwrap();
        let mut heap = Heap::new(container).unwrap();
        let mut block = heap
            .allocate_block(inspect_format::utils::order_to_size(constants::HEADER_ORDER))
            .unwrap();
        assert!(block.become_header(heap.current_size()).is_ok());
        assert!(heap.set_header_block(&mut block).is_ok());

        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 128);
        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 256);
        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 384);
        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());

        let expected = [
            BlockDebug { index: 0.into(), order: 1, block_type: BlockType::Header },
            BlockDebug { index: 2.into(), order: 1, block_type: BlockType::Free },
            BlockDebug { index: 4.into(), order: 2, block_type: BlockType::Free },
            BlockDebug { index: 8.into(), order: 3, block_type: BlockType::Free },
            BlockDebug { index: 16.into(), order: 4, block_type: BlockType::Free },
            BlockDebug { index: 32.into(), order: 5, block_type: BlockType::Free },
            BlockDebug { index: 64.into(), order: 6, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 256.into(), order: 7, block_type: BlockType::Reserved },
            BlockDebug { index: 384.into(), order: 7, block_type: BlockType::Reserved },
        ];
        validate(&expected, &heap);

        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 512);
        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());
        let b = heap.allocate_block(2048).unwrap();
        assert_eq!(*b.index(), 640);
        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());
        assert_eq!(heap.failed_allocations, 0);
        assert!(heap.allocate_block(2048).is_err());
        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());
        assert_eq!(heap.failed_allocations, 1);

        assert!(heap.free_block(heap.get_block(128.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(256.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(384.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(512.into()).unwrap()).is_ok());
        assert!(heap.free_block(heap.get_block(640.into()).unwrap()).is_ok());
        assert_eq!(block.header_vmo_size().unwrap().unwrap() as usize, heap.current_size());

        assert!(heap.free_block(heap.get_block(0.into()).unwrap()).is_ok());

        let expected = [
            BlockDebug { index: 0.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 128.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 256.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 384.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 512.into(), order: 7, block_type: BlockType::Free },
            BlockDebug { index: 640.into(), order: 7, block_type: BlockType::Free },
        ];
        validate(&expected, &heap);
    }
}
