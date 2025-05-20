// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use slab::Slab;

// Bin is a epoch-like data structure for dropping things once all previous references have been
// released.  For the C interface we hand out unowned VMOs whose lifetime is tied to that of the
// request.  Rather than retain all the VMOs that might be used by a request, we just hold on to
// closed VMOs until *all* preceding requests have finished.
pub struct Bin<T> {
    bin: Slab<BinEntry<T>>,
    current_key: usize,
}

struct BinEntry<T> {
    ref_count: usize,
    data_and_next: Option<(T, usize)>,
}

impl<T> Bin<T> {
    pub fn new() -> Self {
        Self {
            bin: Slab::from_iter([(0, BinEntry { ref_count: 0, data_and_next: None })]),
            current_key: 0,
        }
    }

    /// Retains the current epoch and returns the key for it.  The caller must later call `release`.
    pub fn retain(&mut self) -> usize {
        self.bin[self.current_key].ref_count += 1;
        self.current_key
    }

    /// Releases a reference on the epoch with the specified key.
    pub fn release(&mut self, mut key: usize) {
        loop {
            let entry = &mut self.bin[key];
            entry.ref_count -= 1;
            // If the reference count has reached zero and there's something to drop, drop it now.
            // This entry holds a reference to the next entry, so we loop round and drop that
            // reference.
            match entry.data_and_next {
                Some((_, next)) if entry.ref_count == 0 => {
                    self.bin.remove(key);
                    key = next;
                }
                _ => return,
            }
        }
    }

    /// Adds something to be dropped when all previous references are released.
    pub fn add(&mut self, data: T) {
        if self.bin[self.current_key].ref_count == 0 {
            return;
        }
        let next = self.bin.insert(BinEntry {
            ref_count: 1, // The current entry keeps a reference to the next one.
            data_and_next: None,
        });
        self.bin[self.current_key].data_and_next = Some((data, next));
        self.current_key = next;
    }
}

#[cfg(test)]
mod tests {
    use super::Bin;
    use std::sync::Arc;

    #[test]
    fn test_data_dropped_immediately_when_no_references() {
        let data = Arc::new(());
        let mut bin = Bin::new();
        bin.add(data.clone());
        assert_eq!(Arc::strong_count(&data), 1);
    }

    #[test]
    fn test_data_dropped_when_reference_count_reaches_zero() {
        let data = Arc::new(());
        let mut bin = Bin::new();
        let key = bin.retain();
        bin.add(data.clone());
        assert_eq!(Arc::strong_count(&data), 2);
        bin.release(key);
        assert_eq!(Arc::strong_count(&data), 1);
    }

    #[test]
    fn test_data_in_later_epoch_dropped_after_earlier_epochs_are_dropped() {
        let data1 = Arc::new(());
        let data2 = Arc::new(());
        let mut bin = Bin::new();
        let key1 = bin.retain();
        bin.add(data1.clone());
        let key2 = bin.retain();
        bin.add(data2.clone());
        assert_eq!(Arc::strong_count(&data1), 2);
        assert_eq!(Arc::strong_count(&data2), 2);
        bin.release(key2);
        // key2 comes after key1 so nothing should have been dropped.
        assert_eq!(Arc::strong_count(&data1), 2);
        assert_eq!(Arc::strong_count(&data2), 2);
        bin.release(key1);
        // Both should now get dropped.
        assert_eq!(Arc::strong_count(&data1), 1);
        assert_eq!(Arc::strong_count(&data2), 1);
    }
}
