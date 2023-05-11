// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    argh::FromArgs,
    component_debug::dirs::*,
    fidl_ermine_tools as fermine, fidl_fuchsia_identity_account as faccount,
    fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
    fuchsia_component::client::connect_to_protocol_at_path,
    std::convert::TryInto,
};

#[derive(FromArgs, Debug, PartialEq)]
/// Various operations to control Ermine user experience.
pub struct Args {
    #[argh(subcommand)]
    pub command: Option<Command>,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum Command {
    Oobe(OobeCommand),
    Shell(ShellCommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "oobe")]
/// Control OOBE UI.
pub struct OobeCommand {
    #[argh(subcommand)]
    pub command: Option<OobeSubCommand>,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum OobeSubCommand {
    Password(OobeSetPasswordCommand),
    Login(OobeLoginCommand),
    Skip(OobeSkipCommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "set_password")]
/// Create password in OOBE
pub struct OobeSetPasswordCommand {
    #[argh(positional)]
    pub password: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "login")]
/// Login password in OOBE
pub struct OobeLoginCommand {
    #[argh(positional)]
    pub password: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "skip")]
/// Skip current screen.
pub struct OobeSkipCommand {}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "shell")]
/// Control shell UI.
pub struct ShellCommand {
    #[argh(subcommand)]
    pub command: ShellSubCommand,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum ShellSubCommand {
    Launch(ShellLaunchCommand),
    CloseAll(ShellCloseAllCommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "launch")]
/// Launch an application.
pub struct ShellLaunchCommand {
    #[argh(positional)]
    pub app_name: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "closeAll")]
/// Close all running applications.
pub struct ShellCloseAllCommand {}

/// Moniker of login_shell component
const LOGIN_SHELL_MONIKER: &str =
    "./core/session-manager/session:session/workstation_session/login_shell";

/// Moniker of the application shell component
const ERMINE_SHELL_MONIKER: &str =
    "./core/session-manager/session:session/workstation_session/login_shell/application_shell:";

/// Moniker of the account manager component.
const ACCOUNT_MANAGER_MONIKER: &str = "./core/account/account_manager";
const ACCOUNT_MANAGER_PROTOCOL: &str = "fuchsia.identity.account.AccountManager";
const PASSWORD_AUTHENTICATOR_MONIKER: &str = "./core/account/password_authenticator";
const PASSWORD_AUTHENTICATOR_PROTOCOL: &str = "fuchsia.identity.account.DeprecatedAccountManager";

async fn connect_to_exposed_protocol<P: fidl::endpoints::DiscoverableProtocolMarker>(
    realm_query: &fsys::RealmQueryProxy,
    moniker: &str,
) -> Result<P::Proxy, Error> {
    let moniker = moniker.try_into()?;
    let proxy =
        connect_to_instance_protocol_at_dir_root::<P>(&moniker, OpenDirType::Exposed, realm_query)
            .await?;
    Ok(proxy)
}

/// Connects to a discoverable protocol at the component instance supplied by `moniker`.
async fn connect_to_exposed_named_protocol<P: fidl::endpoints::DiscoverableProtocolMarker>(
    realm_query: &fsys::RealmQueryProxy,
    moniker: &str,
    protocol_name: &str,
) -> Result<P::Proxy, Error> {
    let moniker = moniker.try_into()?;
    let proxy = connect_to_instance_protocol_at_path::<P>(
        &moniker,
        OpenDirType::Exposed,
        protocol_name,
        realm_query,
    )
    .await?;
    Ok(proxy)
}

async fn get_account_id(realm_query: &fsys::RealmQueryProxy) -> Result<u64, Error> {
    // Connect to the new account system implementation if possible.
    let result = connect_to_exposed_named_protocol::<faccount::AccountManagerMarker>(
        realm_query,
        ACCOUNT_MANAGER_MONIKER,
        ACCOUNT_MANAGER_PROTOCOL,
    )
    .await;

    // If not then fallback to the deprecated password authenticator implementation.
    let account_manager = match result {
        Ok(account_manager) => account_manager,
        Err(_) => {
            connect_to_exposed_named_protocol::<faccount::AccountManagerMarker>(
                realm_query,
                PASSWORD_AUTHENTICATOR_MONIKER,
                PASSWORD_AUTHENTICATOR_PROTOCOL,
            )
            .await?
        }
    };

    let account_ids = account_manager.get_account_ids().await?;
    if account_ids.is_empty() {
        Err(format_err!("Failed to find any accounts in the system"))
    } else {
        Ok(*account_ids.first().unwrap())
    }
}

#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let Args { command } = argh::from_env();
    let realm_query =
        connect_to_protocol_at_path::<fsys::RealmQueryMarker>("/svc/fuchsia.sys2.RealmQuery.root")
            .unwrap();

    let oobe_automator = connect_to_exposed_protocol::<fermine::OobeAutomatorMarker>(
        &realm_query,
        LOGIN_SHELL_MONIKER,
    )
    .await?;

    match command {
        None => {
            let page = oobe_automator.get_oobe_page().await?;
            match page {
                fermine::OobePage::Shell => println!("Shell"),
                _ => println!("Oobe"),
            }
        }
        Some(Command::Oobe(OobeCommand { command })) => match command {
            None => {
                let page = oobe_automator.get_oobe_page().await?;
                println!("{:?}", page);
            }
            Some(OobeSubCommand::Login(OobeLoginCommand { password })) => {
                let result = oobe_automator.login(&password).await?;
                match result {
                    Ok(()) => println!("ok"),
                    _ => result
                        .map_err(|err: fermine::AutomatorErrorCode| format_err!("{:?}", err))?,
                }
            }
            Some(OobeSubCommand::Password(OobeSetPasswordCommand { password })) => {
                let result = oobe_automator.set_password(&password).await?;
                match result {
                    Ok(()) => println!("ok"),
                    _ => result
                        .map_err(|err: fermine::AutomatorErrorCode| format_err!("{:?}", err))?,
                }
            }
            Some(OobeSubCommand::Skip(OobeSkipCommand {})) => {
                let result = oobe_automator.skip_page().await?;
                match result {
                    Ok(()) => println!("ok"),
                    _ => result
                        .map_err(|err: fermine::AutomatorErrorCode| format_err!("{:?}", err))?,
                }
            }
        },
        Some(Command::Shell(ShellCommand { command })) => {
            let account_id = get_account_id(&realm_query).await?;
            // Append the account id to the shell's moniker.
            let moniker = format!("{}{}", ERMINE_SHELL_MONIKER, account_id);
            let shell_automator = connect_to_exposed_protocol::<fermine::ShellAutomatorMarker>(
                &realm_query,
                &moniker,
            )
            .await?;
            match command {
                ShellSubCommand::Launch(ShellLaunchCommand { app_name }) => {
                    let result = shell_automator
                        .launch(&fermine::ShellAutomatorLaunchRequest {
                            app_name: Some(app_name),
                            ..Default::default()
                        })
                        .await?;
                    match result {
                        Ok(()) => println!("ok"),
                        _ => result
                            .map_err(|err: fermine::AutomatorErrorCode| format_err!("{:?}", err))?,
                    }
                }
                ShellSubCommand::CloseAll(ShellCloseAllCommand {}) => {
                    let result = shell_automator.close_all().await?;
                    match result {
                        Ok(()) => println!("ok"),
                        _ => result
                            .map_err(|err: fermine::AutomatorErrorCode| format_err!("{:?}", err))?,
                    }
                }
            }
        }
    }
    Ok(())
}
