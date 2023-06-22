// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use logging::LogFormat;
use rand::Rng;
use std::{
    fs::{create_dir_all, remove_file, rename, File, OpenOptions},
    io::{ErrorKind, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, RwLock,
    },
};
use tracing::Metadata;
use tracing_subscriber::{
    filter::{self, LevelFilter},
    prelude::*,
    Layer,
};

use crate::EnvironmentContext;

const LOG_DIR: &str = "log.dir";
const LOG_ROTATIONS: &str = "log.rotations";
const LOG_ROTATE_SIZE: &str = "log.rotate_size";
const LOG_ENABLED: &str = "log.enabled";
const LOG_TARGET_LEVELS: &str = "log.target_levels";
const LOG_LEVEL: &str = "log.level";
const LOG_INCLUDE_SPANS: &str = "log.include_spans";
pub const LOG_PREFIX: &str = "ffx";

static LOG_ENABLED_FLAG: AtomicBool = AtomicBool::new(true);

lazy_static::lazy_static! {
    static ref LOGGING_ID: u64 = generate_id();
}

pub fn disable_stdio_logging() {
    LOG_ENABLED_FLAG.store(false, Ordering::Relaxed);
}

fn generate_id() -> u64 {
    rand::thread_rng().gen::<u64>()
}

// There are times when we want to change the log file we are writing to. The
// motivating use-case is the self-test subtests, each of which uses Isolates
// which log to different subdirectories.  While the sub-test is running, we'd
// like the parent self-test process to write to a file in the same directory.
// Modifying the log location requires a global (since logging is handled
// globally), and has to be implemented by working with the `tracing_subscriber`
// crate.

/// Global function to change the current log file (if we are logging to a file).
pub fn change_log_file(file: &Path) -> Result<()> {
    if let Some(mut lfh) = log_file_holder() {
        lfh.change_log_file(file)?;
    }
    Ok(())
}

/// Global function to change the reset the current log file back to the
/// original (if we are logging to a file).
pub fn reset_log_file() -> Result<()> {
    if let Some(mut lfh) = log_file_holder() {
        lfh.reset_log_file()?;
    }
    Ok(())
}

// The singleton object that will track the information we need. Along with
// providing the implementation of the global functions, it can return a
// resettable Write object that can be stored in the tracing_subscriber's file
// layer.
#[derive(Debug)]
struct LogFileHolder {
    orig_path: PathBuf,
    writer: Arc<RwLock<File>>,
}

impl LogFileHolder {
    fn new(path: PathBuf, f: File) -> Self {
        Self { orig_path: path, writer: Arc::new(RwLock::new(f)) }
    }

    fn get_resettable_writer(&self) -> ResettableWriter {
        ResettableWriter::new(self.writer.clone())
    }

    fn change_log_file(&mut self, path: &Path) -> Result<()> {
        let file = open_log_file(path)?;
        let mut w = self.writer.write().unwrap_or_else(|e| e.into_inner());
        // Replace the writer.  The underlying File will be flushed and closed.
        *w = file;
        Ok(())
    }

    fn reset_log_file(&mut self) -> Result<()> {
        self.change_log_file(&self.orig_path.clone())?;
        Ok(())
    }
}

static LOG_FILE_HOLDER: OnceLock<Mutex<LogFileHolder>> = OnceLock::new();

// Initializer for the singleton
fn init_log_file_holder(p: PathBuf, f: File) {
    let lfh = LogFileHolder::new(p, f);
    LOG_FILE_HOLDER.set(Mutex::new(lfh)).expect("init_log_file_holder(): OnceLock::set() failed");
}

// Getter for the singleton.  If we were never initialized (e.g. because the logger
// was told not to log to a file), this will return None.
fn log_file_holder() -> Option<MutexGuard<'static, LogFileHolder>> {
    LOG_FILE_HOLDER.get().map(|mx| mx.lock().unwrap_or_else(|e| e.into_inner()))
}

