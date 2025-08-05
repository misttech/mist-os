// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::parser::{PolicyCursor, PolicyData, PolicyOffset};
use crate::policy::{Counted, Parse};
use std::marker::PhantomData;
use zerocopy::FromBytes;

#[derive(Debug, Clone, Copy)]
pub struct View<T> {
    phantom: PhantomData<T>,
    start: PolicyOffset,
    end: PolicyOffset,
}

impl<T> View<T> {
    pub fn new(start: PolicyOffset, end: PolicyOffset) -> Self {
        Self { phantom: PhantomData, start, end }
    }
}

impl<T: Sized> View<T> {
    pub fn at(start: PolicyOffset) -> Self {
        let end = start + std::mem::size_of::<T>() as u32;
        Self::new(start, end)
    }
}

impl<T: FromBytes + Sized> View<T> {
    pub fn read(&self, policy_data: &PolicyData) -> T {
        debug_assert_eq!(self.end - self.start, std::mem::size_of::<T>() as u32);
        let start = self.start as usize;
        let end = self.end as usize;
        T::read_from_bytes(&policy_data[start..end]).unwrap()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ArrayDataView<D> {
    phantom: PhantomData<D>,
    start: PolicyOffset,
    count: u32,
}

impl<D> ArrayDataView<D> {
    pub fn new(start: PolicyOffset, count: u32) -> Self {
        Self { phantom: PhantomData, start, count }
    }

    pub fn iter(self, policy_data: &PolicyData) -> ArrayDataViewIter<D> {
        let cursor = PolicyCursor::new_at(policy_data.clone(), self.start);
        ArrayDataViewIter::new(cursor, self.count)
    }
}

pub struct ArrayDataViewIter<D> {
    phantom: PhantomData<D>,
    cursor: PolicyCursor,
    remaining: u32,
}

impl<T> ArrayDataViewIter<T> {
    pub(crate) fn new(cursor: PolicyCursor, remaining: u32) -> Self {
        Self { phantom: PhantomData, cursor, remaining }
    }
}

impl<D: Parse> std::iter::Iterator for ArrayDataViewIter<D> {
    type Item = D;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining > 0 {
            let (data, cursor) = D::parse(self.cursor.clone())
                .map_err(Into::<anyhow::Error>::into)
                .expect("policy should be parseable");
            self.cursor = cursor;
            self.remaining -= 1;
            Some(data)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ArrayView<M, D> {
    phantom: PhantomData<(M, D)>,
    start: PolicyOffset,
    count: u32,
}

impl<M, D> ArrayView<M, D> {
    pub fn new(start: PolicyOffset, count: u32) -> Self {
        Self { phantom: PhantomData, start, count }
    }
}

impl<M: Sized, D> ArrayView<M, D> {
    pub fn metadata(&self) -> View<M> {
        View::<M>::at(self.start)
    }

    pub fn data(&self) -> ArrayDataView<D> {
        ArrayDataView::new(self.metadata().end, self.count)
    }
}

fn parse_array_data<D: Parse>(
    cursor: PolicyCursor,
    count: u32,
) -> Result<PolicyCursor, anyhow::Error> {
    let mut tail = cursor;
    for _ in 0..count {
        let (_data, next_tail) = D::parse(tail).map_err(Into::<anyhow::Error>::into)?;
        tail = next_tail;
    }
    Ok(tail)
}

impl<M: Counted + Parse + Sized, D: Parse> Parse for ArrayView<M, D> {
    /// [`ArrayView`] abstracts over two types (`M` and `D`) that may have different [`Parse::Error`]
    /// types. Unify error return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    fn parse(cursor: PolicyCursor) -> Result<(Self, PolicyCursor), Self::Error> {
        let start = cursor.offset();
        let (metadata, cursor) = M::parse(cursor).map_err(Into::<anyhow::Error>::into)?;
        let count = metadata.count();
        let cursor = parse_array_data::<D>(cursor, count)?;
        Ok((Self::new(start, count), cursor))
    }
}
