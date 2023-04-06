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

type deliveryBlobConfig struct {
	Type int `json:"type"`
}

// BlobsUploads parses the blob manifest in the build and returns a list of
// Uploads for all blobs.
func BlobsUploads(blobManifestPath, deliveryBlobConfigPath, srcDir, dstDir string) ([]Upload, error) {
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

	// Also upload delivery blobs if the config exists.
	data, err = os.ReadFile(deliveryBlobConfigPath)
	if err != nil {
		if os.IsNotExist(err) {
			return uploads, nil
		}
		return nil, fmt.Errorf("failed to read delivery blob config: %w", err)
	}

	var config deliveryBlobConfig
	err = json.Unmarshal(data, &config)
	if err != nil {
		return nil, fmt.Errorf("unable to unmarshal delivery blob config: %w", err)
	}
	blobType := fmt.Sprint(config.Type)
	for _, blob := range blobs {
		uploads = append(uploads,
			Upload{
				Source:      path.Join(srcDir, blobType, blob.Merkle),
				Destination: path.Join(dstDir, blobType, blob.Merkle),
				Deduplicate: true,
			})
	}
	return uploads, nil
}
