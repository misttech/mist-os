// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logging::log_warn;
use fidl_fuchsia_input::Key;
use starnix_uapi::uapi;
use std::collections::HashMap;

/// linux <-> fuchsia key map allow search from 2 way.
#[allow(dead_code)]
pub struct KeyMap {
    linux_to_fuchsia: HashMap<u32, Key>,
    fuchsia_to_linux: HashMap<Key, u32>,
}

impl KeyMap {
    fn insert(&mut self, linux_keycode: u32, fuchsia_key: Key) {
        // Should not have any conflict keys.
        assert!(
            !self.linux_to_fuchsia.contains_key(&linux_keycode),
            "conflicted linux keycode={} fuchsia keycode={:?}",
            linux_keycode,
            fuchsia_key
        );
        assert!(
            !self.fuchsia_to_linux.contains_key(&fuchsia_key),
            "conflicted fuchsia keycode={:?}, linux keycode={}",
            fuchsia_key,
            linux_keycode
        );

        self.linux_to_fuchsia.insert(linux_keycode, fuchsia_key);
        self.fuchsia_to_linux.insert(fuchsia_key, linux_keycode);
    }

    #[allow(dead_code)]
    pub fn linux_keycode_to_fuchsia_input_key(&self, key: u32) -> Key {
        match self.linux_to_fuchsia.get(&key) {
            Some(k) => *k,
            None => {
                log_warn!("unknown linux keycode {}", key);
                Key::Unknown
            }
        }
    }

    #[allow(dead_code)]
    pub fn fuchsia_input_key_to_linux_keycode(&self, key: Key) -> u32 {
        match self.fuchsia_to_linux.get(&key) {
            Some(k) => *k,
            None => {
                log_warn!("unknown fuchsia key {:?}", key);
                // this is the invalid key code 0
                uapi::KEY_RESERVED
            }
        }
    }
}

