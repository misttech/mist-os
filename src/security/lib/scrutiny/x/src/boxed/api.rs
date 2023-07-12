// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::blob::BlobDirectoryError;
use dyn_clone::clone_trait_object;
use dyn_clone::DynClone;
use std::cmp;
use std::fmt;
use std::fmt::Debug;
use std::fmt::Display;
use std::hash;
use std::io::Read;
use std::io::Seek;
use std::path;
use thiserror::Error;

/// The type of readable, seekable objects returned by API interfaces.
pub trait ReaderSeeker: Read + Seek {}

impl<RS: Read + Seek> ReaderSeeker for RS {}

/// The type of paths on API interfaces.
pub trait Path: AsRef<path::Path> + DynClone {}

impl<P: AsRef<path::Path> + DynClone> Path for P {}

clone_trait_object!(Path);

impl Debug for dyn Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl Display for dyn Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl PartialEq for dyn Path {
    fn eq(&self, other: &dyn Path) -> bool {
        // Compare `dyn Path` instances as `std::path::Path` references.
        self.as_ref() == other.as_ref()
    }
}

impl Eq for dyn Path {}

impl hash::Hash for dyn Path {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state)
    }
}

/// Instance of the scrutiny framework backed by a particular set of artifacts.
pub trait Scrutiny {
    /// Accessor for high-level data about the Fuchsia system composition.
    fn system(&self) -> Box<dyn System>;

    /// Accessor for the system's component manager configuration.
    fn component_manager(&self) -> Box<dyn ComponentManager>;

    /// Accessor for high-level information about this [`Scrutiny`] instance's data sources.
    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn DataSource>>>;

    /// Iterate over all blobs from all system data sources.
    fn blobs(&self) -> Result<Box<dyn Iterator<Item = Box<dyn Blob>>>, ScrutinyBlobsError>;

    /// Iterate over all packages from all system data sources.
    fn packages(&self) -> Box<dyn Iterator<Item = Box<dyn Package>>>;

    /// Iterate over all package resolvers in the system.
    fn package_resolvers(&self) -> Box<dyn Iterator<Item = Box<dyn PackageResolver>>>;

    /// Iterate over all components in the system.
    fn components(&self) -> Box<dyn Iterator<Item = Box<dyn Component>>>;

