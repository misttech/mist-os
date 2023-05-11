// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use component_debug::cli::{GraphFilter, GraphOrientation, ListFilter};
use fuchsia_url::AbsoluteComponentUrl;
use moniker::AbsoluteMoniker;

#[derive(FromArgs, PartialEq, Debug)]
#[argh(
    name = "component",
    description = "Discover and manage components. Functionally equivalent to `ffx component`."
)]
pub struct ComponentArgs {
    #[argh(subcommand)]
    pub subcommand: ComponentSubcommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum ComponentSubcommand {
    Capability(CapabilityArgs),
    List(ListArgs),
    Graph(GraphArgs),
    Show(ShowArgs),
    Create(CreateArgs),
    Destroy(DestroyArgs),
    Resolve(ResolveArgs),
    Run(RunArgs),
    Start(StartArgs),
    Stop(StopArgs),
    Reload(ReloadArgs),
    Doctor(DoctorArgs),
    Copy(CopyArgs),
    Storage(StorageArgs),
    Collection(CollectionArgs),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "show", description = "Same as `ffx component show`")]
pub struct ShowArgs {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "create", description = "Same as `ffx component create`")]
pub struct CreateArgs {
    #[argh(positional)]
    pub moniker: AbsoluteMoniker,

    #[argh(positional)]
    pub url: AbsoluteComponentUrl,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "resolve", description = "Same as `ffx component resolve`")]
pub struct ResolveArgs {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "destroy", description = "Same as `ffx component destroy`")]
pub struct DestroyArgs {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "start", description = "Same as `ffx component start`")]
pub struct StartArgs {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "stop", description = "Same as `ffx component stop`")]
pub struct StopArgs {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "reload", description = "Same as `ffx component reload`")]
pub struct ReloadArgs {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "doctor", description = "Same as `ffx component doctor`")]
pub struct DoctorArgs {
    #[argh(positional)]
    /// component URL, moniker or instance ID. Partial matches allowed.
    pub query: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "capability", description = "Same as `ffx component capability`")]
pub struct CapabilityArgs {
    #[argh(positional)]
    pub capability_name: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "list", description = "Same as `ffx component list`")]
pub struct ListArgs {
    #[argh(option, long = "only", short = 'o')]
    /// filter the instance list by a criteria: cmx, cml, running, stopped, ancestors:<component_name>, descendants:<component_name>, or relatives:<component_name>
    pub filter: Option<ListFilter>,

    #[argh(switch, long = "verbose", short = 'v')]
    /// show detailed information about each instance
    pub verbose: bool,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "graph", description = "Same as `ffx component graph`")]
pub struct GraphArgs {
    #[argh(option, long = "only", short = 'o')]
    /// filter the instance list by a criteria: ancestor, descendant, relative
    pub filter: Option<GraphFilter>,

    #[argh(option, long = "orientation", short = 'r', default = "GraphOrientation::TopToBottom")]
    /// changes the visual orientation of the graph's nodes.
    /// Allowed values are "lefttoright"/"lr" and "toptobottom"/"tb".
    pub orientation: GraphOrientation,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "run", description = "Same as `ffx component run`")]
pub struct RunArgs {
    #[argh(positional)]
    pub moniker: AbsoluteMoniker,

    #[argh(positional)]
    pub url: AbsoluteComponentUrl,

    #[argh(switch, short = 'r')]
    /// destroy and recreate the component instance if it already exists
    pub recreate: bool,

    #[argh(switch)]
    /// connect stdin, stdout, and stderr to the component (requires component
    /// to be in a collection with single_run durability)
    pub connect_stdio: bool,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "copy", description = "Same as `ffx component copy`")]
pub struct CopyArgs {
    #[argh(positional)]
    pub paths: Vec<String>,
    /// show detailed information about copy action
    #[argh(switch, short = 'v')]
    pub verbose: bool,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "storage", description = "Same as `ffx component storage`")]
pub struct StorageArgs {
    #[argh(subcommand)]
    pub subcommand: StorageSubcommand,

    #[argh(option, default = "String::from(\"/core\")")]
    /// the moniker of the storage provider component.
    /// Defaults to "/core"
    pub provider: String,

    #[argh(option, default = "String::from(\"data\")")]
    /// the capability name of the storage to use.
    /// Examples: "data", "cache", "tmp"
    /// Defaults to "data"
    pub capability: String,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum StorageSubcommand {
    Copy(StorageCopyArgs),
    Delete(StorageDeleteArgs),
    List(StorageListArgs),
    MakeDirectory(StorageMakeDirectoryArgs),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "list", description = "Same as `ffx component storage list`")]
pub struct StorageListArgs {
    #[argh(positional)]
    pub path: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "make-directory",
    description = "Same as `ffx component storage make-directory`"
)]
pub struct StorageMakeDirectoryArgs {
    #[argh(positional)]
    pub path: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "copy", description = "Same as `ffx component storage copy`")]
pub struct StorageCopyArgs {
    #[argh(positional)]
    pub source_path: String,

    #[argh(positional)]
    pub destination_path: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "delete", description = "Same as `ffx component storage delete`")]
pub struct StorageDeleteArgs {
    #[argh(positional)]
    pub path: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "collection", description = "Same as `ffx component collection`")]
pub struct CollectionArgs {
    #[argh(subcommand)]
    pub subcommand: CollectionSubcommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum CollectionSubcommand {
    List(CollectionListArgs),
    Show(CollectionShowArgs),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "list", description = "Same as `ffx component collection list`")]
pub struct CollectionListArgs {
    #[argh(positional)]
    pub path: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "show", description = "Same as `ffx component collection show`")]
pub struct CollectionShowArgs {
    #[argh(positional)]
    pub query: String,
}
