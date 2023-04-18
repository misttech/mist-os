// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package file

import (
	"encoding/json"
)

// File objects may be accessed from multiple goroutines simultaneously.
// To support concurrent access, certain fields are not exported, and can only
// be accessed using threadsafe accessor methods (e.g. the data & text fields).
//
// Since those fields are unexported, they can't be directly serialized using
// the standard JSON practice. Custom Marshalling functions must be used
// to properly save and restore data from these hidden fields.

// ================
// File
type fileJSON struct {
	Name          string      `json:"name"`
	AbsPath       string      `json:"absPath"`
	RelPath       string      `json:"relPath"`
	URL           string      `json:"url"`
	ProjectName   string      `json:"projectName"`
	FileType      FileType    `json:"fileType"`
	ContentLoaded bool        `json:"contentLoaded"`
	Data          []*FileData `json:"data"`
	Text          string      `json:"text"`
	SPDXName      string      `json:"spdxName"`
	SPDXID        string      `json:"spdxID"`
}

func (f *File) MarshalJSON() ([]byte, error) {
	return json.Marshal(&fileJSON{
		Name:          f.name,
		AbsPath:       f.absPath,
		RelPath:       f.relPath,
		URL:           f.url,
		ProjectName:   f.projectName,
		FileType:      f.fileType,
		ContentLoaded: f.contentLoaded,
		Data:          f.data,
		Text:          string(f.text),
		SPDXName:      f.spdxName,
		SPDXID:        f.spdxID,
	})
}

func (f *File) UnmarshalJSON(data []byte) error {
	tmp := &fileJSON{}
	if err := json.Unmarshal(data, &tmp); err != nil {
		return err
	}

	f.name = tmp.Name
	f.absPath = tmp.AbsPath
	f.relPath = tmp.RelPath
	f.url = tmp.URL
	f.projectName = tmp.ProjectName
	f.fileType = tmp.FileType
	f.contentLoaded = tmp.ContentLoaded
	f.data = tmp.Data
	f.spdxName = tmp.SPDXName
	f.spdxID = tmp.SPDXID

	// f.text is a byte array.
	// For easier viewing in json format, convert to a string when marshalling,
	// and convert back to byte array when unmarshalling.
	f.text = []byte{}
	if len(tmp.Text) > 0 {
		f.text = []byte(tmp.Text)
	}

	return nil
}

// ================
// FileData
type fileDataJSON struct {
	File               *File  `json:"file"`
	LibraryName        string `json:"libraryName"`
	LineNumber         int    `json:"lineNumber"`
	Data               string `json:"data"`
	LicenseType        string `json:"licenseType"`
	PatternPath        string `json:"patternPath"`
	URL                string `json:"url"`
	BeingSurfaced      string `json:"beingSurfaced"`
	SourceCodeIncluded string `json:"sourceCodeIncluded"`
	SPDXName           string `json:"spdxName"`
	SPDXID             string `json:"spdxID"`
	Hash               string `json:"hash"`
}

func (fd *FileData) MarshalJSON() ([]byte, error) {
	return json.Marshal(&fileDataJSON{
		File:               fd.file,
		LibraryName:        fd.libraryName,
		LineNumber:         fd.lineNumber,
		Data:               string(fd.data),
		LicenseType:        fd.licenseType,
		PatternPath:        fd.patternPath,
		URL:                fd.url,
		BeingSurfaced:      fd.beingSurfaced,
		SourceCodeIncluded: fd.sourceCodeIncluded,
		SPDXName:           fd.spdxName,
		SPDXID:             fd.spdxID,
		Hash:               fd.hash,
	})
}

func (fd *FileData) UnmarshalJSON(data []byte) error {
	tmp := &fileDataJSON{}
	if err := json.Unmarshal(data, &tmp); err != nil {
		return err
	}

	fd.file = tmp.File
	fd.libraryName = tmp.LibraryName
	fd.lineNumber = tmp.LineNumber
	fd.licenseType = tmp.LicenseType
	fd.patternPath = tmp.PatternPath
	fd.url = tmp.URL
	fd.beingSurfaced = tmp.BeingSurfaced
	fd.sourceCodeIncluded = tmp.SourceCodeIncluded
	fd.spdxName = tmp.SPDXName
	fd.spdxID = tmp.SPDXID
	fd.hash = tmp.Hash

	// f.data is a byte array.
	// For easier viewing in json format, convert to a string when marshalling,
	// and convert back to byte array when unmarshalling.
	fd.data = []byte{}
	if len(tmp.Data) > 0 {
		fd.data = []byte(tmp.Data)
	}

	return nil
}