    /// Iterate over all component resolvers in the system.
    fn component_resolvers(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentResolver>>>;

    /// Iterate over all component's capabilities in the system.
    fn component_capabilities(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentCapability>>>;

    /// Iterate over all component instances in the system. Note that a component instance is a
    /// component situated at a particular point in the system's component tree.
    fn component_instances(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstance>>>;

    /// Iterate over all capabilities of component instances in the system.
    fn component_instance_capabilities(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>;
}

#[derive(Debug, Error)]
pub enum ScrutinyBlobsError {
    #[error("error iterating over blobs in directory: {0}")]
    Directory(#[from] BlobDirectoryError),
}

/// High-level metadata about the system inspected by a [`Scrutiny`] instance.
pub trait System {
    /// The build directory associated with the system build.
    fn build_dir(&self) -> Box<dyn Path>;

    /// Accessor for the system's Zircon Boot Image (ZBI).
    fn zbi(&self) -> Box<dyn Zbi>;

    /// Accessor for the system's update package.
    fn update_package(&self) -> Box<dyn Package>;

    /// Accessor for the system's kernel command-line flags.
    fn kernel_flags(&self) -> Box<dyn KernelFlags>;

    /// Accessor for the system's Verified Boot Metadata (vbmeta).
    fn vb_meta(&self) -> Box<dyn VbMeta>;

    /// Accessor for the system's additional boot configuration file.
    fn additional_boot_configuration(&self) -> Box<dyn AdditionalBootConfiguration>;

    /// Accessor for the system's component manager configuration.
    fn component_manager_configuration(&self) -> Box<dyn ComponentManagerConfiguration>;
}

// TODO(fxbug.dev/112121): This is over-fitted to the "inspect bootfs" use case, and should probably be in terms of
// the various types of ZBI sections.

/// Model of the system's Zircon Boot Image (ZBI) used for Zircon kernel to userspace bootstrapping
/// (userboot). See https://fuchsia.dev/fuchsia-src/concepts/process/userboot for details.
pub trait Zbi {
    /// Iterate over (path, contents) pairs of files in this ZBI's bootfs. See
    /// https://fuchsia.dev/fuchsia-src/concepts/process/userboot#bootfs for details.
    fn bootfs(&self) -> Result<Box<dyn Iterator<Item = (Box<dyn Path>, Box<dyn Blob>)>>, ZbiError>;
}

#[derive(Debug, Error)]
pub enum ZbiError {
    #[error("failed to hash blobfs blob at bootfs path {bootfs_path:?}: {error}")]
    Hash { bootfs_path: Box<dyn Path>, error: std::io::Error },
    #[error("failed to parse bootfs image in zbi at path {path:?}: {error}")]
    ParseBootfs { path: Box<dyn Path>, error: anyhow::Error },
}

/// Kernel command-line flags. See https://fuchsia.dev/fuchsia-src/reference/kernel/kernel_cmdline
/// for details.
pub trait KernelFlags {
    /// Get the kernel command-line flag named `key`, or `None` if the flag does not exist in among
    /// this instance of kernel command-line flags.
    fn get(&self, key: &str) -> Option<&str>;

    /// Iterate over all kernel command-line flags specified on this instance.
    fn iter(&self) -> Box<dyn Iterator<Item = (String, String)>>;
}

// TODO(fxbug.dev/112121): What should this API look like?

/// Model of the Verified Boot Metadata (vbmeta).
pub trait VbMeta {}

/// Device manager configuration file key/value pairs. This configuration file is passed to the
/// device manager during early boot, and is combined with configuration set in [`KernelFlags`] and
/// [`VbMeta`] to determine various configuration parameters for booting the Fuchsia system on the
/// device.
pub trait AdditionalBootConfiguration {
    /// Get the value associated with `key`, or `None` if the key does not exist in in the
    /// underlying device configuration file.
    fn get(&self, key: &str) -> Option<&str>;

    /// Iterate over all key/value pairs specified on this instance.
    fn iter(&self) -> Box<dyn Iterator<Item = (String, String)>>;
}

/// Metadata about the component manager on a system.
pub trait ComponentManager {
    /// Accessor for this component manager's configuration.
    fn configuration(&self) -> Box<dyn ComponentManagerConfiguration>;

    /// Capabilities the system provides to the component manager.
    fn namespace_capabilities(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentCapability>>>;

    /// Capabilities the component manager provides to all components that it manages.
    fn builtin_capabilities(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentCapability>>>;
}

// TODO(fxbug.dev/112121): What should this API look like?

/// Model of the component manager configuration. For details about the role of component manager
/// in the system, see https://fuchsia.dev/fuchsia-src/concepts/components/v2/component_manager.
pub trait ComponentManagerConfiguration {}

/// Model of a data source that a [`Scrutiny`] instance is using as a source of truth about the
/// underlying system. This type is used for interrogating where a Fuchsia abstraction such as a
/// blob, package, or component came from (in terms of host filesystem artifacts). This is useful,
/// for example, in constructing error messages related to failed operations over a Fuchsia
/// abstraction.
pub trait DataSource: Debug + DynClone {
    /// The kind of artifact that this data source represents.
    fn kind(&self) -> DataSourceKind;

    /// The parent data source in the case of nested data sources. For example, this may refer to an
    /// FVM volume that contains a blobfs archive.
    fn parent(&self) -> Option<Box<dyn DataSource>>;

    /// Children data sources in the case of nested data sources.
    fn children(&self) -> Box<dyn Iterator<Item = Box<dyn DataSource>>>;

    /// The local path to this data source. Generally only applicable to data sources that have no
    /// parent.
    fn path(&self) -> Option<Box<dyn Path>>;

    /// The version of the underlying format of the data source.
    fn version(&self) -> DataSourceVersion;
}

clone_trait_object!(DataSource);

/// Computes equality of data exposed by two [`DataSource`] objects and their descendants.
fn descendants_eq(a: &dyn DataSource, b: &dyn DataSource) -> bool {
    if !data_source_shallow_eq(a, b) {
        return false;
    }

    let a_children: Vec<_> = a.children().collect();
    let b_children: Vec<_> = b.children().collect();
    if a_children.len() != b_children.len() {
        return false;
    }
    for (a_child, b_child) in a_children.into_iter().zip(b_children.into_iter()) {
        if !descendants_eq(a_child.as_ref(), b_child.as_ref()) {
            return false;
        }
    }

    true
}

/// Computes equality of [`DataSource`] ancestor objects of `a` and `b`, not including `a` and `b`
/// themselves.
fn ancestors_eq(a: &dyn DataSource, b: &dyn DataSource) -> bool {
    match (a.parent(), b.parent()) {
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
        (Some(a_parent), Some(b_parent)) => {
            if !data_source_shallow_eq(a_parent.as_ref(), b_parent.as_ref()) {
                false
            } else {
                ancestors_eq(a_parent.as_ref(), b_parent.as_ref())
            }
        }
    }
}

/// Computes whether `a` and `b` appear at the same position among sibling [`DataSource`] objects in
/// their associated trees of data sources.
fn data_source_child_placement_eq(a: &dyn DataSource, b: &dyn DataSource) -> bool {
    match (a.parent(), b.parent()) {
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
        (Some(a_parent), Some(b_parent)) => {
            let a_indices: Vec<_> = a_parent
                .children()
                .enumerate()
                .filter(|(_, child)| data_source_shallow_eq(a, child.as_ref()))
                .map(|(index, _)| index)
                .collect();
            if a_indices.len() != 1 {
                return false;
            }
            let b_indices: Vec<_> = b_parent
                .children()
                .enumerate()
                .filter(|(_, child)| data_source_shallow_eq(b, child.as_ref()))
                .map(|(index, _)| index)
                .collect();
            if b_indices.len() != 1 {
                return false;
            }

            a_indices[0] == b_indices[0]
        }
    }
}

/// Computes equality of all non-parent/child pointer data exposed by two [`DataSource`] objects.
fn data_source_shallow_eq(a: &dyn DataSource, b: &dyn DataSource) -> bool {
    if a.kind() != b.kind() || a.version() != b.version() {
        return false;
    }
    match (a.path(), b.path()) {
        (None, Some(_)) | (Some(_), None) => false,
        (None, None) => true,
        (Some(a_path), Some(b_path)) => {
            let a_ref: &std::path::Path = a_path.as_ref().as_ref();
            let b_ref: &std::path::Path = b_path.as_ref().as_ref();
            a_ref == b_ref
        }
    }
}

impl PartialEq for dyn DataSource {
    fn eq(&self, other: &dyn DataSource) -> bool {
        // To be deemed equal, `self` and `other` must define identical subtrees, have the same
        // ancestor data sources, and appear at the same position among their sibling data sources.
        descendants_eq(self, other)
            && ancestors_eq(self, other)
            && data_source_child_placement_eq(self, other)
    }
}

impl Eq for dyn DataSource {}

/// Kinds of artifacts that may constitute a source of truth for a [`Scrutiny`] instance reasoning
/// about a built Fuchsia system.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataSourceKind {
    /// A product bundle directory that contains various artifacts at known paths within the
    /// directory.
    ProductBundle,
    /// A TUF repository containing metadata and blobs.
    TufRepository,
    /// A blobfs archive (typically named "blob.blk" in Fuchsia builds). For details about blobfs
    /// itself, see https://fuchsia.dev/fuchsia-src/concepts/filesystems/blobfs.
    BlobfsArchive,
    /// A directory of blob files that are named after their Fuchsia merkle root hashes.
    BlobDirectory,
    /// An update package that designates a set of packages that constitute an over-the-air (OTA)
    /// system software update. See https://fuchsia.dev/fuchsia-src/concepts/packages/update_pkg
    /// for details.
    UpdatePackage,
    /// A Fuchsia Volume Manager (FVM) filesystem volume file. See
    /// https://fuchsia.dev/fuchsia-src/concepts/filesystems/filesystems#fvm for details.
    FvmVolume,
    /// A Zircon Boot Image (ZBI) file.
    Zbi,

    /// Multiple kinds are associated with underlying data source(s).
    Multiple(Vec<DataSourceKind>),

    // TODO(fxbug.dev/112121): Are there other data sources to consume?
    /// An artifact that was passed to a [`Scrutiny`] instance, but either its kind was not
    /// recognized, or the artifact was not a well-formed instance of the kind passed in.
    Unknown,
}

// TODO(fxbug.dev/112121): What varieties of versioning do formats need?

/// A version identifier associated with an artifact or unit of software used to interpret
/// artifacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataSourceVersion {
    /// Either no version information is available, or the information was malformed.
    Unknown,
}

/// A content-addressed file.
pub trait Blob {
    /// Accessor for the hash (i.e., content-addressed identity) of this file.
    fn hash(&self) -> Box<dyn Hash>;

    /// Gets a readable and seekable file content access API.
    ///
    /// # Panics
    ///
    /// Some blob sources may not support concurrent invocations of `Blob::reader_seeker`.
    fn reader_seeker(&self) -> Result<Box<dyn ReaderSeeker>, BlobError>;

    /// Iterate over the data sources that provide this blob.
    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn DataSource>>>;
}

/// An error encountered executing [`Blob::reader_seeker`].
#[derive(Debug, Error)]
pub enum BlobError {
    #[error("error reading from blob: {hash}, from directory: {directory}: {io_error_string}")]
    IoError { hash: Box<dyn Hash>, directory: Box<dyn Path>, io_error_string: String },
}

/// A type that can be reduced to a byte sequence, such as a hash digest.
pub trait AsBytes: Display + Debug {
    fn as_bytes(&self) -> &[u8];
}

impl PartialEq for dyn AsBytes {
    fn eq(&self, other: &dyn AsBytes) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for dyn AsBytes {}

impl dyn AsBytes {
    fn compare(&self, other: &dyn AsBytes) -> cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl PartialOrd for dyn AsBytes {
    fn partial_cmp(&self, other: &dyn AsBytes) -> Option<cmp::Ordering> {
        Some(self.compare(other))
    }
}

impl Ord for dyn AsBytes {
    fn cmp(&self, other: &dyn AsBytes) -> cmp::Ordering {
        self.compare(other)
    }
}

impl hash::Hash for dyn AsBytes {
    fn hash<H>(&self, state: &mut H)
    where
        H: hash::Hasher,
    {
        state.write(self.as_bytes())
    }
}

/// A content-address of a sequence of bytes. In most production cases, this is a Fuchsia merkle
/// root; see https://fuchsia.dev/fuchsia-src/concepts/packages/merkleroot for details.
pub trait Hash: AsBytes + DynClone + Send {}

impl<H: AsBytes + DynClone + Send> Hash for H {}

clone_trait_object!(Hash);

impl dyn Hash {
    fn compare(&self, other: &dyn Hash) -> cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl PartialOrd for dyn Hash {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.compare(other))
    }
}

impl Ord for dyn Hash {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.compare(other)
    }
}

impl PartialEq for dyn Hash {
    fn eq(&self, other: &dyn Hash) -> bool {
        self.as_bytes().eq(other.as_bytes())
    }
}

impl Eq for dyn Hash {}

impl hash::Hash for dyn Hash {
    fn hash<H>(&self, state: &mut H)
    where
        H: hash::Hasher,
    {
        state.write(self.as_bytes())
    }
}

/// Model of a Fuchsia package. See https://fuchsia.dev/fuchsia-src/concepts/packages/package for
/// details.
pub trait Package {
    /// Get the content-addressed hash of the package's "meta.far" file.
    fn hash(&self) -> Box<dyn Hash>;

    /// Gets the package's "meta/package" file.
    fn meta_package(&self) -> Box<dyn MetaPackage>;

    /// Gets the package's "meta/contents" file.
    fn meta_contents(&self) -> Box<dyn MetaContents>;

    /// Constructs iterator over blobs designated in the "meta/contents" of the package.
    fn content_blobs(&self) -> Box<dyn Iterator<Item = (Box<dyn Path>, Box<dyn Blob>)>>;

    /// Constructs iterator over files in the package's "meta.far" file. This includes, but is not
    /// limited to "meta/package" and "meta/contents" which have their own structured
    /// APIs.
    fn meta_blobs(&self) -> Box<dyn Iterator<Item = (Box<dyn Path>, Box<dyn Blob>)>>;

    /// Constructs iterator over blobs that appear to be component manifests.
    fn components(&self) -> Box<dyn Iterator<Item = (Box<dyn Path>, Box<dyn Component>)>>;
}

// TODO(fxbug.dev/112121): Define API consistent with fuchsia_pkg::MetaPackage.

/// Model of a Fuchsia package's "meta/package" file. See
/// https://fuchsia.dev/fuchsia-src/concepts/packages/package#structure-of-a-package for details.
pub trait MetaPackage {}

// TODO(fxbug.dev/112121): Define API consistent with fuchsia_pkg::MetaContents.

/// Model of a Fuchsia package's "meta/contents" file. See
/// https://fuchsia.dev/fuchsia-src/concepts/packages/package#structure-of-a-package for details.
pub trait MetaContents {
    /// Returns an iterator over all path -> content hash mappings stored in this "meta/contents"
    /// file.
    fn contents(&self) -> Box<dyn Iterator<Item = (Box<dyn Path>, Box<dyn Hash>)>>;
}

/// Model for a package resolution strategy. See
/// https://fuchsia.dev/fuchsia-src/get-started/learn/intro/packages#hosting_and_serving_packages
/// for details.
pub trait PackageResolver {
    /// Resolve a package URL to a content-addressed identity (hash).
    fn resolve(&self, url: PackageResolverUrl) -> Option<Box<dyn Hash>>;

    /// Iterate over the variety of package URLs that the resolver would resolve to the package
    /// identity given by `hash`.
    fn aliases(&self, hash: Box<dyn Hash>) -> Box<dyn Iterator<Item = PackageResolverUrl>>;
}

// TODO(fxbug.dev/112121): Define varieties of URL that PackageResolver supports.

/// The variety of URLs that [`PackageResolver`] can resolve to package hashes.
pub enum PackageResolverUrl {}

/// Model for a Fuchsia component. Note that this model is of a component as described by a
/// component manifest, not to be confused with a component _instance_, which is a component
/// situated at a particular point in a runtime component tree. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2 for details.
pub trait Component {
    /// Iterate over the known packages that contain the component.
    fn packages(&self) -> Box<dyn Iterator<Item = Box<dyn Package>>>;

    /// Iterate over known child component URLs.
    fn children(&self) -> Box<dyn Iterator<Item = PackageResolverUrl>>;

    /// Iterate over capability that the component uses.
    fn uses(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentCapability>>>;

    /// Iterate over capabilities that the component exposes to its parent.
    fn exposes(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentCapability>>>;

    /// Iterate over capabilities that the component offers to one or more of its children.
    fn offers(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentCapability>>>;

    /// Iterate over capabilities defined by (i.e., originating from) the component.
    fn capabilities(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentCapability>>>;

    /// Iterate over instances of the component that appear in the component tree known to the
    /// [`Scrutiny`] instance that underpins this component.
    fn instances(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstance>>>;
}

/// Model for a component resolution strategy. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities/resolvers for details.
pub trait ComponentResolver {
    /// Resolve a component URL to a content-addressed identity (hash).
    fn resolve(&self, url: ComponentResolverUrl) -> Option<Box<dyn Hash>>;

    /// Iterate over the variety of component URLs that the resolver would resolve to the package
    /// identity given by `hash`.
    fn aliases(&self, hash: Box<dyn Hash>) -> Box<dyn Iterator<Item = ComponentResolverUrl>>;
}

// TODO(fxbug.dev/112121): Define varieties of URL that ComponentResolver supports.

/// The variety of URLs that [`ComponentResolver`] can resolve to component hashes.
pub enum ComponentResolverUrl {}

/// A capability named in a particular component manifest. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/component_manifests and
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities for details.
pub trait ComponentCapability {
    /// Accessor for component that names the capability.
    fn component(&self) -> Box<dyn Component>;

    /// Accessor for the kind of capability.
    fn kind(&self) -> CapabilityKind;

    /// Accessor for the component-local notion of the capability's source, such as the parent
    /// component or a particular child component. Note that this is different from a
    /// [`ComponentInstance`] notion of source, which refers to the component instance where the
    /// capability originates before being routed around the component tree.
    fn source(&self) -> CapabilitySource;

    /// Accessor for the component-local notion of the capability's destination, such as the parent
    /// component or a particular child component. Note that this is different from a
    /// [`ComponentInstance`] notion of source, which refers to the component instances to which
    /// the capability is routed to, and routed no further.
    fn destination(&self) -> CapabilityDestination;

    /// Accessor for the component-local source name for this capability, if any. Capabilities can
    /// be renamed as they are routed around in the component tree by designating different source
    /// and destination names.
    fn source_name(&self) -> Option<Box<dyn ComponentCapabilityName>>;

    /// Accessor for the component-local source name for this capability, if any. Capabilities can
    /// be renamed as they are routed around in the component tree by designating different source
    /// and destination names.
    fn destination_name(&self) -> Option<Box<dyn ComponentCapabilityName>>;

    /// Accessor for the component-local source path for this capability, if any. Capabilities can
    /// be mapped to different path locations as they are routed around the component tree by
    /// designating different source and destination paths.
    fn source_path(&self) -> Option<Box<dyn ComponentCapabilityPath>>;

    /// Accessor for the component-local destination path for this capability, if any. Capabilities
    /// can be mapped to different path locations as they are routed around the component tree by
    /// designating different source and destination paths.
    fn destination_path(&self) -> Option<Box<dyn ComponentCapabilityPath>>;
}

/// Various kinds of capabilities that a [`Scrutiny`] instance can reason about. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities#capability-types for
/// details.
pub enum CapabilityKind {
    // TODO(fxbug.dev/112121): Add kinds of capabilities based on cm capability types.
    /// The kind of capability denoted in the component manfiest is not recognized by the
    /// underlying [`Scrutiny`] instance.
    Unknown,
}

/// The component-local source from which a capability is routed. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities#routing for details.
pub enum CapabilitySource {
    // TODO(fxbug.dev/112121): Add capability sources based on cm capability types.
    /// The capability source denoted in the component manfiest is not recognized by the
    /// underlying [`Scrutiny`] instance.
    Unknown,
}

/// The component-local destination to which a capability is routed. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities#routing for details.
pub enum CapabilityDestination {
    // TODO(fxbug.dev/112121): Add capability destinations based on cm capability types.
    /// The capability destination denoted in the component manfiest is not recognized by the
    /// underlying [`Scrutiny`] instance.
    Unknown,
}

/// The name of a capability in the context of its component manifest.
pub trait ComponentCapabilityName {
    /// Returns a borrowed string representation of the capability name.
    fn as_str(&self) -> &str;

    /// Accessor for the capability that uses this name.
    fn component(&self) -> Box<dyn ComponentCapability>;
}

/// The path associated with a capability in the context of its component manifest.
pub trait ComponentCapabilityPath {
    /// Returns a copy of the capability path.
    fn as_path(&self) -> Box<dyn Path>;

    /// Accessor for the capability that uses this path.
    fn component(&self) -> Box<dyn ComponentCapability>;
}

/// Model of a component instance that appears at a particular location in a component tree. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/topology#component-instances for details.
pub trait ComponentInstance {
    /// Accessor for the component instance identity, or "moniker".
    fn moniker(&self) -> Box<dyn Moniker>;

    /// Accessor for the environment passed to the component instance in the context of the
    /// component tree.
    fn environment(&self) -> Box<dyn Environment>;

    /// Accessor for the underlying component.
    fn component(&self) -> Box<dyn Component>;

    /// Accessor for the parent component instance.
    fn parent(&self) -> Box<dyn ComponentInstance>;

    /// Iterate over the children directly under this component in the component tree.
    fn children(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstance>>>;

    /// Iterate over the full set of descendants under this component in the component tree.
    fn descendants(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstance>>>;

    /// Iterate over the full set of ancestors above this component in the component tree.
    fn ancestors(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstance>>>;

    /// Iterate over capabilities that the component instance uses.
    fn uses(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>;

    /// Iterate over capabilities that the component instance exposes to its parent.
    fn exposes(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>;

    /// Iterate over capabilities that the component instance offers to one or more of its children.
    fn offers(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>;

    /// Iterate over capabilities defined by (i.e., originating from) the component instance.
    fn capabilities(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>;
}

// TODO(fxbug.dev/112121): Define API compatible with moniker::Moniker.

/// Model of a component instance moniker, the instance's identifier in the context of
/// the component tree constructed by the underlying [`Scrutiny`] instance. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/identifiers#monikers for details.
pub trait Moniker {}

// TODO(fxbug.dev/112121): Define API compatible with notion of bound environment in a component tree.

/// Model of a component instance environment that is realized in the context of the component
/// the component tree constructed by the underlying [`Scrutiny`] instance. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/environments for details.
pub trait Environment {}

/// A capability named by a component instance in the context of the component tree constructed
/// by the underlying [`Scrutiny`] instance. See
/// https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities#routing for details.
pub trait ComponentInstanceCapability {
    /// Accessor for the component capability for which this instance is a special case.
    fn component_capability(&self) -> Box<dyn ComponentCapability>;

    /// Accessor for the component instance that names this capability.
    fn component_instance(&self) -> Box<dyn ComponentInstance>;

    /// The source where this capability originates.
    fn source(&self) -> Box<dyn ComponentInstanceCapability>;

    /// Iterate over the component instance capabilities that constitute the capability route from
    /// its origin to this component instance capability (inclusive).
    fn source_path(&self) -> Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>;

    /// Iterate over the component instance capabilities that constitute all routes from this
    /// component instance capability to destinations that route no further (inclusive).
    fn destination_paths(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>>>;

    /// Iterate over the component instance capabilities that constitute all routes from this
    /// component instance capability's source to all destinations that route no further
    /// (inclusive).
    fn all_paths(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn Iterator<Item = Box<dyn ComponentInstanceCapability>>>>>;
}

#[cfg(test)]
mod tests {
    use super::super::data_source as ds;
    use super::super::data_source::test as dst;
    use super::DataSource;
    use super::DataSourceKind;
    use super::DataSourceVersion;
    use super::Path;

    #[fuchsia::test]
    fn test_data_source_eq() {
        fn expect(a: Box<dyn DataSource>, b: Box<dyn DataSource>, eq: bool) {
            if eq {
                assert_eq!(&a, &b);
            } else {
                assert_ne!(&a, &b);
            }
        }

        fn new_data_source(
            kind: DataSourceKind,
            path: Option<Box<dyn Path>>,
            version: DataSourceVersion,
        ) -> ds::DataSource {
            ds::DataSource::new(Box::new(dst::DataSourceInfo::new(kind, path, version)))
        }

        fn reference_tree() -> ds::DataSource {
            let mut root = new_data_source(
                DataSourceKind::ProductBundle,
                Some(Box::new("/pb")),
                DataSourceVersion::Unknown,
            );

            let mut child_1 =
                new_data_source(DataSourceKind::TufRepository, None, DataSourceVersion::Unknown);
            root.add_child(child_1.clone());
            root.add_child(new_data_source(
                DataSourceKind::FvmVolume,
                Some(Box::new("/pb/fvm.blk")),
                DataSourceVersion::Unknown,
            ));
            child_1.add_child(new_data_source(
                DataSourceKind::BlobDirectory,
                Some(Box::new("/pb/test.fuchsia.com/blobs")),
                DataSourceVersion::Unknown,
            ));
            root
        }

        fn diff_child2_kind_tree() -> ds::DataSource {
            let mut root = new_data_source(
                DataSourceKind::ProductBundle,
                Some(Box::new("/pb")),
                DataSourceVersion::Unknown,
            );

            let mut child_1 =
                new_data_source(DataSourceKind::TufRepository, None, DataSourceVersion::Unknown);
            root.add_child(child_1.clone());
            root.add_child(new_data_source(
                // Divergence: FVM misclassified as blobfs volume.
                DataSourceKind::BlobfsArchive,
                Some(Box::new("/pb/fvm.blk")),
                DataSourceVersion::Unknown,
            ));
            child_1.add_child(new_data_source(
                DataSourceKind::BlobDirectory,
                Some(Box::new("/pb/test.fuchsia.com/blobs")),
                DataSourceVersion::Unknown,
            ));
            root
        }

        fn diff_repo_blobs_path_tree() -> ds::DataSource {
            let mut root = new_data_source(
                DataSourceKind::ProductBundle,
                Some(Box::new("/pb")),
                DataSourceVersion::Unknown,
            );

            let mut child_1 =
                new_data_source(DataSourceKind::TufRepository, None, DataSourceVersion::Unknown);
            root.add_child(child_1.clone());
            root.add_child(new_data_source(
                DataSourceKind::FvmVolume,
                Some(Box::new("/pb/fvm.blk")),
                DataSourceVersion::Unknown,
            ));
            child_1.add_child(new_data_source(
                DataSourceKind::BlobDirectory,
                // Divergence: Different path to repository blobs.
                Some(Box::new("/pb/test.fuchsia.com/test_blobs")),
                DataSourceVersion::Unknown,
            ));
            root
        }

        fn diff_missing_grandchild_tree() -> ds::DataSource {
            let mut root = new_data_source(
                DataSourceKind::ProductBundle,
                Some(Box::new("/pb")),
                DataSourceVersion::Unknown,
            );

            let child_1 =
                new_data_source(DataSourceKind::TufRepository, None, DataSourceVersion::Unknown);
            root.add_child(child_1.clone());
            root.add_child(new_data_source(
                DataSourceKind::FvmVolume,
                Some(Box::new("/pb/fvm.blk")),
                DataSourceVersion::Unknown,
            ));
            // Divergence: No grandchild under repository.
            root
        }

        fn diff_extra_root_tree() -> ds::DataSource {
            // Divergence: `reference_tree()` is a subtree under an additional root node.
            let mut extra_root =
                new_data_source(DataSourceKind::Unknown, None, DataSourceVersion::Unknown);
            let mut root = new_data_source(
                DataSourceKind::ProductBundle,
                Some(Box::new("/pb")),
                DataSourceVersion::Unknown,
            );
            extra_root.add_child(root.clone());

            let mut child_1 =
                new_data_source(DataSourceKind::TufRepository, None, DataSourceVersion::Unknown);
            root.add_child(child_1.clone());
            root.add_child(new_data_source(
                DataSourceKind::FvmVolume,
                Some(Box::new("/pb/fvm.blk")),
                DataSourceVersion::Unknown,
            ));
            child_1.add_child(new_data_source(
                DataSourceKind::BlobDirectory,
                Some(Box::new("/pb/test.fuchsia.com/blobs")),
                DataSourceVersion::Unknown,
            ));
            extra_root
        }

        fn diff_extra_descendant_tree() -> ds::DataSource {
            let mut root = new_data_source(
                DataSourceKind::ProductBundle,
                Some(Box::new("/pb")),
                DataSourceVersion::Unknown,
            );

            let mut child_1 =
                new_data_source(DataSourceKind::TufRepository, None, DataSourceVersion::Unknown);
            root.add_child(child_1.clone());
            root.add_child(new_data_source(
                DataSourceKind::FvmVolume,
                Some(Box::new("/pb/fvm.blk")),
                DataSourceVersion::Unknown,
            ));
            let mut grandchild_1 = new_data_source(
                DataSourceKind::BlobDirectory,
                Some(Box::new("/pb/test.fuchsia.com/blobs")),
                DataSourceVersion::Unknown,
            );
            child_1.add_child(grandchild_1.clone());
            // Divergence: `grandchild_1` has a child.
            grandchild_1.add_child(new_data_source(
                DataSourceKind::Unknown,
                None,
                DataSourceVersion::Unknown,
            ));
            root
        }

        expect(Box::new(reference_tree()), Box::new(reference_tree()), true);
        expect(Box::new(reference_tree()), Box::new(diff_child2_kind_tree()), false);
        expect(Box::new(reference_tree()), Box::new(diff_repo_blobs_path_tree()), false);
        expect(Box::new(reference_tree()), Box::new(diff_missing_grandchild_tree()), false);
        expect(Box::new(reference_tree()), Box::new(diff_extra_descendant_tree()), false);

        // The subtree rooted in the first (and only) child of `diff_extra_root_tree` is identical
        // to `reference_tree`, but should not be treated as equal due to the additional parent
        // above.
        let reference: Box<dyn DataSource> = Box::new(reference_tree());
        let extra_root = diff_extra_root_tree();
        let almost_same_as_reference = extra_root.children().next().expect("extra root has child");

        // Check that all node attributes are equal, but that the extra ancestor causes a deep
        // equality check to fail.
        assert_eq!(reference.kind(), almost_same_as_reference.kind());
        assert_eq!(reference.version(), almost_same_as_reference.version());
        assert_eq!(
            &reference.path().expect("root path"),
            &almost_same_as_reference.path().expect("extra root child path")
        );
        expect(reference, almost_same_as_reference, false);
    }
}
