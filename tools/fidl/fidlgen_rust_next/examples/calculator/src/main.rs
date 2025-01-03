// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next::bind::{
    Client, ClientDispatcher, ClientEnd, RequestBuffer, Responder, ResponseBuffer, Server,
    ServerDispatcher, ServerEnd,
};
use fidl_next::protocol::mpsc::Mpsc;
use fidl_next::protocol::Transport;
use fidl_next_examples_calculator::{
    calculator, Calculator, CalculatorClientHandler, CalculatorServerHandler,
};
use fuchsia_async::{Scope, Task};

struct MyCalculatorClient;

impl<T: Transport> CalculatorClientHandler<T> for MyCalculatorClient {
    fn on_error(
        &mut self,
        client: &Client<T, Calculator>,
        _: ResponseBuffer<T, calculator::OnError>,
    ) {
        println!("Client received an error event, closing connection.");
        client.close();
    }
}

struct MyCalculatorServer {
    scope: Scope,
}

impl<T: Transport> CalculatorServerHandler<T> for MyCalculatorServer {
    fn add(
        &mut self,
        server: &Server<T, Calculator>,
        mut request: RequestBuffer<T, calculator::Add>,
        responder: Responder<calculator::Add>,
    ) {
        let server = server.clone();
        self.scope.spawn(async move {
            let Ok(request) = request.decode() else { return server.close() };

            use fidl_next_examples_calculator::{CalculatorAddResponse, CalculatorAddResult};

            let mut response =
                CalculatorAddResult::Response(CalculatorAddResponse { sum: request.a + request.b });
            let Ok(_) = responder.respond(&server, &mut response).unwrap().await else {
                return server.close();
            };
        });
    }

    fn divide(
        &mut self,
        server: &Server<T, Calculator>,
        mut request: RequestBuffer<T, calculator::Divide>,
        responder: Responder<calculator::Divide>,
    ) {
        let server = server.clone();
        self.scope.spawn(async move {
            let Ok(request) = request.decode() else { return server.close() };

            use fidl_next_examples_calculator::{
                CalculatorDivideResponse, CalculatorDivideResult, DivisionError,
            };

            let mut response = if request.divisor != 0 {
                CalculatorDivideResult::Response(CalculatorDivideResponse {
                    quotient: request.dividend / request.divisor,
                    remainder: request.dividend % request.divisor,
                })
            } else {
                CalculatorDivideResult::Err(DivisionError::DivideByZero)
            };
            let Ok(_) = responder.respond(&server, &mut response).unwrap().await else {
                return server.close();
            };
        });
    }

    fn clear(&mut self, _: &Server<T, Calculator>) {
        println!("cleared")
    }
}

#[fuchsia_async::run_singlethreaded]
async fn main() {
    let (client_mpsc, server_mpsc) = Mpsc::new();
    let client_end = ClientEnd::<_, Calculator>::from_untyped(client_mpsc);
    let server_end = ServerEnd::<_, Calculator>::from_untyped(server_mpsc);

    let mut client_dispatcher = ClientDispatcher::new(client_end);
    let client = client_dispatcher.client().clone();
    let mut server_dispatcher = ServerDispatcher::new(server_end);

    let client_task = Task::spawn(async move {
        client_dispatcher.run(MyCalculatorClient).await.unwrap();
    });
    let server_task = Task::spawn(async move {
        server_dispatcher.run(MyCalculatorServer { scope: Scope::new() }).await.unwrap();
    });

    use fidl_next_examples_calculator::{
        calculator_add_result, CalculatorAddRequest, CalculatorClient as _,
    };

    let mut buffer = client.add(&mut CalculatorAddRequest { a: 16, b: 26 }).unwrap().await.unwrap();
    let result = buffer.decode().unwrap();
    let calculator_add_result::Ref::Response(response) = result.as_ref() else { panic!() };
    assert_eq!(response.sum, 42);

    client.close();

    client_task.await;
    server_task.await;

    println!("All tests passed!");
}
