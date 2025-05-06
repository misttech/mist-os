// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::{NonZeroU32, TryFromIntError};
use std::time::{Duration, Instant};
use std::u64;

use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use thiserror::Error;
use {fidl_fuchsia_developer_ffx_speedtest as fspeedtest, fuchsia_async as fasync};

pub struct Transfer {
    pub socket: fasync::Socket,
    pub params: TransferParams,
}

#[derive(Debug, Clone)]
pub struct TransferParams {
    pub data_len: NonZeroU32,
    pub buffer_len: NonZeroU32,
}

impl TryFrom<fspeedtest::TransferParams> for TransferParams {
    type Error = TryFromIntError;
    fn try_from(value: fspeedtest::TransferParams) -> Result<Self, Self::Error> {
        let fspeedtest::TransferParams { len_bytes, buffer_bytes, __source_breaking } = value;
        Ok(Self {
            data_len: len_bytes.unwrap_or(fspeedtest::DEFAULT_TRANSFER_SIZE).try_into()?,
            buffer_len: buffer_bytes.unwrap_or(fspeedtest::DEFAULT_BUFFER_SIZE).try_into()?,
        })
    }
}

impl TryFrom<TransferParams> for fspeedtest::TransferParams {
    type Error = TryFromIntError;
    fn try_from(value: TransferParams) -> Result<Self, Self::Error> {
        let TransferParams { data_len, buffer_len } = value;
        Ok(Self {
            len_bytes: Some(data_len.try_into()?),
            buffer_bytes: Some(buffer_len.try_into()?),
            __source_breaking: fidl::marker::SourceBreaking,
        })
    }
}

#[derive(Debug)]
pub struct Report {
    pub duration: Duration,
}

impl From<Report> for fspeedtest::TransferReport {
    fn from(value: Report) -> Self {
        let Report { duration } = value;
        Self {
            duration_nsec: Some(duration.as_nanos().try_into().unwrap_or(u64::MAX)),
            __source_breaking: fidl::marker::SourceBreaking,
        }
    }
}

#[derive(Error, Debug)]
#[error("missing mandatory field")]
pub struct MissingFieldError;

impl TryFrom<fspeedtest::TransferReport> for Report {
    type Error = MissingFieldError;

    fn try_from(value: fspeedtest::TransferReport) -> Result<Self, Self::Error> {
        let fspeedtest::TransferReport { duration_nsec, __source_breaking } = value;
        Ok(Self { duration: Duration::from_nanos(duration_nsec.ok_or(MissingFieldError)?) })
    }
}

#[derive(Error, Debug)]
pub enum TransferError {
    #[error(transparent)]
    IntConversion(#[from] TryFromIntError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("remote hung up before terminating transfer")]
    Hangup,
}

impl Transfer {
    pub async fn send(self) -> Result<Report, TransferError> {
        let Self { mut socket, params: TransferParams { data_len, buffer_len } } = self;
        let buffer_len = usize::try_from(buffer_len.get())?;
        let mut data_len = usize::try_from(data_len.get())?;
        let buffer = vec![0xAA; buffer_len];
        let start = Instant::now();
        while data_len != 0 {
            let send = buffer_len.min(data_len);
            let written = socket.write(&buffer[..send]).await?;
            data_len -= written;
        }
        let end = Instant::now();
        Ok(Report { duration: end - start })
    }

    pub async fn receive(self) -> Result<Report, TransferError> {
        let Self { mut socket, params: TransferParams { data_len, buffer_len } } = self;
        let buffer_len = usize::try_from(buffer_len.get())?;
        let mut data_len = usize::try_from(data_len.get())?;
        let mut buffer = vec![0x00; buffer_len];
        let start = Instant::now();
        while data_len != 0 {
            let recv = buffer_len.min(data_len);
            let recv = socket.read(&mut buffer[..recv]).await?;
            if recv == 0 {
                return Err(TransferError::Hangup);
            }
            data_len -= recv;
        }
        let end = Instant::now();
        Ok(Report { duration: end - start })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use assert_matches::assert_matches;

    #[fasync::run_singlethreaded(test)]
    async fn receive_hangup() {
        let (socket, _) = fidl::Socket::create_stream();
        let result = Transfer {
            socket: fasync::Socket::from_socket(socket),
            params: TransferParams {
                data_len: NonZeroU32::new(10).unwrap(),
                buffer_len: NonZeroU32::new(100).unwrap(),
            },
        }
        .receive()
        .await;

        assert_matches!(result, Err(TransferError::Hangup));
    }
}
