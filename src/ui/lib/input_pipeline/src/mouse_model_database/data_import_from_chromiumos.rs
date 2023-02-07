// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(crate) const MODELS: [(&'static str, &'static str, u32, &'static str); 70] = [
    // Ingram
    ("0002", "000a", 400, "PS/2 IBM Trackpoint"),
    // TODO(chaopeng): this is wrong vendor id, will fix in upstream
    ("004c", "0269", 1780, "Apple magicmouse 2"),
    // HP, Inc
    ("03f0", "0490", 800, "HP HyperX Pulsefire Surge RGB"),
    ("03f0", "0b97", 800, "HyperX Pulsefire Haste 2"),
    ("03f0", "0d8f", 800, "HP HyperX Pulsefire Core RGB"),
    ("03f0", "0f8f", 800, "HP HyperX Pulsefire Haste"),
    // Microsoft Corp.
    ("045e", "0024", 800, "Microsoft Corp. Trackball Explorer*"),
    ("045e", "0040", 416, "Microsoft Corp. Wheel Mouse Optical"),
    // Primax Electronics, Ltd
    ("0461", "4d22", 474, "Standard Dell"),
    // Logitech, Inc.
    ("046d", "1028", 1200, "M570 trackball[u]"),
    ("046d", "400e", 800, "K400 touchpad[u]"),
    ("046d", "4024", 800, "K400r touchpad[u]"),
    ("046d", "b02a", 1000, "Logitech M650"),
    ("046d", "b02b", 1000, "Logitech M550"),
    ("046d", "b02c", 1000, "Logitech M750"),
    ("046d", "b031", 1000, "Logitech LIFT"),
    ("046d", "b032", 1000, "Logitech M650 For Business"),
    ("046d", "b033", 1000, "Logitech LIFT B2B"),
    ("046d", "b36f", 1000, "Logitech BOLT Receiver"),
    ("046d", "c00*", 385, "Old Logitech Mice (copying 0xc00f setting)"),
    ("046d", "c00f", 385, "Logitech MouseMan Traveler/Mobile"),
    ("046d", "c014", 425, "HP branded - Logitech Optical USB Mouse"),
    ("046d", "c016", 377, "HP branded - Logitech, Inc. Optical Wheel Mouse"),
    ("046d", "c018", 530, "Logitech, Inc. Optical Wheel Mouse - model M-UAE96"),
    ("046d", "c03d", 434, "Logitech M-BT96a Pilot Optical Mouse"),
    ("046d", "c03e", 464, "Logitech Premium Optical Wheel Mouse (M-BT58)"),
    ("046d", "c40*", 600, "Logitech Trackballs*"),
    ("046d", "c508", 600, "Cordless Trackball"),
    // Kensington
    ("047d", "1002", 600, "Kensington Turbo Mouse Pro (trackball)*"),
    ("047d", "1003", 600, "Kensington Orbit TrackBall (trackball)*"),
    ("047d", "1005", 600, "Kensington TurboBall (trackball)*"),
    ("047d", "1006", 600, "Kensington TurboRing (trackball)*"),
    ("047d", "1009", 600, "Kensington Orbit TrackBall for Mac (trackball)*"),
    ("047d", "1020", 600, "Kensington Expert Mouse (trackball)*"),
    ("047d", "2041", 600, "Kensington SlimBlade Trackball (trackball)*"),
    // Holtek Semiconductor, Inc.
    ("04d9", "2519", 800, "FAVI Wireless Keyboard (TouchPad)*"),
    // Elecom Co., Ltd
    ("056e", "00e4", 1575, "Elecom EX-G Blue LED Wireless Mouse"),
    ("056e", "011a", 2100, "Elecom EX-G Blue LED Wired Mouse (2021)"),
    ("056e", "0141", 1700, "Elecom EPRIM Blue LED 5 Button Mouse 228"),
    ("056e", "0145", 1400, "Elecom EPRIM Blue LED 3 Button Mouse 542 (2020)"),
    ("056e", "0158", 1700, "Elecom M Size Mouse Black 227"),
    ("056e", "0159", 1800, "Elecom M Size Blue LED Mouse 203"),
    ("056e", "0171", 1700, "Elecom EPRIM Blue LED 3 Button Mouse 542 (2021)"),
    // Apple, Inc.
    ("05ac", "*", 373, "Apple mice (other)"),
    ("05ac", "0304", 400, "Apple USB Optical Mouse (Mighty Mouse)"),
    ("05ac", "030c", 400, "Apple BT Optical Mouse (Mighty Mouse)"),
    ("05ac", "030d", 1780, "Apple magicmouse (BT)"),
    // Synaptics, Inc.
    ("06cb", "0009", 400, "USB IBM Trackpoint"),
    // Pixart Imaging, Inc.
    ("093a", "2510", 1600, "PixArt Optical Mouse"),
    // Kingston
    ("0951", "16d3", 800, "Kingston HyperX Pulsefire Surge RGB"),
    ("0951", "16de", 800, "Kingston HyperX Pulsefire Core RGB"),
    ("0951", "1727", 800, "Kingston HyperX Pulsefire Haste"),
    // Broadcom Corp.
    ("0a5c", "8502", 800, "FAVI Wireless Keyboard (TouchPad), Bluetooth*"),
    // Microdia
    ("0c45", "7000", 800, "FAVI Entertainment Wireless Keyboard (TouchPad)*"),
    // SteelSeries ApS
    ("1038", "1369", 1620, "SteelSeries Sensei Raw"),
    ("1038", "1830", 1250, "SteelSeries Rival 3 Wireless"),
    // Razer USA, Ltd
    ("1532", "0016", 1714, "Razer USA, Ltd DeathAdder RZ01-0015"),
    ("1532", "0045", 4280, "Razer USA, Ltd Mamba"),
    // Lenovo
    ("17ef", "6009", 400, "Lenovo ThinkPad Keyboard w/ TrackPoint"),
    ("17ef", "6014", 800, "Lenovo N5901 multimedia keyboard/trackball*"),
    ("17ef", "602b", 800, "Lenovo N5902 multimedia keyboard/OFN*"),
    ("17ef", "6047", 400, "Thinkpad Compact USB Keyboard"),
    ("17ef", "6048", 400, "Thinkpad Compact USB Keyboard - Bluetooth"),
    ("17ef", "608a", 1600, "Lenovo YOGA Mouse"),
    ("17ef", "608d", 1750, "Lenovo Essential USB Mouse 4Y50R20863"),
    // Shenzhen Riitek Technology Co., Ltd
    ("1997", "0409", 800, "Riitek Rii Mote i6 (TouchPad)*"),
    // Corsair
    ("1b1c", "1b79", 1200, "Corsair Sabre Pro RGB Champion Wired"),
    ("1b1c", "1b7a", 1200, "Corsair Sabre Pro Champion Wired"),
    ("1b1c", "1bac", 1500, "Corsair Katar Pro XT Wired"),
    // Dell Computer Corp.
    ("413c", "3012", 502, "Dell Computer Corp. Optical Wheel Mouse"),
];

