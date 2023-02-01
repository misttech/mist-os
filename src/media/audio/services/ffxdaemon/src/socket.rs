// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Helper functions for reading and writing wav files to and from sockets.

use {
    anyhow::Error,
    fuchsia_async as _, fuchsia_zircon as _,
    futures::prelude::*,
    std::io::{Cursor, Seek, SeekFrom, Write},
};

pub struct Socket<'a> {
    pub socket: &'a mut fidl::AsyncSocket,
}

impl<'a> Socket<'a> {
    pub async fn write_wav_header(
        &mut self,
        duration: std::time::Duration,
        format: &format_utils::Format,
    ) -> Result<(), Error> {
        // TODO(fxbug.dev/109807): Support capture until stop.

        // A valid Wav File Header must have the data format and data length fields.
        // We need all values corresponding to wav header fields set on the cursor_writer before
        // writing to stdout.
        let mut cursor_writer = Cursor::new(Vec::<u8>::new());
        {
            // Creation of WavWriter writes the Wav File Header to cursor_writer.
            // This written header has the file size field and data chunk size field both set to 0,
            // since the number of samples (and resulting file and chunk sizes) are unknown to
            // the WavWriter at this point.
            let _writer =
                hound::WavWriter::new(&mut cursor_writer, hound::WavSpec::from(format)).unwrap();
        }

        // The file and chunk size fields are set to 0 as placeholder values by the construction of
        // the WavWriter above. We can compute the actual values based on the command arguments
        // for format and duration, and set the file size and chunk size fields to the computed
        // values in the cursor_writer before writing to stdout.

        let bytes_to_capture: u32 =
            format.frames_in_duration(duration) as u32 * format.bytes_per_frame();
        let total_header_bytes = 44;
        // The File Size field of a WAV header. 32-bit int starting at position 4, represents
        // the size of the overall file minus 8 bytes (exclude RIFF description and file size description)
        let file_size_bytes: u32 = bytes_to_capture as u32 + total_header_bytes - 8;

        cursor_writer.seek(SeekFrom::Start(4))?;
        cursor_writer.write_all(&file_size_bytes.to_le_bytes()[..])?;

        // Data size field of a WAV header. For PCM, this is a 32-bit int starting at position 40,
        // and represents the size of the data section.
        cursor_writer.seek(SeekFrom::Start(40))?;
        cursor_writer.write_all(&bytes_to_capture.to_le_bytes()[..])?;

        // Write the completed WAV header to stdout. We then write the raw sample values from the
        // packets received directly to stdout.
        let header = cursor_writer.into_inner();
        self.socket.write_all(&header).await?;

        Ok(())
    }

    pub async fn read_wav_header(&mut self) -> Result<hound::WavSpec, Error> {
        let spec = {
            let mut header_buf = vec![0u8; 44];
            self.socket.read_exact(&mut header_buf).await?;
            let cursor_header = Cursor::new(header_buf);
            let reader = hound::WavReader::new(cursor_header.clone())?;
            reader.spec()
        };
        Ok(spec)
    }

    // Reads up to buffer size bytes from the socket. Similiar to `std::io::Read::read_exact()`,
    // except that if EOF is encountered before filling the buffer, we still preserve the partially
    // filled buffer and return how many bytes were read from the socket instead of returning an
    // error and leaving the buffer in an unspecified state.
    pub async fn read_until_full(&mut self, buffer: &mut Vec<u8>) -> Result<u64, Error> {
        let mut bytes_read_so_far = 0;

        loop {
            let bytes_read = self.socket.read(&mut buffer[bytes_read_so_far..]).await?;
            bytes_read_so_far += bytes_read;

            if bytes_read == 0 || bytes_read_so_far == buffer.len() as usize {
                break;
            }
        }

        Ok(bytes_read_so_far as u64)
    }
}
