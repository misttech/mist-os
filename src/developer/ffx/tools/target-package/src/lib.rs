// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::anyhow;
use fho::FfxContext as _;
use fidl_fuchsia_dash as fdash;
use futures::stream::StreamExt as _;

#[derive(fho::FfxTool)]
pub struct TargetPackageTool {
    #[command]
    cmd: TargetPackageCommand,
    #[with(fho::deferred(fho::moniker("/core/debug-dash-launcher")))]
    dash_launcher_proxy: fho::Deferred<fdash::LauncherProxy>,
}

#[derive(argh::FromArgs, Debug, PartialEq, Eq)]
#[argh(
    subcommand,
    name = "target-package",
    description = "Interact with the target's packaging system"
)]
pub struct TargetPackageCommand {
    #[argh(subcommand)]
    subcommand: TargetPackageSubCommand,
}

#[derive(argh::FromArgs, Debug, PartialEq, Eq)]
#[argh(subcommand)]
pub enum TargetPackageSubCommand {
    Explore(ExploreCommand),
}

// TODO(fxbug.dev/128699) Make the explore command a separate subtool.
#[derive(argh::FromArgs, Debug, PartialEq, Eq)]
#[argh(
    subcommand,
    name = "explore",
    description = "Resolves a package and then spawns a shell with said package loaded into the namespace at /pkg.",
    example = "To explore the update package interactively:

> ffx target-package explore 'fuchsia-pkg://fuchsia.com/update'
$ ls
svc
pkg
$ exit
Connection to terminal closed

To run a command directly from the command line:
> ffx target package explore 'fuchsia-pkg://fuchsia.com/update' -c 'printenv'
PATH=/.dash/tools/debug-dash-launcher
PWD=/
",
    note = "The environment contains the following directories:
* /.dash    User-added and built-in dash tools
* /pkg      The package directory of the resolved package
* /svc      Protocols required by the dash shell

If additional binaries are provided via --tools, they will be loaded into .dash/tools/<pkg>/<binary>
The path is set so that they can be run by name. The path preference is in the command line order
of the --tools arguments, ending with the built-in dash tools (/.dash/tools/debug-dash-launcher).

--tools URLs may be package or binary URLs. Note that collisions can occur if different URLs have
the same package and binary names. An error, `NonUniqueBinaryName`, is returned if a binary name
collision occurs.
"
)]
pub struct ExploreCommand {
    #[argh(positional)]
    /// the package URL to resolve. If `subpackages` is empty the resolved package directory will
    /// be loaded into the shell's namespace at `/pkg`.
    pub url: String,

    #[argh(option, long = "subpackage")]
    /// the chain of subpackages, if any, of `url` to resolve, in resolution order.
    /// If `subpackages` is not empty, the package directory of the final subpackage will be
    /// loaded into the shell's namespace at `/pkg`.
    pub subpackages: Vec<String>,

    #[argh(option)]
    /// list of URLs of tools packages to include in the shell environment.
    /// the PATH variable will be updated to include binaries from these tools packages.
    /// repeat `--tools url` for each package to be included.
    /// The path preference is given by command line order.
    pub tools: Vec<String>,

    #[argh(option, short = 'c', long = "command")]
    /// execute a command instead of reading from stdin.
    /// the exit code of the command will be forwarded to the host.
    pub command: Option<String>,
}

#[async_trait::async_trait(?Send)]
impl fho::FfxMain for TargetPackageTool {
    type Writer = fho::SimpleWriter;
    async fn main(self, _: Self::Writer) -> fho::Result<()> {
        match self.cmd.subcommand {
            TargetPackageSubCommand::Explore(command) => {
                explore(command, self.dash_launcher_proxy.await?).await?
            }
        }
        Ok(())
    }
}

async fn explore(command: ExploreCommand, dash_launcher: fdash::LauncherProxy) -> fho::Result<()> {
    let ExploreCommand { url, subpackages, tools, command } = command;

    let (client, server) = fidl::Socket::create_stream();
    let stdout = if command.is_some() {
        socket_to_stdio::Stdout::buffered()
    } else {
        socket_to_stdio::Stdout::raw()?
    };
    let () = dash_launcher
        .explore_package_over_socket(&url, &subpackages, server, &tools, command.as_deref())
        .await
        .bug_context("fidl error launching dash")?
        .map_err(|e| match e {
            fdash::LauncherError::ResolveTargetPackage => fho::Error::User(anyhow!(
                "No package found matching '{}' {}.",
                url,
                subpackages.join(" ")
            )),
            e => fho::Error::Unexpected(anyhow!("Unexpected error launching dash: {:?}", e)),
        })?;
    let () = socket_to_stdio::connect_socket_to_stdio(client, stdout).await?;
    let exit_code = wait_for_shell_exit(&dash_launcher).await?;
    std::process::exit(exit_code);
}

async fn wait_for_shell_exit(launcher_proxy: &fdash::LauncherProxy) -> fho::Result<i32> {
    // Report process errors and return the exit status.
    let mut event_stream = launcher_proxy.take_event_stream();
    match event_stream.next().await {
        Some(Ok(fdash::LauncherEvent::OnTerminated { return_code })) => Ok(return_code),
        Some(Err(e)) => Err(fho::Error::Unexpected(anyhow!("OnTerminated event error: {:?}", e))),
        None => {
            Err(fho::Error::Unexpected(anyhow!("didn't receive an expected OnTerminated event")))
        }
    }
}