#[cfg(test)]
mod test {
    use {super::super::xorg_conf_parser, super::*, std::collections::HashMap, std::fs};

    fn model_to_string(m: &xorg_conf_parser::MouseModel) -> String {
        format!(r#"("{}", "{}", {}, "{}"),"#, m.vendor_id, m.product_id, m.cpi, m.identifier,)
    }

    #[fuchsia::test]
    fn sync_xorg_file() {
        let file = fs::read_to_string("/pkg/data/xorg_conf/20-mouse.conf")
            .expect("should be able to read 20-mouse.conf");
        let (_, mut models_from_file) =
            xorg_conf_parser::parse_xorg_file(xorg_conf_parser::NomSpan::new(file.as_str()))
                .expect("parse 20-mouse.conf failed");

        models_from_file.sort_by(|m1, m2| {
            if m1.vendor_id != m2.vendor_id {
                return m1.vendor_id.cmp(&m2.vendor_id);
            }
            m1.product_id.cmp(&m2.product_id)
        });

        // remove duplicated models in xorg.
        models_from_file
            .dedup_by(|m1, m2| m1.vendor_id == m2.vendor_id && m1.product_id == m2.product_id);

        let mut models_exist: HashMap<String, (String, String, u32, String)> = MODELS
            .into_iter()
            .map(|(vid, pid, cpi, identifier)| {
                (
                    [vid, pid].join(":").to_owned(),
                    (vid.to_owned(), pid.to_owned(), cpi, identifier.to_owned()),
                )
            })
            .collect();

        let mut to_add: Vec<String> = vec![];
        let mut to_change: Vec<String> = vec![];
        let mut to_delete: Vec<String> = vec![];

        for m in models_from_file {
            let k = [m.vendor_id.clone(), m.product_id.clone()].join(":").to_owned();
            match models_exist.get(&k) {
                Some((_vid, _pid, cpi, identifier)) => {
                    if *cpi != m.cpi || *identifier != m.identifier {
                        to_change.push(model_to_string(&m));
                    }
                    models_exist.remove(&k);
                }
                None => {
                    to_add.push(model_to_string(&m));
                }
            }
        }

        for (_, m) in models_exist {
            to_delete.push(format!(r#"("{}", "{}", {}, "{}"),"#, m.0, m.1, m.2, m.3));
        }

        if to_add.len() != 0 || to_change.len() != 0 || to_delete.len() != 0 {
            let to_add = to_add.join("\n");
            let to_change = to_change.join("\n");
            let to_delete = to_delete.join("\n");
            let output = [
                "to_add:",
                to_add.as_str(),
                "to_change:",
                to_change.as_str(),
                "to_delete:",
                to_delete.as_str(),
            ]
            .join("\n\n");

            panic!("{}", output);
        }
    }
}