#[allow(dead_code)]
pub fn init_key_map() -> KeyMap {
    let mut m = KeyMap { linux_to_fuchsia: HashMap::new(), fuchsia_to_linux: HashMap::new() };

    m.insert(uapi::KEY_ESC, Key::Escape);
    m.insert(uapi::KEY_1, Key::Key1);
    m.insert(uapi::KEY_2, Key::Key2);
    m.insert(uapi::KEY_3, Key::Key3);
    m.insert(uapi::KEY_4, Key::Key4);
    m.insert(uapi::KEY_5, Key::Key5);
    m.insert(uapi::KEY_6, Key::Key6);
    m.insert(uapi::KEY_7, Key::Key7);
    m.insert(uapi::KEY_8, Key::Key8);
    m.insert(uapi::KEY_9, Key::Key9);
    m.insert(uapi::KEY_0, Key::Key0);
    m.insert(uapi::KEY_MINUS, Key::Minus);
    m.insert(uapi::KEY_EQUAL, Key::Equals);
    m.insert(uapi::KEY_BACKSPACE, Key::Backspace);
    m.insert(uapi::KEY_TAB, Key::Tab);
    m.insert(uapi::KEY_Q, Key::Q);
    m.insert(uapi::KEY_W, Key::W);
    m.insert(uapi::KEY_E, Key::E);
    m.insert(uapi::KEY_R, Key::R);
    m.insert(uapi::KEY_T, Key::T);
    m.insert(uapi::KEY_Y, Key::Y);
    m.insert(uapi::KEY_U, Key::U);
    m.insert(uapi::KEY_I, Key::I);
    m.insert(uapi::KEY_O, Key::O);
    m.insert(uapi::KEY_P, Key::P);
    m.insert(uapi::KEY_LEFTBRACE, Key::LeftBrace);
    m.insert(uapi::KEY_RIGHTBRACE, Key::RightBrace);
    m.insert(uapi::KEY_ENTER, Key::Enter);
    m.insert(uapi::KEY_LEFTCTRL, Key::LeftCtrl);
    m.insert(uapi::KEY_A, Key::A);
    m.insert(uapi::KEY_S, Key::S);
    m.insert(uapi::KEY_D, Key::D);
    m.insert(uapi::KEY_F, Key::F);
    m.insert(uapi::KEY_G, Key::G);
    m.insert(uapi::KEY_H, Key::H);
    m.insert(uapi::KEY_J, Key::J);
    m.insert(uapi::KEY_K, Key::K);
    m.insert(uapi::KEY_L, Key::L);
    m.insert(uapi::KEY_SEMICOLON, Key::Semicolon);
    m.insert(uapi::KEY_APOSTROPHE, Key::Apostrophe);
    m.insert(uapi::KEY_GRAVE, Key::GraveAccent);
    m.insert(uapi::KEY_LEFTSHIFT, Key::LeftShift);
    m.insert(uapi::KEY_BACKSLASH, Key::Backslash);
    m.insert(uapi::KEY_Z, Key::Z);
    m.insert(uapi::KEY_X, Key::X);
    m.insert(uapi::KEY_C, Key::C);
    m.insert(uapi::KEY_V, Key::V);
    m.insert(uapi::KEY_B, Key::B);
    m.insert(uapi::KEY_N, Key::N);
    m.insert(uapi::KEY_M, Key::M);
    m.insert(uapi::KEY_COMMA, Key::Comma);
    m.insert(uapi::KEY_DOT, Key::Dot);
    m.insert(uapi::KEY_SLASH, Key::Slash);
    m.insert(uapi::KEY_RIGHTSHIFT, Key::RightShift);
    m.insert(uapi::KEY_KPASTERISK, Key::KeypadAsterisk);
    m.insert(uapi::KEY_LEFTALT, Key::LeftAlt);
    m.insert(uapi::KEY_SPACE, Key::Space);
    m.insert(uapi::KEY_CAPSLOCK, Key::CapsLock);
    m.insert(uapi::KEY_F1, Key::F1);
    m.insert(uapi::KEY_F2, Key::F2);
    m.insert(uapi::KEY_F3, Key::F3);
    m.insert(uapi::KEY_F4, Key::F4);
    m.insert(uapi::KEY_F5, Key::F5);
    m.insert(uapi::KEY_F6, Key::F6);
    m.insert(uapi::KEY_F7, Key::F7);
    m.insert(uapi::KEY_F8, Key::F8);
    m.insert(uapi::KEY_F9, Key::F9);
    m.insert(uapi::KEY_F10, Key::F10);
    m.insert(uapi::KEY_NUMLOCK, Key::NumLock);
    m.insert(uapi::KEY_SCROLLLOCK, Key::ScrollLock);
    m.insert(uapi::KEY_KP7, Key::Keypad7);
    m.insert(uapi::KEY_KP8, Key::Keypad8);
    m.insert(uapi::KEY_KP9, Key::Keypad9);
    m.insert(uapi::KEY_KPMINUS, Key::KeypadMinus);
    m.insert(uapi::KEY_KP4, Key::Keypad4);
    m.insert(uapi::KEY_KP5, Key::Keypad5);
    m.insert(uapi::KEY_KP6, Key::Keypad6);
    m.insert(uapi::KEY_KPPLUS, Key::KeypadPlus);
    m.insert(uapi::KEY_KP1, Key::Keypad1);
    m.insert(uapi::KEY_KP2, Key::Keypad2);
    m.insert(uapi::KEY_KP3, Key::Keypad3);
    m.insert(uapi::KEY_KP0, Key::Keypad0);
    m.insert(uapi::KEY_KPDOT, Key::KeypadDot);
    // Germany Keyboard layout.
    //
    // m.insert(uapi::KEY_ZENKAKUHANKAKU,);
    // m.insert(uapi::KEY_102ND,);
    m.insert(uapi::KEY_F11, Key::F11);
    m.insert(uapi::KEY_F12, Key::F12);
    // Japan Keyboard layout.
    //
    // m.insert(uapi::KEY_RO,);
    // m.insert(uapi::KEY_KATAKANA,);
    // m.insert(uapi::KEY_HIRAGANA,);
    // m.insert(uapi::KEY_HENKAN,);
    // m.insert(uapi::KEY_KATAKANAHIRAGANA,);
    // m.insert(uapi::KEY_MUHENKAN,);
    // m.insert(uapi::KEY_KPJPCOMMA,);
    m.insert(uapi::KEY_KPENTER, Key::KeypadEnter);
    m.insert(uapi::KEY_RIGHTCTRL, Key::RightCtrl);
    m.insert(uapi::KEY_KPSLASH, Key::KeypadSlash);
    // SYSRQ is "PrintScreen" Key located on the right of F12 on 104 keyboard.
    m.insert(uapi::KEY_SYSRQ, Key::PrintScreen);
    m.insert(uapi::KEY_RIGHTALT, Key::RightAlt);
    // m.insert(uapi::KEY_LINEFEED,);
    m.insert(uapi::KEY_HOME, Key::Home);
    m.insert(uapi::KEY_UP, Key::Up);
    m.insert(uapi::KEY_PAGEUP, Key::PageUp);
    m.insert(uapi::KEY_LEFT, Key::Left);
    m.insert(uapi::KEY_RIGHT, Key::Right);
    m.insert(uapi::KEY_END, Key::End);
    m.insert(uapi::KEY_DOWN, Key::Down);
    m.insert(uapi::KEY_PAGEDOWN, Key::PageDown);
    m.insert(uapi::KEY_INSERT, Key::Insert);
    m.insert(uapi::KEY_DELETE, Key::Delete);
    // m.insert(uapi::KEY_MACRO,);
    m.insert(uapi::KEY_MUTE, Key::Mute);
    m.insert(uapi::KEY_VOLUMEDOWN, Key::VolumeDown);
    m.insert(uapi::KEY_VOLUMEUP, Key::VolumeUp);
    m.insert(uapi::KEY_POWER, Key::Power);
    m.insert(uapi::KEY_KPEQUAL, Key::KeypadEquals);
    // m.insert(uapi::KEY_KPPLUSMINUS,);
    m.insert(uapi::KEY_PAUSE, Key::Pause);
    // m.insert(uapi::KEY_SCALE,);
    // m.insert(uapi::KEY_KPCOMMA,);
    //
    // Japan Keyboard layout.
    //
    // m.insert(uapi::KEY_HANGEUL,);
    // m.insert(uapi::KEY_HANGUEL,);
    // m.insert(uapi::KEY_HANJA,);
    // m.insert(uapi::KEY_YEN,);
    m.insert(uapi::KEY_LEFTMETA, Key::LeftMeta);
    m.insert(uapi::KEY_RIGHTMETA, Key::RightMeta);
    // m.insert(uapi::KEY_COMPOSE,);
    // m.insert(uapi::KEY_STOP,);
    // m.insert(uapi::KEY_AGAIN,);
    // m.insert(uapi::KEY_PROPS,);
    // m.insert(uapi::KEY_UNDO,);
    // m.insert(uapi::KEY_FRONT,);
    // m.insert(uapi::KEY_COPY,);
    // m.insert(uapi::KEY_OPEN,);
    // m.insert(uapi::KEY_PASTE,);
    // m.insert(uapi::KEY_FIND,);
    // m.insert(uapi::KEY_CUT,);
    // m.insert(uapi::KEY_HELP,);
    m.insert(uapi::KEY_MENU, Key::Menu);
    // m.insert(uapi::KEY_CALC,);
    // m.insert(uapi::KEY_SETUP,);
    // m.insert(uapi::KEY_SLEEP,);
    // m.insert(uapi::KEY_WAKEUP,);
    // m.insert(uapi::KEY_FILE,);
    // m.insert(uapi::KEY_SENDFILE,);
    // m.insert(uapi::KEY_DELETEFILE,);
    // m.insert(uapi::KEY_XFER,);
    // m.insert(uapi::KEY_PROG1,);
    // m.insert(uapi::KEY_PROG2,);
    // m.insert(uapi::KEY_WWW,);
    // m.insert(uapi::KEY_MSDOS,);
    // m.insert(uapi::KEY_COFFEE,);
    // m.insert(uapi::KEY_SCREENLOCK,);
    // m.insert(uapi::KEY_ROTATE_DISPLAY,);
    // m.insert(uapi::KEY_DIRECTION,);
    // m.insert(uapi::KEY_CYCLEWINDOWS,);
    // m.insert(uapi::KEY_MAIL,);
    // m.insert(uapi::KEY_BOOKMARKS,);
    // m.insert(uapi::KEY_COMPUTER,);
    // m.insert(uapi::KEY_BACK,);
    // m.insert(uapi::KEY_FORWARD,);
    // m.insert(uapi::KEY_CLOSECD,);
    // m.insert(uapi::KEY_EJECTCD,);
    // m.insert(uapi::KEY_EJECTCLOSECD,);
    // m.insert(uapi::KEY_NEXTSONG,);
    m.insert(uapi::KEY_PLAYPAUSE, Key::PlayPause);
    // m.insert(uapi::KEY_PREVIOUSSONG,);
    // m.insert(uapi::KEY_STOPCD,);
    // m.insert(uapi::KEY_RECORD,);
    // m.insert(uapi::KEY_REWIND,);
    // m.insert(uapi::KEY_PHONE,);
    // m.insert(uapi::KEY_ISO,);
    // m.insert(uapi::KEY_CONFIG,);
    // m.insert(uapi::KEY_HOMEPAGE,);
    // m.insert(uapi::KEY_REFRESH,);
    // m.insert(uapi::KEY_EXIT,);
    // m.insert(uapi::KEY_MOVE,);
    // m.insert(uapi::KEY_EDIT,);
    // m.insert(uapi::KEY_SCROLLUP,);
    // m.insert(uapi::KEY_SCROLLDOWN,);
    // m.insert(uapi::KEY_KPLEFTPAREN,);
    // m.insert(uapi::KEY_KPRIGHTPAREN,);
    // m.insert(uapi::KEY_NEW,);
    // m.insert(uapi::KEY_REDO,);
    // m.insert(uapi::KEY_F13,);
    // m.insert(uapi::KEY_F14,);
    // m.insert(uapi::KEY_F15,);
    // m.insert(uapi::KEY_F16,);
    // m.insert(uapi::KEY_F17,);
    // m.insert(uapi::KEY_F18,);
    // m.insert(uapi::KEY_F19,);
    // m.insert(uapi::KEY_F20,);
    // m.insert(uapi::KEY_F21,);
    // m.insert(uapi::KEY_F22,);
    // m.insert(uapi::KEY_F23,);
    // m.insert(uapi::KEY_F24,);
    // m.insert(uapi::KEY_PLAYCD,);
    // m.insert(uapi::KEY_PAUSECD,);
    // m.insert(uapi::KEY_PROG3,);
    // m.insert(uapi::KEY_PROG4,);
    // m.insert(uapi::KEY_ALL_APPLICATIONS,);
    // m.insert(uapi::KEY_DASHBOARD,);
    // m.insert(uapi::KEY_SUSPEND,);
    // m.insert(uapi::KEY_CLOSE,);
    // m.insert(uapi::KEY_PLAY,);
    // m.insert(uapi::KEY_FASTFORWARD,);
    // m.insert(uapi::KEY_BASSBOOST,);
    // m.insert(uapi::KEY_PRINT,);
    // m.insert(uapi::KEY_HP,);
    // m.insert(uapi::KEY_CAMERA,);
    // m.insert(uapi::KEY_SOUND,);
    // m.insert(uapi::KEY_QUESTION,);
    // m.insert(uapi::KEY_EMAIL,);
    // m.insert(uapi::KEY_CHAT,);
    // m.insert(uapi::KEY_SEARCH,);
    // m.insert(uapi::KEY_CONNECT,);
    // m.insert(uapi::KEY_FINANCE,);
    // m.insert(uapi::KEY_SPORT,);
    // m.insert(uapi::KEY_SHOP,);
    // m.insert(uapi::KEY_ALTERASE,);
    // m.insert(uapi::KEY_CANCEL,);
    m.insert(uapi::KEY_BRIGHTNESSDOWN, Key::BrightnessDown);
    m.insert(uapi::KEY_BRIGHTNESSUP, Key::BrightnessUp);
    // m.insert(uapi::KEY_MEDIA,);
    // m.insert(uapi::KEY_SWITCHVIDEOMODE,);
    // m.insert(uapi::KEY_KBDILLUMTOGGLE,);
    // m.insert(uapi::KEY_KBDILLUMDOWN,);
    // m.insert(uapi::KEY_KBDILLUMUP,);
    // m.insert(uapi::KEY_SEND,);
    // m.insert(uapi::KEY_REPLY,);
    // m.insert(uapi::KEY_FORWARDMAIL,);
    // m.insert(uapi::KEY_SAVE,);
    // m.insert(uapi::KEY_DOCUMENTS,);
    // m.insert(uapi::KEY_BATTERY,);
    // m.insert(uapi::KEY_BLUETOOTH,);
    // m.insert(uapi::KEY_WLAN,);
    // m.insert(uapi::KEY_UWB,);
    // m.insert(uapi::KEY_UNKNOWN,);
    // m.insert(uapi::KEY_VIDEO_NEXT,);
    // m.insert(uapi::KEY_VIDEO_PREV,);
    // m.insert(uapi::KEY_BRIGHTNESS_CYCLE,);
    // m.insert(uapi::KEY_BRIGHTNESS_AUTO,);
    // m.insert(uapi::KEY_BRIGHTNESS_ZERO,);
    // m.insert(uapi::KEY_DISPLAY_OFF,);
    // m.insert(uapi::KEY_WWAN,);
    // m.insert(uapi::KEY_WIMAX,);
    // m.insert(uapi::KEY_RFKILL,);
    // m.insert(uapi::KEY_MICMUTE,);
    // m.insert(uapi::KEY_OK,);
    // m.insert(uapi::KEY_SELECT,);
    // m.insert(uapi::KEY_GOTO,);
    // m.insert(uapi::KEY_CLEAR,);
    // m.insert(uapi::KEY_POWER2,);
    // m.insert(uapi::KEY_OPTION,);
    // m.insert(uapi::KEY_INFO,);
    // m.insert(uapi::KEY_TIME,);
    // m.insert(uapi::KEY_VENDOR,);
    // m.insert(uapi::KEY_ARCHIVE,);
    // m.insert(uapi::KEY_PROGRAM,);
    // m.insert(uapi::KEY_CHANNEL,);
    // m.insert(uapi::KEY_FAVORITES,);
    // m.insert(uapi::KEY_EPG,);
    // m.insert(uapi::KEY_PVR,);
    // m.insert(uapi::KEY_MHP,);
    // m.insert(uapi::KEY_LANGUAGE,);
    // m.insert(uapi::KEY_TITLE,);
    // m.insert(uapi::KEY_SUBTITLE,);
    // m.insert(uapi::KEY_ANGLE,);
    // m.insert(uapi::KEY_FULL_SCREEN,);
    // m.insert(uapi::KEY_ZOOM,);
    // m.insert(uapi::KEY_MODE,);
    // m.insert(uapi::KEY_KEYBOARD,);
    // m.insert(uapi::KEY_ASPECT_RATIO,);
    // m.insert(uapi::KEY_SCREEN,);
    // m.insert(uapi::KEY_PC,);
    // m.insert(uapi::KEY_TV,);
    // m.insert(uapi::KEY_TV2,);
    // m.insert(uapi::KEY_VCR,);
    // m.insert(uapi::KEY_VCR2,);
    // m.insert(uapi::KEY_SAT,);
    // m.insert(uapi::KEY_SAT2,);
    // m.insert(uapi::KEY_CD,);
    // m.insert(uapi::KEY_TAPE,);
    // m.insert(uapi::KEY_RADIO,);
    // m.insert(uapi::KEY_TUNER,);
    // m.insert(uapi::KEY_PLAYER,);
    // m.insert(uapi::KEY_TEXT,);
    // m.insert(uapi::KEY_DVD,);
    // m.insert(uapi::KEY_AUX,);
    // m.insert(uapi::KEY_MP3,);
    // m.insert(uapi::KEY_AUDIO,);
    // m.insert(uapi::KEY_VIDEO,);
    // m.insert(uapi::KEY_DIRECTORY,);
    // m.insert(uapi::KEY_LIST,);
    // m.insert(uapi::KEY_MEMO,);
    // m.insert(uapi::KEY_CALENDAR,);
    // m.insert(uapi::KEY_RED,);
    // m.insert(uapi::KEY_GREEN,);
    // m.insert(uapi::KEY_YELLOW,);
    // m.insert(uapi::KEY_BLUE,);
    // m.insert(uapi::KEY_CHANNELUP,);
    // m.insert(uapi::KEY_CHANNELDOWN,);
    // m.insert(uapi::KEY_FIRST,);
    // m.insert(uapi::KEY_LAST,);
    // m.insert(uapi::KEY_AB,);
    // m.insert(uapi::KEY_NEXT,);
    // m.insert(uapi::KEY_RESTART,);
    // m.insert(uapi::KEY_SLOW,);
    // m.insert(uapi::KEY_SHUFFLE,);
    // m.insert(uapi::KEY_BREAK,);
    // m.insert(uapi::KEY_PREVIOUS,);
    // m.insert(uapi::KEY_DIGITS,);
    // m.insert(uapi::KEY_TEEN,);
    // m.insert(uapi::KEY_TWEN,);
    // m.insert(uapi::KEY_VIDEOPHONE,);
    // m.insert(uapi::KEY_GAMES,);
    // m.insert(uapi::KEY_ZOOMIN,);
    // m.insert(uapi::KEY_ZOOMOUT,);
    // m.insert(uapi::KEY_ZOOMRESET,);
    // m.insert(uapi::KEY_WORDPROCESSOR,);
    // m.insert(uapi::KEY_EDITOR,);
    // m.insert(uapi::KEY_SPREADSHEET,);
    // m.insert(uapi::KEY_GRAPHICSEDITOR,);
    // m.insert(uapi::KEY_PRESENTATION,);
    // m.insert(uapi::KEY_DATABASE,);
    // m.insert(uapi::KEY_NEWS,);
    // m.insert(uapi::KEY_VOICEMAIL,);
    // m.insert(uapi::KEY_ADDRESSBOOK,);
    // m.insert(uapi::KEY_MESSENGER,);
    // m.insert(uapi::KEY_DISPLAYTOGGLE,);
    // m.insert(uapi::KEY_BRIGHTNESS_TOGGLE,);
    // m.insert(uapi::KEY_SPELLCHECK,);
    // m.insert(uapi::KEY_LOGOFF,);
    // m.insert(uapi::KEY_DOLLAR,);
    // m.insert(uapi::KEY_EURO,);
    // m.insert(uapi::KEY_FRAMEBACK,);
    // m.insert(uapi::KEY_FRAMEFORWARD,);
    // m.insert(uapi::KEY_CONTEXT_MENU,);
    // m.insert(uapi::KEY_MEDIA_REPEAT,);
    // m.insert(uapi::KEY_10CHANNELSUP,);
    // m.insert(uapi::KEY_10CHANNELSDOWN,);
    // m.insert(uapi::KEY_IMAGES,);
    // m.insert(uapi::KEY_NOTIFICATION_CENTER,);
    // m.insert(uapi::KEY_PICKUP_PHONE,);
    // m.insert(uapi::KEY_HANGUP_PHONE,);
    // m.insert(uapi::KEY_DEL_EOL,);
    // m.insert(uapi::KEY_DEL_EOS,);
    // m.insert(uapi::KEY_INS_LINE,);
    // m.insert(uapi::KEY_DEL_LINE,);
    // m.insert(uapi::KEY_FN,);
    // m.insert(uapi::KEY_FN_ESC,);
    // m.insert(uapi::KEY_FN_F1,);
    // m.insert(uapi::KEY_FN_F2,);
    // m.insert(uapi::KEY_FN_F3,);
    // m.insert(uapi::KEY_FN_F4,);
    // m.insert(uapi::KEY_FN_F5,);
    // m.insert(uapi::KEY_FN_F6,);
    // m.insert(uapi::KEY_FN_F7,);
    // m.insert(uapi::KEY_FN_F8,);
    // m.insert(uapi::KEY_FN_F9,);
    // m.insert(uapi::KEY_FN_F10,);
    // m.insert(uapi::KEY_FN_F11,);
    // m.insert(uapi::KEY_FN_F12,);
    // m.insert(uapi::KEY_FN_1,);
    // m.insert(uapi::KEY_FN_2,);
    // m.insert(uapi::KEY_FN_D,);
    // m.insert(uapi::KEY_FN_E,);
    // m.insert(uapi::KEY_FN_F,);
    // m.insert(uapi::KEY_FN_S,);
    // m.insert(uapi::KEY_FN_B,);
    // m.insert(uapi::KEY_FN_RIGHT_SHIFT,);
    // m.insert(uapi::KEY_BRL_DOT1,);
    // m.insert(uapi::KEY_BRL_DOT2,);
    // m.insert(uapi::KEY_BRL_DOT3,);
    // m.insert(uapi::KEY_BRL_DOT4,);
    // m.insert(uapi::KEY_BRL_DOT5,);
    // m.insert(uapi::KEY_BRL_DOT6,);
    // m.insert(uapi::KEY_BRL_DOT7,);
    // m.insert(uapi::KEY_BRL_DOT8,);
    // m.insert(uapi::KEY_BRL_DOT9,);
    // m.insert(uapi::KEY_BRL_DOT10,);
    // m.insert(uapi::KEY_NUMERIC_0,);
    // m.insert(uapi::KEY_NUMERIC_1,);
    // m.insert(uapi::KEY_NUMERIC_2,);
    // m.insert(uapi::KEY_NUMERIC_3,);
    // m.insert(uapi::KEY_NUMERIC_4,);
    // m.insert(uapi::KEY_NUMERIC_5,);
    // m.insert(uapi::KEY_NUMERIC_6,);
    // m.insert(uapi::KEY_NUMERIC_7,);
    // m.insert(uapi::KEY_NUMERIC_8,);
    // m.insert(uapi::KEY_NUMERIC_9,);
    // m.insert(uapi::KEY_NUMERIC_STAR,);
    // m.insert(uapi::KEY_NUMERIC_POUND,);
    // m.insert(uapi::KEY_NUMERIC_A,);
    // m.insert(uapi::KEY_NUMERIC_B,);
    // m.insert(uapi::KEY_NUMERIC_C,);
    // m.insert(uapi::KEY_NUMERIC_D,);
    // m.insert(uapi::KEY_CAMERA_FOCUS,);
    // m.insert(uapi::KEY_WPS_BUTTON,);
    // m.insert(uapi::KEY_TOUCHPAD_TOGGLE,);
    // m.insert(uapi::KEY_TOUCHPAD_ON,);
    // m.insert(uapi::KEY_TOUCHPAD_OFF,);
    // m.insert(uapi::KEY_CAMERA_ZOOMIN,);
    // m.insert(uapi::KEY_CAMERA_ZOOMOUT,);
    // m.insert(uapi::KEY_CAMERA_UP,);
    // m.insert(uapi::KEY_CAMERA_DOWN,);
    // m.insert(uapi::KEY_CAMERA_LEFT,);
    // m.insert(uapi::KEY_CAMERA_RIGHT,);
    // m.insert(uapi::KEY_ATTENDANT_ON,);
    // m.insert(uapi::KEY_ATTENDANT_OFF,);
    // m.insert(uapi::KEY_ATTENDANT_TOGGLE,);
    // m.insert(uapi::KEY_LIGHTS_TOGGLE,);
    // m.insert(uapi::KEY_ALS_TOGGLE,);
    // m.insert(uapi::KEY_ROTATE_LOCK_TOGGLE,);
    // m.insert(uapi::KEY_BUTTONCONFIG,);
    // m.insert(uapi::KEY_TASKMANAGER,);
    // m.insert(uapi::KEY_JOURNAL,);
    // m.insert(uapi::KEY_CONTROLPANEL,);
    // m.insert(uapi::KEY_APPSELECT,);
    // m.insert(uapi::KEY_SCREENSAVER,);
    // m.insert(uapi::KEY_VOICECOMMAND,);
    // m.insert(uapi::KEY_ASSISTANT,);
    // m.insert(uapi::KEY_KBD_LAYOUT_NEXT,);
    // m.insert(uapi::KEY_EMOJI_PICKER,);
    // m.insert(uapi::KEY_DICTATE,);
    // m.insert(uapi::KEY_CAMERA_ACCESS_ENABLE,);
    // m.insert(uapi::KEY_CAMERA_ACCESS_DISABLE,);
    // m.insert(uapi::KEY_CAMERA_ACCESS_TOGGLE,);
    // m.insert(uapi::KEY_BRIGHTNESS_MIN,);
    // m.insert(uapi::KEY_BRIGHTNESS_MAX,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_PREV,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_NEXT,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_PREVGROUP,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_NEXTGROUP,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_ACCEPT,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_CANCEL,);
    // m.insert(uapi::KEY_RIGHT_UP,);
    // m.insert(uapi::KEY_RIGHT_DOWN,);
    // m.insert(uapi::KEY_LEFT_UP,);
    // m.insert(uapi::KEY_LEFT_DOWN,);
    // m.insert(uapi::KEY_ROOT_MENU,);
    // m.insert(uapi::KEY_MEDIA_TOP_MENU,);
    // m.insert(uapi::KEY_NUMERIC_11,);
    // m.insert(uapi::KEY_NUMERIC_12,);
    // m.insert(uapi::KEY_AUDIO_DESC,);
    // m.insert(uapi::KEY_3D_MODE,);
    // m.insert(uapi::KEY_NEXT_FAVORITE,);
    // m.insert(uapi::KEY_STOP_RECORD,);
    // m.insert(uapi::KEY_PAUSE_RECORD,);
    // m.insert(uapi::KEY_VOD,);
    // m.insert(uapi::KEY_UNMUTE,);
    // m.insert(uapi::KEY_FASTREVERSE,);
    // m.insert(uapi::KEY_SLOWREVERSE,);
    // m.insert(uapi::KEY_DATA,);
    // m.insert(uapi::KEY_ONSCREEN_KEYBOARD,);
    // m.insert(uapi::KEY_PRIVACY_SCREEN_TOGGLE,);
    // m.insert(uapi::KEY_SELECTIVE_SCREENSHOT,);
    // m.insert(uapi::KEY_NEXT_ELEMENT,);
    // m.insert(uapi::KEY_PREVIOUS_ELEMENT,);
    // m.insert(uapi::KEY_AUTOPILOT_ENGAGE_TOGGLE,);
    // m.insert(uapi::KEY_MARK_WAYPOINT,);
    // m.insert(uapi::KEY_SOS,);
    // m.insert(uapi::KEY_NAV_CHART,);
    // m.insert(uapi::KEY_FISHING_CHART,);
    // m.insert(uapi::KEY_SINGLE_RANGE_RADAR,);
    // m.insert(uapi::KEY_DUAL_RANGE_RADAR,);
    // m.insert(uapi::KEY_RADAR_OVERLAY,);
    // m.insert(uapi::KEY_TRADITIONAL_SONAR,);
    // m.insert(uapi::KEY_CLEARVU_SONAR,);
    // m.insert(uapi::KEY_SIDEVU_SONAR,);
    // m.insert(uapi::KEY_NAV_INFO,);
    // m.insert(uapi::KEY_BRIGHTNESS_MENU,);
    // m.insert(uapi::KEY_MACRO1,);
    // m.insert(uapi::KEY_MACRO2,);
    // m.insert(uapi::KEY_MACRO3,);
    // m.insert(uapi::KEY_MACRO4,);
    // m.insert(uapi::KEY_MACRO5,);
    // m.insert(uapi::KEY_MACRO6,);
    // m.insert(uapi::KEY_MACRO7,);
    // m.insert(uapi::KEY_MACRO8,);
    // m.insert(uapi::KEY_MACRO9,);
    // m.insert(uapi::KEY_MACRO10,);
    // m.insert(uapi::KEY_MACRO11,);
    // m.insert(uapi::KEY_MACRO12,);
    // m.insert(uapi::KEY_MACRO13,);
    // m.insert(uapi::KEY_MACRO14,);
    // m.insert(uapi::KEY_MACRO15,);
    // m.insert(uapi::KEY_MACRO16,);
    // m.insert(uapi::KEY_MACRO17,);
    // m.insert(uapi::KEY_MACRO18,);
    // m.insert(uapi::KEY_MACRO19,);
    // m.insert(uapi::KEY_MACRO20,);
    // m.insert(uapi::KEY_MACRO21,);
    // m.insert(uapi::KEY_MACRO22,);
    // m.insert(uapi::KEY_MACRO23,);
    // m.insert(uapi::KEY_MACRO24,);
    // m.insert(uapi::KEY_MACRO25,);
    // m.insert(uapi::KEY_MACRO26,);
    // m.insert(uapi::KEY_MACRO27,);
    // m.insert(uapi::KEY_MACRO28,);
    // m.insert(uapi::KEY_MACRO29,);
    // m.insert(uapi::KEY_MACRO30,);
    // m.insert(uapi::KEY_MACRO_RECORD_START,);
    // m.insert(uapi::KEY_MACRO_RECORD_STOP,);
    // m.insert(uapi::KEY_MACRO_PRESET_CYCLE,);
    // m.insert(uapi::KEY_MACRO_PRESET1,);
    // m.insert(uapi::KEY_MACRO_PRESET2,);
    // m.insert(uapi::KEY_MACRO_PRESET3,);
    // m.insert(uapi::KEY_KBD_LCD_MENU1,);
    // m.insert(uapi::KEY_KBD_LCD_MENU2,);
    // m.insert(uapi::KEY_KBD_LCD_MENU3,);
    // m.insert(uapi::KEY_KBD_LCD_MENU4,);
    // m.insert(uapi::KEY_KBD_LCD_MENU5,);

    m
}

#[cfg(test)]
mod test {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn init_key_map_no_assert_failed() {
        let _ = init_key_map();
    }

    #[test]
    fn linux_keycode_to_fuchsia_input_key() {
        let km = init_key_map();
        for (&linux_key, &want) in km.linux_to_fuchsia.iter() {
            let got = km.linux_keycode_to_fuchsia_input_key(linux_key);
            assert_eq!(want, got);
        }
    }

    #[test]
    fn unknown_linux_keycode_to_fuchsia_input_key() {
        let km = init_key_map();
        let got = km.linux_keycode_to_fuchsia_input_key(701);
        assert_eq!(Key::Unknown, got);
    }

    #[test]
    fn fuchsia_input_key_to_linux_keycode() {
        let km = init_key_map();
        for (&fuchsia_key, &want) in km.fuchsia_to_linux.iter() {
            let got = km.fuchsia_input_key_to_linux_keycode(fuchsia_key);
            assert_eq!(want, got);
        }
    }
}
