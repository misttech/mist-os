// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use display_utils::CollectionId;
use std::collections::BTreeSet;

pub mod sysmem;

#[cfg(test)]
use std::ops::Range;

// TODO(fxbug.dev/130035): Use display_utils structs instead.
pub type ImageId = u64;

#[derive(Debug)]
pub struct FrameSet {
    // TODO(fxbug.dev/84729)
    #[allow(unused)]
    collection_id: CollectionId,
    image_count: usize,
    available: BTreeSet<ImageId>,
    pub prepared: Option<ImageId>,
    presented: BTreeSet<ImageId>,
}

impl FrameSet {
    pub fn new(collection_id: CollectionId, available: BTreeSet<ImageId>) -> FrameSet {
        FrameSet {
            collection_id,
            image_count: available.len(),
            available,
            prepared: None,
            presented: BTreeSet::new(),
        }
    }

    #[cfg(test)]
    pub fn new_with_range(r: Range<ImageId>) -> FrameSet {
        let mut available = BTreeSet::new();
        for image_id in r {
            available.insert(image_id);
        }
        Self::new(CollectionId(0), available)
    }

    pub fn mark_presented(&mut self, image_id: ImageId) {
        assert!(
            !self.presented.contains(&image_id),
            "Attempted to mark as presented image {} which was already in the presented image set",
            image_id
        );
        self.presented.insert(image_id);
        self.prepared = None;
    }

    pub fn mark_done_presenting(&mut self, image_id: ImageId) {
        assert!(
            self.presented.remove(&image_id),
            "Attempted to mark as freed image {} which was not the presented image",
            image_id
        );
        self.available.insert(image_id);
    }

    pub fn mark_prepared(&mut self, image_id: ImageId) {
        assert!(self.prepared.is_none(), "Trying to mark image {} as prepared when image {} is prepared and has not been presented", image_id, self.prepared.unwrap());
        self.prepared.replace(image_id);
        self.available.remove(&image_id);
    }

    pub fn get_available_image(&mut self) -> Option<ImageId> {
        let first = self.available.iter().next().map(|a| *a);
        if let Some(first) = first {
            self.available.remove(&first);
        }
        first
    }

    pub fn return_image(&mut self, image_id: ImageId) {
        self.available.insert(image_id);
    }

    pub fn no_images_in_use(&self) -> bool {
        self.available.len() == self.image_count
    }
}

#[cfg(test)]
mod frameset_tests {
    use crate::{FrameSet, ImageId};
    use std::ops::Range;

    const IMAGE_RANGE: Range<ImageId> = 200..202;

    #[test]
    #[should_panic]
    fn test_double_prepare() {
        let mut fs = FrameSet::new_with_range(IMAGE_RANGE);

        fs.mark_prepared(100);
        fs.mark_prepared(200);
    }

    #[test]
    #[should_panic]
    fn test_not_presented() {
        let mut fs = FrameSet::new_with_range(IMAGE_RANGE);
        fs.mark_done_presenting(100);
    }

    #[test]
    #[should_panic]
    fn test_already_presented() {
        let mut fs = FrameSet::new_with_range(IMAGE_RANGE);
        fs.mark_presented(100);
        fs.mark_presented(100);
    }

    #[test]
    fn test_basic_use() {
        let mut fs = FrameSet::new_with_range(IMAGE_RANGE);
        let avail = fs.get_available_image();
        assert!(avail.is_some());
        let avail = avail.unwrap();
        assert!(!fs.available.contains(&avail));
        assert!(!fs.presented.contains(&avail));
        fs.mark_prepared(avail);
        assert_eq!(fs.prepared.unwrap(), avail);
        fs.mark_presented(avail);
        assert!(fs.prepared.is_none());
        assert!(!fs.available.contains(&avail));
        assert!(fs.presented.contains(&avail));
        fs.mark_done_presenting(avail);
        assert!(fs.available.contains(&avail));
        assert!(!fs.presented.contains(&avail));
    }
}

// TODO: this should eventually be removed in favor of client adding
// CPU access requirements if needed
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameUsage {
    Cpu,
    Gpu,
}
