// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package artifactory

import (
	"encoding/json"
	"fmt"
	"os"
	"path"
)

type blob struct {
	Merkle string `json:"merkle"`
}

// BlobsUploads parses the blob manifest in the build and returns a list of
// Uploads for all blobs.
func BlobsUploads(blobManifestPath, srcDir, dstDir string) ([]Upload, error) {
	uploads := []Upload{}

	data, err := os.ReadFile(blobManifestPath)
	if err != nil {
		if os.IsNotExist(err) {
			return uploads, nil
		}
		return nil, fmt.Errorf("failed to read blob manifest: %w", err)
	}

	var blobs []blob
	err = json.Unmarshal(data, &blobs)
	if err != nil {
		return nil, fmt.Errorf("unable to unmarshal blob manifest: %w", err)
	}

	for _, blob := range blobs {
		uploads = append(uploads,
			Upload{
				Source:      path.Join(srcDir, blob.Merkle),
				Destination: path.Join(dstDir, blob.Merkle),
				Deduplicate: true,
			})
	}
	return uploads, nil
}
