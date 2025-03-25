// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::log::LogClient;
use anyhow::Error;
use std::io::sink;
use term_model::ansi::Processor;
use term_model::event::EventListener;

const LOGO_TEXT: &str = "
\r
                                      ff    ff  ff\r
                                   ff  fffffffffff ff\r
                                  f ffffffffffffffff f\r
                                ff ffffffffffffffffffff\r
                                f fffffffff        ffff\r
                               f ffffffff            ff\r
                               f fffffff              f\r
                              ff fffffff\r
                              f  ffffff\r
                               fffffffff             f\r
                        fffffff                    fff\r
                    ffffffffffffffffffffffff   ffffff\r
                 ffffffffffffffffffffffffffffffffff\r
                ffffff   fffffff         ffffff\r
               fffff  fff      ffffffffff\r
              fffff ff         fffffff  f\r
              ffff fff         fffffff ff\r
             fffff ff         ffffffff f\r
              ffff fff       ffffffff  f\r
              ffff ffffffffffffffffff f\r
               ffff fffffffffffffff  f\r
                fffff  ffffffffff  ff\r
                  fffff         fff\r
                    fffffffffffff\r
";

pub struct Logo;
impl Logo {
    pub fn start<T: LogClient>(client: &T, id: u32) -> Result<(), Error>
    where
        <T as LogClient>::Listener: EventListener,
    {
        let client = client.clone();
        let terminal =
            client.create_terminal(id, "logo".to_string()).expect("failed to create terminal");
        let term = terminal.clone_term();

        let mut sink = sink();
        let mut parser = Processor::new();
        {
            let mut term = term.borrow_mut();
            for byte in LOGO_TEXT.as_bytes() {
                parser.advance(&mut *term, *byte, &mut sink);
            }
            client.request_update(id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colors::ColorScheme;
    use crate::terminal::Terminal;
    use fuchsia_async as fasync;
    use term_model::event::Event;

    #[derive(Default)]
    struct TestListener;

    impl EventListener for TestListener {
        fn send_event(&self, _event: Event) {}
    }

    #[derive(Default, Clone)]
    struct TestLogClient;

    impl LogClient for TestLogClient {
        type Listener = TestListener;

        fn create_terminal(
            &self,
            _id: u32,
            title: String,
        ) -> Result<Terminal<Self::Listener>, Error> {
            Ok(Terminal::new(TestListener::default(), title, ColorScheme::default(), 1024, None))
        }
        fn request_update(&self, _id: u32) {}
    }

    #[fasync::run_singlethreaded(test)]
    async fn can_start_logo() -> Result<(), Error> {
        let client = TestLogClient::default();
        let _ = Logo::start(&client, 0)?;
        Ok(())
    }
}
