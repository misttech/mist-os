// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next::bind::{
    Client, ClientEnd, RequestBuffer, Responder, ResponseBuffer, Server, ServerEnd,
};
use fidl_next::protocol::mpsc::Mpsc;
use fidl_next::protocol::Transport;
use fidl_next_examples_calculator::{
    calculator, calculator_add_result, Calculator, CalculatorAddRequest, CalculatorAddResponse,
    CalculatorAddResult, CalculatorClient as _, CalculatorClientHandler, CalculatorDivideResponse,
    CalculatorDivideResult, CalculatorServerHandler, DivisionError,
};
use fuchsia_async::{Scope, Task};

struct MyCalculatorClient;

impl<T: Transport> CalculatorClientHandler<T> for MyCalculatorClient {
    fn on_error(&mut self, _message: ResponseBuffer<T, calculator::OnError>) {
        eprintln!("Client error!");
    }

    fn handle_unknown_interaction(&mut self, _ordinal: u64) {
        panic!("Unknown interaction!");
    }
}

struct MyCalculatorServer<T: Transport> {
    server: Server<T, Calculator>,
    scope: Scope,
}

impl<T: Transport> CalculatorServerHandler<T> for MyCalculatorServer<T> {
    fn add(
        &mut self,
        mut request: RequestBuffer<T, calculator::Add>,
        responder: Responder<calculator::Add>,
    ) {
        let server = self.server.clone();
        self.scope.spawn(async move {
            let request = request.decode().expect("failed to decode add message");
            let mut response =
                CalculatorAddResult::Response(CalculatorAddResponse { sum: request.a + request.b });
            responder.respond(&server, &mut response).unwrap().await.unwrap();
        });
    }

    fn divide<'buf>(
        &mut self,
        mut request: RequestBuffer<T, calculator::Divide>,
        responder: Responder<calculator::Divide>,
    ) {
        let server = self.server.clone();
        self.scope.spawn(async move {
            let request = request.decode().expect("failed to decode message");
            let mut response = if request.divisor != 0 {
                CalculatorDivideResult::Response(CalculatorDivideResponse {
                    quotient: request.dividend / request.divisor,
                    remainder: request.dividend % request.divisor,
                })
            } else {
                CalculatorDivideResult::Err(DivisionError::DivideByZero)
            };
            responder.respond(&server, &mut response).unwrap().await.unwrap();
        });
    }

    fn clear(&mut self) {
        println!("cleared")
    }

    fn handle_unknown_interaction(&mut self, _ordinal: u64) {
        panic!("unknown interaction");
    }
}

#[fuchsia_async::run_singlethreaded]
async fn main() {
    let (client_mpsc, server_mpsc) = Mpsc::new();
    let client_end = ClientEnd::<_, Calculator>::from_untyped(client_mpsc);
    let server_end = ServerEnd::<_, Calculator>::from_untyped(server_mpsc);

    let (client, mut client_dispatcher) = Client::new(client_end);
    let (server, mut server_dispatcher) = Server::new(server_end);

    let client_task = Task::spawn(async move {
        client_dispatcher.run(MyCalculatorClient).await.unwrap();
    });
    let server_task = Task::spawn(async move {
        server_dispatcher.run(MyCalculatorServer { server, scope: Scope::new() }).await.unwrap();
    });

    let mut buffer = client.add(&mut CalculatorAddRequest { a: 16, b: 26 }).unwrap().await.unwrap();
    let result = buffer.decode().unwrap();
    let calculator_add_result::Ref::Response(response) = result.as_ref() else { panic!() };
    assert_eq!(response.sum, 42);

    client.untyped().close();

    client_task.await;
    server_task.await;

    println!("All tests passed!");
}
