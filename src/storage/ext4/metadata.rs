// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
    thiserror::Error,
};

// This is deliberately the same as ext4.
pub const ROOT_INODE_NUM: u64 = 2;

#[derive(Error, Debug)]
pub enum MetadataError {
    #[error("Node not found")]
    NotFound,
    #[error("Failed to deserialize metadata (corrupt?)")]
    FailedToDeserialize,
}

#[derive(Serialize, Deserialize)]
pub struct Metadata {
    nodes: HashMap<u64, Node>,
}

impl Metadata {
    pub fn new() -> Self {
        Self { nodes: HashMap::new() }
    }

    /// Finds the node with name `name` in directory with inode number `parent`.
    pub fn lookup(&self, parent: u64, name: &str) -> Result<u64, MetadataError> {
        self.nodes
            .get(&parent)
            .unwrap()
            .directory()
            .unwrap()
            .children
            .get(name)
            .ok_or(MetadataError::NotFound)
            .cloned()
    }

    /// Returns the node with inode number `inode_num`.
    pub fn get(&self, inode_num: u64) -> Option<&Node> {
        self.nodes.get(&inode_num)
    }

    /// Deserializes the metadata.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, MetadataError> {
        bincode::deserialize(bytes).map_err(|_| MetadataError::FailedToDeserialize)
    }

    /// Serializes the metadata.
    pub fn serialize(&self) -> Vec<u8> {
        bincode::serialize(&self).unwrap()
    }

    /// Add a child at `path` with inode number `inode_num`.
    pub fn add_child(&mut self, path: &[&str], inode_num: u64) {
        let mut node = self.nodes.get_mut(&ROOT_INODE_NUM).unwrap().directory_mut().unwrap();
        let mut iter = path.iter();
        let name = iter.next_back().unwrap();
        for component in iter {
            let inode_num = *node.children.get_mut(*component).unwrap();
            node = self.nodes.get_mut(&inode_num).unwrap().directory_mut().unwrap();
        }
        node.children.insert(name.to_string(), inode_num);
    }

    /// Inserts a directory node.  This will not add a child to a directory; see `add_child`.
    pub fn insert_directory(
        &mut self,
        inode_num: u64,
        mode: u16,
        uid: u16,
        gid: u16,
        extended_attributes: ExtendedAttributes,
    ) {
        self.nodes.insert(
            inode_num,
            Node {
                info: NodeInfo::Directory(Directory { children: HashMap::new() }),
                mode,
                uid,
                gid,
                extended_attributes,
            },
        );
    }

    /// Inserts a file node.  This will not add a child to a directory; see `add_child`.
    pub fn insert_file(
        &mut self,
        inode_num: u64,
        mode: u16,
        uid: u16,
        gid: u16,
        extended_attributes: ExtendedAttributes,
    ) {
        self.nodes.insert(
            inode_num,
            Node { info: NodeInfo::File(File), mode, uid, gid, extended_attributes },
        );
    }

    /// Inserts a symlink node.  This will not add a child to a directory; see `add_child`.
    pub fn insert_symlink(
        &mut self,
        inode_num: u64,
        target: String,
        mode: u16,
        uid: u16,
        gid: u16,
        extended_attributes: ExtendedAttributes,
    ) {
        self.nodes.insert(
            inode_num,
            Node {
                info: NodeInfo::Symlink(Symlink { target }),
                mode,
                uid,
                gid,
                extended_attributes,
            },
        );
    }
}

pub type ExtendedAttributes = HashMap<Box<[u8]>, Box<[u8]>>;

#[derive(Serialize, Deserialize)]
pub struct Node {
    info: NodeInfo,
    pub mode: u16,
    pub uid: u16,
    pub gid: u16,
    pub extended_attributes: ExtendedAttributes,
}

impl Node {
    pub fn info(&self) -> &NodeInfo {
        &self.info
    }

    pub fn directory(&self) -> Option<&Directory> {
        match &self.info {
            NodeInfo::Directory(d) => Some(d),
            _ => None,
        }
    }

    pub fn directory_mut(&mut self) -> Option<&mut Directory> {
        match &mut self.info {
            NodeInfo::Directory(d) => Some(d),
            _ => None,
        }
    }

    pub fn file(&self) -> Option<&File> {
        match &self.info {
            NodeInfo::File(f) => Some(f),
            _ => None,
        }
    }

    pub fn symlink(&self) -> Option<&Symlink> {
        match &self.info {
            NodeInfo::Symlink(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum NodeInfo {
    Directory(Directory),
    File(File),
    Symlink(Symlink),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Directory {
    pub children: HashMap<String, u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct File;

#[derive(Debug, Serialize, Deserialize)]
pub struct Symlink {
    pub target: String,
}

#[cfg(test)]
mod tests {
    use {
        super::{Metadata, NodeInfo, ROOT_INODE_NUM},
        assert_matches::assert_matches,
        std::collections::HashMap,
    };

    #[test]
    fn test_serialize_and_deserialize() {
        let mut m = Metadata::new();
        let xattr: HashMap<_, _> =
            [((*b"a").into(), (*b"apple").into()), ((*b"b").into(), (*b"ball").into())].into();
        m.insert_directory(ROOT_INODE_NUM, 1, 2, 3, Default::default());
        m.insert_directory(3, 1, 2, 3, xattr.clone());
        m.add_child(&["foo"], 3);
        m.insert_file(4, 1, 2, 3, xattr.clone());
        m.add_child(&["foo/bar"], 4);
        m.insert_symlink(5, "symlink-target".to_string(), 1, 2, 3, xattr.clone());
        m.add_child(&["foo/baz"], 5);

        let m = Metadata::deserialize(&m.serialize()).expect("deserialize failed");
        let node = m.get(ROOT_INODE_NUM).expect("root not found");
        assert_matches!(node.info(), NodeInfo::Directory(_));
        assert_eq!(node.mode, 1);
        assert_eq!(node.uid, 2);
        assert_eq!(node.gid, 3);
        assert_eq!(node.extended_attributes, [].into());

        assert_eq!(m.lookup(ROOT_INODE_NUM, "foo").expect("foo not found"), 3);
        let node = m.get(3).expect("root not found");
        assert_matches!(node.info(), NodeInfo::Directory(_));
        assert_eq!(node.mode, 1);
        assert_eq!(node.uid, 2);
        assert_eq!(node.gid, 3);
        assert_eq!(&node.extended_attributes, &xattr);

        assert_eq!(m.lookup(ROOT_INODE_NUM, "foo/bar").expect("foo/bar not found"), 4);
        let node = m.get(4).expect("root not found");
        assert_matches!(node.info(), NodeInfo::File(_));
        assert_eq!(node.mode, 1);
        assert_eq!(node.uid, 2);
        assert_eq!(node.gid, 3);
        assert_eq!(&node.extended_attributes, &xattr);

        assert_eq!(m.lookup(ROOT_INODE_NUM, "foo/baz").expect("foo/baz not found"), 5);
        let node = m.get(5).expect("root not found");
        assert_matches!(node.info(), NodeInfo::Symlink(_));
        assert_eq!(node.mode, 1);
        assert_eq!(node.uid, 2);
        assert_eq!(node.gid, 3);
        assert_eq!(&node.extended_attributes, &xattr);
    }
}
