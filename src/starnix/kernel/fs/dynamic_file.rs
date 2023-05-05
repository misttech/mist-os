// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::lock::Mutex;

use super::*;
use crate::fs::buffers::{InputBuffer, OutputBuffer, VecOutputBuffer};
use crate::fs::SeekOrigin;
use crate::task::*;
use crate::types::*;

pub trait SequenceFileSource: Send + Sync + 'static {
    type Cursor: Default + Send;
    fn next(
        &self,
        cursor: Self::Cursor,
        sink: &mut SeqFileBuf,
    ) -> Result<Option<Self::Cursor>, Errno>;
}

pub trait DynamicFileSource: Send + Sync + 'static {
    fn generate(&self, sink: &mut SeqFileBuf) -> Result<(), Errno>;
}

impl<T> SequenceFileSource for T
where
    T: DynamicFileSource,
{
    type Cursor = ();
    fn next(&self, _cursor: (), sink: &mut SeqFileBuf) -> Result<Option<()>, Errno> {
        self.generate(sink).map(|_| None)
    }
}

/// DynamicFile implements a file that updates its content dynamically from the `Source`. It's
/// intended to be used mainly for files in procfs.
pub struct DynamicFile<Source: SequenceFileSource> {
    pub source: Source,
    seq_file: Mutex<SeqFileState<Source::Cursor>>,
}

impl<Source: SequenceFileSource> DynamicFile<Source> {
    pub fn new(source: Source) -> Self {
        DynamicFile { source, seq_file: Mutex::new(SeqFileState::default()) }
    }
}

impl<Source: SequenceFileSource + Clone> DynamicFile<Source> {
    pub fn new_node(source: Source) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(DynamicFile::new(source.clone())))
    }
}

impl<Source: SequenceFileSource> FileOps for DynamicFile<Source> {
    fileops_impl_seekable_read!();
    fileops_impl_seekable_write!();

    fn read_at(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.seq_file.lock().read_at(
            current_task,
            |cursor: Source::Cursor, sink: &mut SeqFileBuf| self.source.next(cursor, sink),
            offset,
            data,
        )
    }

    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }

    fn seek(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: off_t,
        whence: SeekOrigin,
    ) -> Result<off_t, Errno> {
        let mut current_offset = file.offset.lock();

        // Only allow absolute and relative forward seek.
        let new_offset = (match whence {
            SeekOrigin::Set => Some(offset),
            SeekOrigin::Cur if offset < 0 => None,
            SeekOrigin::Cur => (*current_offset).checked_add(offset),
            SeekOrigin::End => None,
        })
        .ok_or_else(|| errno!(EINVAL))?;

        // Call `read_at(0)` to ensure the data is generated now instead of later (except, when
        // seeking to the start of the file).
        let mut dummy_buf = VecOutputBuffer::new(0);
        self.read_at(file, current_task, new_offset as usize, &mut dummy_buf)?;

        *current_offset = new_offset;
        Ok(*current_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use std::sync::Arc;

    struct Counter {
        value: Mutex<u8>,
    }

    struct TestFileSource {
        counter: Arc<Counter>,
    }

    impl DynamicFileSource for TestFileSource {
        fn generate(&self, sink: &mut SeqFileBuf) -> Result<(), Errno> {
            let mut counter = self.counter.value.lock();
            let base = *counter;
            // Write 10 bytes where v[i] = base + i.
            let data = (0..10).map(|i| base + i).collect::<Vec<u8>>();
            sink.write(&data);
            *counter += 1;
            Ok(())
        }
    }

    fn create_test_file() -> (Arc<Counter>, CurrentTask, Arc<FileObject>) {
        let counter = Arc::new(Counter { value: Mutex::new(0) });
        let (_kern, current_task) = create_kernel_and_task();
        let file = Anon::new_file(
            &current_task,
            Box::new(DynamicFile::new(TestFileSource { counter: counter.clone() })),
            OpenFlags::RDONLY,
        );
        (counter, current_task, file)
    }

    #[::fuchsia::test]
    fn test_read_at() -> Result<(), Errno> {
        let (_counter, current_task, file) = create_test_file();
        let read_at = |offset: usize, length: usize| -> Result<Vec<u8>, Errno> {
            let mut buffer = VecOutputBuffer::new(length);
            let bytes_read = file.read_at(&current_task, offset, &mut buffer)?;
            Ok(buffer.data()[0..bytes_read].to_vec())
        };

        // Verify that we can read all data to the end.
        assert_eq!(read_at(0, 20)?, (0..10).collect::<Vec<u8>>());

        // Read from the beginning. Content should be refreshed.
        assert_eq!(read_at(0, 2)?, [1, 2]);

        // Continue reading. Content should not be updated.
        assert_eq!(read_at(2, 2)?, [3, 4]);

        // Try reading from a new position. Content should be updated.
        assert_eq!(read_at(5, 2)?, [7, 8]);

        Ok(())
    }

    #[::fuchsia::test]
    fn test_read_and_seek() -> Result<(), Errno> {
        let (counter, current_task, file) = create_test_file();
        let read = |length: usize| -> Result<Vec<u8>, Errno> {
            let mut buffer = VecOutputBuffer::new(length);
            let bytes_read = file.read(&current_task, &mut buffer)?;
            Ok(buffer.data()[0..bytes_read].to_vec())
        };

        // Call `read()` to read the content all the way to the end. Content should not update
        assert_eq!(read(1)?, [0]);
        assert_eq!(read(2)?, [1, 2]);
        assert_eq!(read(20)?, (3..10).collect::<Vec<u8>>());

        // Seek to the start of the file. Content should be updated on the following read.
        file.seek(&current_task, 0, SeekOrigin::Set)?;
        assert_eq!(*counter.value.lock(), 1);
        assert_eq!(read(2)?, [1, 2]);
        assert_eq!(*counter.value.lock(), 2);

        // Seeking to `pos > 0` should update the content.
        file.seek(&current_task, 1, SeekOrigin::Set)?;
        assert_eq!(*counter.value.lock(), 3);

        Ok(())
    }
}
