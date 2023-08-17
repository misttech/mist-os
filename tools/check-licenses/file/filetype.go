// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package file

// FileType is an "enum" describing the type of file this is.
//
// Different files in the Fuchsia tree should be processed differently.
// LICENSE files should be analyzed and processed as a single unit, whereas
// NOTICE files need to be broken up (so each license segment can be analyzed
// separately).
//
// This field is set on every "file" object and allows us to process them
// using the right method.
type FileType string

const (
	RegularFile          FileType = "Regular File"
	SingleLicense                 = "Single License File"
	MultiLicense                  = "Multi License"
	MultiLicenseChromium          = "Multi License Chromium"
	MultiLicenseFlutter           = "Multi License Flutter"
	MultiLicenseAndroid           = "Multi License Android"
	MultiLicenseGoogle            = "Multi License Google"
)

var FileTypes map[string]FileType

func init() {
	FileTypes = map[string]FileType{
		"Regular File":           RegularFile,
		"Single License":         SingleLicense,
		"Multi License":          MultiLicense,
		"Multi License Chromium": MultiLicenseChromium,
		"Multi License Flutter":  MultiLicenseFlutter,
		"Multi License Android":  MultiLicenseAndroid,
		"Multi License Google":   MultiLicenseGoogle,

		// DEPRECATED: soft transition
		"regular":                RegularFile,
		"copyright_header":       RegularFile,
		"single_license":         SingleLicense,
		"multi_license_chromium": MultiLicenseChromium,
		"multi_license_flutter":  MultiLicenseFlutter,
		"multi_license_android":  MultiLicenseAndroid,
		"multi_license_google":   MultiLicenseGoogle,
	}
}

func (ft FileType) String() string {
	return string(ft)
}
