// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        errors::FxfsError,
        lsm_tree::types::{ItemRef, LayerIterator},
        object_store::{
            transaction::{LockKey, Mutation, Options},
            ObjectKey, ObjectKeyData, ObjectKind, ObjectStore, ObjectValue, ProjectProperty,
        },
    },
    anyhow::{ensure, Error},
    std::{convert::TryInto, ops::Bound},
};

impl ObjectStore {
    /// Adds a mutation to set the project limit as an attribute with `bytes` and `nodes` to root
    /// node.
    pub async fn set_project_limit(
        &self,
        project_id: u64,
        bytes: u64,
        nodes: u64,
    ) -> Result<(), Error> {
        ensure!(project_id != 0, FxfsError::OutOfRange);
        let root_id = self.root_directory_object_id();
        let mut transaction = self
            .filesystem()
            .new_transaction(
                &vec![LockKey::ProjectId { store_object_id: self.store_object_id, project_id }],
                Options::default(),
            )
            .await?;
        transaction.add(
            self.store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::project_limit(root_id, project_id),
                ObjectValue::BytesAndNodes {
                    bytes: bytes.try_into().map_err(|_| FxfsError::TooBig)?,
                    nodes: nodes.try_into().map_err(|_| FxfsError::TooBig)?,
                },
            ),
        );
        transaction.commit().await?;
        Ok(())
    }

    /// Clear the limit for a project by tombstoning the limits and usage attributes for the
    /// given `project_id`. Fails if the project is still in use by one or more nodes.
    pub async fn clear_project_limit(&self, project_id: u64) -> Result<(), Error> {
        let root_id = self.root_directory_object_id();
        let mut transaction = self
            .filesystem()
            .new_transaction(
                &vec![LockKey::ProjectId { store_object_id: self.store_object_id, project_id }],
                Options::default(),
            )
            .await?;
        transaction.add(
            self.store_object_id,
            Mutation::replace_or_insert_object(
                ObjectKey::project_limit(root_id, project_id),
                ObjectValue::None,
            ),
        );
        transaction.commit().await?;
        Ok(())
    }

    /// Apply a `project_id` to a given node. Fails if node is not found or already has a project
    /// applied to it.
    pub async fn set_project_for_node(&self, node_id: u64, project_id: u64) -> Result<(), Error> {
        ensure!(project_id != 0, FxfsError::OutOfRange);
        let root_id = self.root_directory_object_id();
        let mut transaction = self
            .filesystem()
            .new_transaction(
                &vec![
                    LockKey::ProjectId { store_object_id: self.store_object_id, project_id },
                    LockKey::object(self.store_object_id, node_id),
                ],
                Options::default(),
            )
            .await?;

        let object_key = ObjectKey::object(node_id);
        let (kind, mut attributes) =
            match self.tree().find(&object_key).await?.ok_or(FxfsError::NotFound)?.value {
                ObjectValue::Object { kind, attributes } => (kind, attributes),
                ObjectValue::None => return Err(FxfsError::NotFound.into()),
                _ => return Err(FxfsError::Inconsistent.into()),
            };
        // Prevent updating this if it is already set.
        if attributes.project_id != 0 {
            return Err(FxfsError::AlreadyExists.into());
        }
        // Make sure the object kind makes sense.
        match kind {
            ObjectKind::File { .. } | ObjectKind::Directory { .. } => (),
            // For now, we don't support attributes on symlink objects, so setting a project id
            // doesn't make sense.
            ObjectKind::Symlink { .. } => return Err(FxfsError::NotSupported.into()),
            ObjectKind::Graveyard => return Err(FxfsError::Inconsistent.into()),
        }
        let storage_size = attributes.allocated_size;
        attributes.project_id = project_id;

        transaction.add(
            self.store_object_id,
            Mutation::replace_or_insert_object(
                object_key,
                ObjectValue::Object { kind, attributes },
            ),
        );
        transaction.add(
            self.store_object_id,
            Mutation::merge_object(
                ObjectKey::project_usage(root_id, project_id),
                ObjectValue::BytesAndNodes {
                    bytes: storage_size.try_into().map_err(|_| FxfsError::TooBig)?,
                    nodes: 1,
                },
            ),
        );
        transaction.commit().await?;
        Ok(())
    }

    /// Return the project_id associated with the given `node_id`.
    pub async fn get_project_for_node(&self, node_id: u64) -> Result<u64, Error> {
        match self.tree().find(&ObjectKey::object(node_id)).await?.ok_or(FxfsError::NotFound)?.value
        {
            ObjectValue::Object { attributes, .. } => match attributes.project_id {
                id => Ok(id),
            },
            ObjectValue::None => return Err(FxfsError::NotFound.into()),
            _ => return Err(FxfsError::Inconsistent.into()),
        }
    }

    /// Remove the project id for a given `node_id`. The call will do nothing and return success
    /// if the node is found to not be associated with any project.
    pub async fn clear_project_for_node(&self, node_id: u64) -> Result<(), Error> {
        let root_id = self.root_directory_object_id();
        let mut transaction = self
            .filesystem()
            .new_transaction(
                &vec![LockKey::object(self.store_object_id, node_id)],
                Options::default(),
            )
            .await?;

        let object_key = ObjectKey::object(node_id);
        let (kind, mut attributes) =
            match self.tree().find(&object_key).await?.ok_or(FxfsError::NotFound)?.value {
                ObjectValue::Object { kind, attributes } => (kind, attributes),
                ObjectValue::None => return Err(FxfsError::NotFound.into()),
                _ => return Err(FxfsError::Inconsistent.into()),
            };
        if attributes.project_id == 0 {
            return Ok(());
        }
        // Make sure the object kind makes sense.
        match kind {
            ObjectKind::File { .. } | ObjectKind::Directory { .. } => (),
            // For now, we don't support attributes on symlink objects, so setting a project id
            // doesn't make sense.
            ObjectKind::Symlink { .. } => return Err(FxfsError::NotSupported.into()),
            ObjectKind::Graveyard => return Err(FxfsError::Inconsistent.into()),
        }
        let old_project_id = attributes.project_id;
        attributes.project_id = 0;
        let storage_size = attributes.allocated_size;
        transaction.add(
            self.store_object_id,
            Mutation::replace_or_insert_object(
                object_key,
                ObjectValue::Object { kind, attributes },
            ),
        );
        // Not safe to convert storage_size to i64, as space usage can exceed i64 in size. Not
        // going to deal with handling such enormous files, fail the request.
        transaction.add(
            self.store_object_id,
            Mutation::merge_object(
                ObjectKey::project_usage(root_id, old_project_id),
                ObjectValue::BytesAndNodes {
                    bytes: -(storage_size.try_into().map_err(|_| FxfsError::TooBig)?),
                    nodes: -1,
                },
            ),
        );
        transaction.commit().await?;
        Ok(())
    }

    /// Returns a list of project ids currently tracked with project limits or usage in ascending
    /// order, beginning after `last_id` and providing up to `max_entries`. If `max_entries` would
    /// be exceeded then it also returns the final id in the list, for use in the following call to
    /// resume the listing.
    pub async fn list_projects(
        &self,
        start_id: u64,
        max_entries: usize,
    ) -> Result<(Vec<u64>, Option<u64>), Error> {
        let root_dir_id = self.root_directory_object_id();
        let layer_set = self.tree().layer_set();
        let mut merger = layer_set.merger();
        let mut iter =
            merger.seek(Bound::Included(&ObjectKey::project_limit(root_dir_id, start_id))).await?;
        let mut entries = Vec::new();
        let mut prev_entry = 0;
        let mut next_entry = None;
        while let Some(ItemRef { key: ObjectKey { object_id, data: key_data }, value, .. }) =
            iter.get()
        {
            // We've moved outside the target object id.
            if *object_id != root_dir_id {
                break;
            }
            match key_data {
                ObjectKeyData::Project { project_id, .. } => {
                    // Bypass deleted or repeated entries.
                    if *value != ObjectValue::None && prev_entry < *project_id {
                        if entries.len() == max_entries {
                            next_entry = Some(*project_id);
                            break;
                        }
                        prev_entry = *project_id;
                        entries.push(*project_id);
                    }
                }
                // We've moved outside the list of Project limits and usages.
                _ => {
                    break;
                }
            }
            iter.advance().await?;
        }
        // Skip deleted entries
        Ok((entries, next_entry))
    }

    /// Looks up the limit and usage of `project_id` as a pair of bytes and notes. Any of the two
    /// fields not found will return None for them.
    pub async fn project_info(
        &self,
        project_id: u64,
    ) -> Result<(Option<(u64, u64)>, Option<(u64, u64)>), Error> {
        let root_id = self.root_directory_object_id();
        let layer_set = self.tree().layer_set();
        let mut merger = layer_set.merger();
        let mut iter =
            merger.seek(Bound::Included(&ObjectKey::project_limit(root_id, project_id))).await?;
        let mut limit = None;
        let mut usage = None;
        // The limit should be immediately followed by the usage if both exist.
        while let Some(ItemRef { key: ObjectKey { object_id, data: key_data }, value, .. }) =
            iter.get()
        {
            // Should be within the bounds of the root dir id.
            if *object_id != root_id {
                break;
            }
            if let (
                ObjectKeyData::Project { project_id: found_project_id, property },
                ObjectValue::BytesAndNodes { bytes, nodes },
            ) = (key_data, value)
            {
                // Outside the range for target project information.
                if *found_project_id != project_id {
                    break;
                }
                let raw_value: (u64, u64) = (
                    // Should succeed in conversions since they shouldn't be negative.
                    (*bytes).try_into().map_err(|_| FxfsError::Inconsistent)?,
                    (*nodes).try_into().map_err(|_| FxfsError::Inconsistent)?,
                );
                match property {
                    ProjectProperty::Limit => limit = Some(raw_value),
                    ProjectProperty::Usage => usage = Some(raw_value),
                }
            } else {
                break;
            }
            iter.advance().await?;
        }
        Ok((limit, usage))
    }
}

// Tests are done end to end from the Fuchsia endpoint and so are in platform/fuchsia/volume.rs
