// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, ensure, Context, Result};
use errors::ffx_bail;
use ffx_config::{
    global_env_context,
    logging::{change_log_file, reset_log_file},
};
use ffx_daemon::{DaemonConfig, SocketDetails};
use fuchsia_async::TimeoutExt;
use serde_json::Value;
use std::{future::Future, io::Write, path::PathBuf, pin::Pin, time::Duration};
use termion::is_tty;

pub mod asserts;

// Pass the value of the config_key into the isolate.
// (Note that due to interior mutability, the isolate will
// be changed even though it's not &mut)
async fn set_in_isolate(
    context: &ffx_config::EnvironmentContext,
    isolate: &ffx_isolate::Isolate,
    config_key: &str,
) -> Result<()> {
    let val: Option<String> = context.get(config_key).await?;
    if let Some(s) = val {
        set_value_in_isolate(&isolate, config_key, Value::String(s)).await?;
    }
    Ok(())
}

// Set a config value
// (Note that due to interior mutability, the isolate will
// be changed even though it's not &mut)
async fn set_value_in_isolate(
    isolate: &ffx_isolate::Isolate,
    config_key: &str,
    value: Value,
) -> Result<()> {
    isolate
        .env_context()
        .query(config_key)
        .level(Some(ffx_config::ConfigLevel::User))
        .set(value)
        .await
}

fn subtest_log_file(isolate: &ffx_isolate::Isolate) -> PathBuf {
    let mut dir = isolate.log_dir().to_path_buf();
    dir.push("self_test.log");
    dir
}

/// Create a new ffx isolate. This method relies on the environment provided by
/// the ffx binary and should only be called within ffx.
pub async fn new_isolate(name: &str) -> Result<ffx_isolate::Isolate> {
    let ssh_key = ffx_config::get::<String, _>("ssh.priv").await?.into();
    let context = global_env_context().context("No global context")?;
    let isolate = ffx_isolate::Isolate::new_with_sdk(name, ssh_key, &context).await?;
    set_in_isolate(&context, &isolate, "overnet.cso").await?;
    set_value_in_isolate(&isolate, "watchdogs.host_pipe.enabled", true.into()).await?;
    // Globally change the log file to one appropriate to the isolate.  We'll reset it after
    // the test completes
    change_log_file(&subtest_log_file(&isolate))?;
    Ok(isolate)
}

const EXIT_WAIT_TIME_MS: u32 = 300;

async fn cleanup_isolate(isolate: ffx_isolate::Isolate) -> Result<()> {
    let socket_path = isolate.env_context().get_ascendd_path().await?;
    let details = SocketDetails::new(socket_path.clone());
    let pid = details.get_running_pid();
    // Give isolate a chance to clean up
    drop(isolate);
    if let Some(pid) = pid {
        // If the pid _was_ running, wait for it to exit, and kill it if it doesn't quit
        // within EXIT_WAIT_TIME_MS
        ffx_daemon::wait_for_daemon_to_exit(pid, EXIT_WAIT_TIME_MS).await?;
    }
    Ok(())
}

