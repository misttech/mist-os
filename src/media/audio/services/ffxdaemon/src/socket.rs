// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Helper functions for reading and writing wav files to and from sockets.

use {
    anyhow::Error, fuchsia_async as _, fuchsia_zircon as _, futures::prelude::*, std::io::Cursor,
};

pub struct Socket<'a> {
    pub socket: &'a mut fidl::AsyncSocket,
}

impl<'a> Socket<'a> {
    pub async fn write_wav_header(
        &mut self,
        duration: Option<std::time::Duration>,
        format: &format_utils::Format,
    ) -> Result<(), Error> {
        let spec = hound::WavSpec::from(format);
        let header = match duration {
            Some(d) => format.wav_header_for_duration(d)?,
            None => spec.into_header_for_infinite_file(),
        };

        self.socket
            .write_all(&header)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write header to socket: {}", e))
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
