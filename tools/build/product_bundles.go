// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package build

// ProductBundle represents an entry in the product bundles build api.
type ProductBundle struct {
	// Label is the GN label for the product bundle.
	Label string `json:"label,omitempty"`

	// <product.board> label for this product bundle.
	Name string `json:"name"`

	// Path is the path to the product bundle directory.
	Path string `json:"path,omitempty"`

	// A unique version for this <product.board>.
	ProductVersion string `json:"product_version"`

	// Local path to the transfer manifest for the product bundle.
	TransferManifestPath string `json:"transfer_manifest_path,omitempty"`

	// The URL to the transfer manifest for the product bundle. i.e. it will
	// have a "file://" prefix if the file is local.
	TransferManifestUrl string `json:"transfer_manifest_url"`
}

// ProductBundlesManifest is a JSON list of product bundles produced by the
// Fuchsia build.
type ProductBundlesManifest = []ProductBundle

// GetPbPathByName gets the path to a product bundle based on name
func GetPbPathByName(pbs []ProductBundle, name string) string {
	for _, pb := range pbs {
		if pb.Name == name {
			return pb.Path
		}
	}
	return ""
}