// We need an object whose underlying file we can change, while meeting the
// needs of tracing_subscriber, which wants to own a Writer with a static
// lifetime.  These requirements lead to a File held by a RwLock (because
// the global object can be accessed from multiple threads), inside an Arc
// (because both the ResettableWriter and the LogFileHolder need access). We
// can't replace Arc with a reference, because of the requirement for a static
// lifetime.
struct ResettableWriter {
    w: Arc<RwLock<File>>,
}

impl ResettableWriter {
    fn new(w: Arc<RwLock<File>>) -> Self {
        Self { w }
    }
}

impl Write for ResettableWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut w = self.w.write().unwrap_or_else(|e| e.into_inner());
        Ok(w.write(buf)?)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let mut w = self.w.write().unwrap_or_else(|e| e.into_inner());
        Ok(w.flush()?)
    }
}

fn rotate_file(
    log_rotate_size: Option<u64>,
    log_rotations: u64,
    log_path: &PathBuf,
) -> Result<Option<File>> {
    let mut rot_path = log_path.clone();

    if let Some(log_rotate_size) = log_rotate_size {
        // log.rotate_size was set. We only rotate if the current file is bigger than that size,
        // so open the current file and, if it's smaller than that size, return it.
        match OpenOptions::new().write(true).append(true).create(false).open(log_path) {
            Ok(mut f) => {
                if f.seek(SeekFrom::End(0)).context("checking log file size")? < log_rotate_size {
                    return Ok(Some(f));
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            other => {
                other.context("opening log file")?;
                unreachable!();
            }
        }
    }

    rot_path.set_file_name(format!("{}.{}", log_path.display(), log_rotations - 1));
    match remove_file(&rot_path) {
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        other => other.context("deleting stale log")?,
    }

    for rotation in (0..log_rotations - 1).rev() {
        let prev_path = rot_path.clone();
        rot_path.set_file_name(format!("{}.{}", log_path.display(), rotation));
        match rename(&rot_path, prev_path) {
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            other => other.context("rotating log files")?,
        }
    }

    if let Some(log_rotate_size) = log_rotate_size {
        // When we move the most recent log into rotation, truncate it if it is larger than the
        // rotation length.
        match OpenOptions::new().read(true).create(false).open(log_path) {
            Ok(mut f) => {
                let size = f.seek(SeekFrom::End(0)).context("checking size of old log file")?;
                let log_rotate_size = std::cmp::min(size, log_rotate_size);
                f.seek(SeekFrom::End(-(log_rotate_size as i64)))
                    .context("seeking through old log file")?;
                let mut new = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(rot_path)
                    .context("opening rotating log file")?;
                new.write_all(b"<truncated for length>")
                    .context("writing log truncation notice")?;
                let mut buf = [0; 4096];
                loop {
                    let got = f.read(&mut buf).context("reading old log file")?;
                    if got == 0 {
                        break;
                    }
                    new.write_all(&buf[..got]).context("writing truncated log file")?;
                }
                match remove_file(&log_path) {
                    Err(e) if e.kind() == ErrorKind::NotFound => (),
                    other => other.context("deleting stale untruncated log")?,
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            other => {
                other.context("opening old log file")?;
                unreachable!();
            }
        }
    } else {
        match rename(&log_path, rot_path) {
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            other => other.context("rotating log files")?,
        }
    }
    Ok(None)
}

async fn init_global_log_file(ctx: &EnvironmentContext, name: &str, rotate: bool) -> Result<()> {
    let (f, log_path) = log_file_with_info(ctx, name, rotate).await?;
    init_log_file_holder(log_path, f);
    Ok(())
}

async fn log_file_with_info(
    ctx: &EnvironmentContext,
    name: &str,
    rotate: bool,
) -> Result<(File, PathBuf)> {
    let mut log_path: PathBuf = ctx.query(LOG_DIR).get().await?;
    create_dir_all(&log_path)?;
    log_path.push(format!("{}.log", name));

    let mut f: Option<File> = None;
    if rotate {
        let log_rotations: Option<u64> = ctx.query(LOG_ROTATIONS).get().await?;
        let log_rotations = log_rotations.unwrap_or(0);
        if log_rotations > 0 {
            let log_rotate_size: Option<u64> = ctx.query(LOG_ROTATE_SIZE).get().await?;
            // rotate_file() returns Some(f) if it uses an existing file
            f = rotate_file(log_rotate_size, log_rotations, &log_path)?;
        }
    }

    if f.is_none() {
        f = Some(open_log_file(&log_path)?);
    }

    let f = f.unwrap();
    Ok((f, log_path))
}

pub async fn log_file(ctx: &EnvironmentContext, name: &str, rotate: bool) -> Result<File> {
    let (f, _) = log_file_with_info(ctx, name, rotate).await?;
    Ok(f)
}

fn open_log_file(path: &Path) -> Result<std::fs::File> {
    OpenOptions::new().write(true).append(true).create(true).open(path).context("opening log file")
}

pub async fn is_enabled(ctx: &EnvironmentContext) -> bool {
    ctx.query(LOG_ENABLED).get().await.unwrap_or(false)
}

async fn filter_level(ctx: &EnvironmentContext) -> LevelFilter {
    ctx.query(LOG_LEVEL)
        .get::<String>()
        .await
        .ok()
        .map(|str| {
            // Ideally we could log here, but there may be no log sink, so fall back to a default
            LevelFilter::from_str(&str).unwrap_or(LevelFilter::INFO)
        })
        .unwrap_or(LevelFilter::INFO)
}

pub async fn init(ctx: &EnvironmentContext, log_to_stdio: bool, log_to_file: bool) -> Result<()> {
    let init_file = log_to_file && is_enabled(ctx).await;
    if init_file {
        init_global_log_file(ctx, LOG_PREFIX, true).await?;
    };

    let level = filter_level(ctx).await;

    configure_subscribers(ctx, log_to_stdio, init_file, level).await;

    Ok(())
}

struct DisableableFilter;

impl<S> tracing_subscriber::layer::Filter<S> for DisableableFilter {
    fn enabled(
        &self,
        _meta: &Metadata<'_>,
        _cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        LOG_ENABLED_FLAG.load(Ordering::Relaxed)
    }
}

async fn target_levels(ctx: &EnvironmentContext) -> Vec<(String, LevelFilter)> {
    // Parse the targets from the config. Ideally we'd log errors, but since there might be no log
    // sink, filter out any unexpected values.

    if let Ok(targets) = ctx.query(LOG_TARGET_LEVELS).get::<serde_json::Value>().await {
        if let serde_json::Value::Object(o) = targets {
            return o
                .into_iter()
                .filter_map(|(target, level)| {
                    if let serde_json::Value::String(level) = level {
                        if let Ok(level) = LevelFilter::from_str(&level) {
                            return Some((target, level));
                        }
                    }
                    None
                })
                .collect();
        }
    }

    vec![]
}

async fn include_spans(ctx: &EnvironmentContext) -> bool {
    ctx.query(LOG_INCLUDE_SPANS).get().await.unwrap_or(false)
}

async fn configure_subscribers(
    ctx: &EnvironmentContext,
    stdio: bool,
    use_file: bool,
    level: LevelFilter,
) {
    let filter_targets =
        filter::Targets::new().with_targets(target_levels(ctx).await).with_default(level);

    let include_spans = include_spans(ctx).await;
    let stdio_layer = if stdio {
        let event_format = LogFormat::new(*LOGGING_ID, include_spans);
        let format = tracing_subscriber::fmt::layer()
            .event_format(event_format)
            .with_filter(DisableableFilter)
            .with_filter(filter_targets.clone());
        Some(format)
    } else {
        None
    };

    let file_layer = if use_file {
        let event_format = LogFormat::new(*LOGGING_ID, include_spans);
        let lfh = log_file_holder().expect("uninitialized LFH when use_file is set??");
        let writer = Mutex::new(std::io::LineWriter::new(lfh.get_resettable_writer()));
        let format = tracing_subscriber::fmt::layer()
            .event_format(event_format)
            .with_writer(writer)
            .with_filter(filter_targets);
        Some(format)
    } else {
        None
    };

    tracing_subscriber::registry().with(stdio_layer).with(file_layer).init();
}