/// Get the target nodename we're expected to interact with in this test, or
/// pick the first discovered target. If nodename is set via $FUCHSIA_NODENAME
/// that is returned, if the nodename is not given, and zero targets are found,
/// this is also an error.
pub async fn get_target_nodename() -> Result<String> {
    if let Ok(nodename) = std::env::var("FUCHSIA_NODENAME") {
        return Ok(nodename);
    }

    let isolate = new_isolate("initial-target-discovery").await?;

    // ensure a daemon is spun up first, so we have a moment to discover targets.
    let out = isolate.ffx(&["target", "wait", "-t", "5"]).await?;
    if !out.status.success() {
        bail!("No targets found after 5s")
    }

    let out = isolate.ffx(&["target", "list", "-f", "j"]).await.context("getting target list")?;
    cleanup_isolate(isolate).await?;

    ensure!(out.status.success(), "Looking up a target name failed: {:?}", out);

    let targets: Value =
        serde_json::from_str(&out.stdout).context("parsing output from target list")?;

    let targets = targets.as_array().ok_or(anyhow!("expected target list ot return an array"))?;

    let target = targets
        .iter()
        .find(|target| {
            target["nodename"] != ""
                && target["target_state"]
                    .as_str()
                    .map(|s| s.to_lowercase().contains("product"))
                    .unwrap_or(false)
        })
        .ok_or(anyhow!("did not find any named targets in a product state"))?;
    target["nodename"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or(anyhow!("expected product state target to have a nodename"))
}

/// run runs the given set of tests printing results to stdout and exiting
/// with 0 or 1 if the tests passed or failed, respectively.
pub async fn run(tests: Vec<TestCase>, timeout: Duration, case_timeout: Duration) -> Result<()> {
    let mut writer = std::io::stdout();
    let color = is_tty(&writer);
    let green = green(color);
    let red = red(color);
    let nocol = nocol(color);

    let test_result = async {
        let num_tests = tests.len();

        writeln!(&mut writer, "1..{}", num_tests)?;

        let mut num_errs: usize = 0;
        for (i, tc) in tests.iter().enumerate().map(|(i, tc)| (i + 1, tc)) {
            write!(&mut writer, "{nocol}{i}. {name} - ", name = tc.name)?;
            writer.flush()?;
            match (tc.f)()
                .on_timeout(case_timeout, || ffx_bail!("timed out after {:?}", case_timeout))
                .await
            {
                Ok(oi) => {
                    writeln!(&mut writer, "{green}ok{nocol}",)?;
                    if let Some(isolate) = oi {
                        cleanup_isolate(isolate).await?;
                    }
                }
                Err(err) => {
                    // Unfortunately we didn't get a chance to get back any
                    // Isolate the test may have used, which means we can't
                    // clean up the daemon. The hope is that this is not a big
                    // deal -- when the Isolate drops(), it will remove the
                    // daemon.socket file which should cause the daemon to exit
                    // anyway -- this code was just a backstop to improve our
                    // chances of having it exit. Plus, the failure path is when
                    // we error out, which is the unusual case (and honestly,
                    // if we can't pass our self-test, there are no guarantees
                    // about the behavior anyway.)
                    writeln!(&mut writer, "{red}not ok{nocol}:\n{err:?}\n",)?;
                    num_errs = num_errs + 1;
                }
            }
            reset_log_file()?;
        }

        if num_errs != 0 {
            ffx_bail!("{red}{num_errs}/{num_tests} failed{nocol}");
        } else {
            writeln!(&mut writer, "{green}{num_tests}/{num_tests} passed{nocol}",)?;
        }

        Ok(())
    }
    .on_timeout(timeout, || ffx_bail!("timed out after {:?}", timeout))
    .await;

    test_result
}

fn green(color: bool) -> &'static str {
    if color {
        termion::color::Green.fg_str()
    } else {
        ""
    }
}
fn red(color: bool) -> &'static str {
    if color {
        termion::color::Red.fg_str()
    } else {
        ""
    }
}
fn nocol(color: bool) -> &'static str {
    if color {
        termion::color::Reset.fg_str()
    } else {
        ""
    }
}

#[macro_export]
macro_rules! tests {
    ( $( $x:expr ),* $(,)* ) => {
        {
            let mut temp_vec = Vec::new();
            $(
                // We need to store a boxed, pinned future, so let's provide the closure that
                // does that.
                temp_vec.push($crate::test::TestCase::new(stringify!($x), move || Box::pin($x())));
            )*
            temp_vec
        }
    };
}

// We need to store a boxed pinned future: boxed because we need it to be Sized since it's going
// in a Vec; pinned because it's asynchronous.
// Return an optional isolate, so that the runner can clean up our daemon if we created one.
pub type TestFn = fn() -> Pin<Box<dyn Future<Output = Result<Option<ffx_isolate::Isolate>>>>>;

pub struct TestCase {
    pub name: &'static str,
    f: TestFn,
}

impl TestCase {
    pub fn new(name: &'static str, f: TestFn) -> Self {
        Self { name, f }
    }
}
