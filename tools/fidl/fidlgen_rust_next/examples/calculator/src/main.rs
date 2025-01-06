// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next::bind::{
    Client, ClientEnd, ClientSender, RequestBuffer, Responder, ResponseBuffer, Server, ServerEnd,
    ServerSender,
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
        sender: &ClientSender<T, Calculator>,
        _: ResponseBuffer<T, calculator::OnError>,
    ) {
        println!("Client received an error event, closing connection");
        sender.close();
    }
}

struct MyCalculatorServer {
    scope: Scope,
}

impl<T: Transport> CalculatorServerHandler<T> for MyCalculatorServer {
    fn add(
        &mut self,
        sender: &ServerSender<T, Calculator>,
        mut request: RequestBuffer<T, calculator::Add>,
        responder: Responder<calculator::Add>,
    ) {
        let sender = sender.clone();
        self.scope.spawn(async move {
            let Ok(request) = request.decode() else { return sender.close() };

            use fidl_next_examples_calculator::{CalculatorAddResponse, CalculatorAddResult};

            println!("{} + {} = {}", request.a, request.b, request.a + request.b);
            let mut response =
                CalculatorAddResult::Response(CalculatorAddResponse { sum: request.a + request.b });
            let Ok(_) = responder.respond(&sender, &mut response).unwrap().await else {
                return sender.close();
            };
        });
    }

    fn divide(
        &mut self,
        sender: &ServerSender<T, Calculator>,
        mut request: RequestBuffer<T, calculator::Divide>,
        responder: Responder<calculator::Divide>,
    ) {
        let sender = sender.clone();
        self.scope.spawn(async move {
            let Ok(request) = request.decode() else { return sender.close() };

            use fidl_next_examples_calculator::{
                CalculatorDivideResponse, CalculatorDivideResult, DivisionError,
            };

            let mut response = if request.divisor != 0 {
                println!(
                    "{} / {} = {} rem {}",
                    request.dividend,
                    request.divisor,
                    request.dividend / request.divisor,
                    request.dividend % request.divisor,
                );
                CalculatorDivideResult::Response(CalculatorDivideResponse {
                    quotient: request.dividend / request.divisor,
                    remainder: request.dividend % request.divisor,
                })
            } else {
                println!("{} / 0 = undefined", request.dividend);
                CalculatorDivideResult::Err(DivisionError::DivideByZero)
            };
            let Ok(_) = responder.respond(&sender, &mut response).unwrap().await else {
                return sender.close();
            };
        });
    }

    fn clear(&mut self, sender: &ServerSender<T, Calculator>) {
        let sender = sender.clone();
        self.scope.spawn(async move {
            use fidl_next_examples_calculator::CalculatorServerSender as _;

            println!("Cleared, sending an error back to close the connection");
            sender.on_error().unwrap().await.unwrap();
        });
    }
}

#[fuchsia_async::run_singlethreaded]
async fn main() {
    let (client_mpsc, server_mpsc) = Mpsc::new();
    let client_end = ClientEnd::<_, Calculator>::from_untyped(client_mpsc);
    let server_end = ServerEnd::<_, Calculator>::from_untyped(server_mpsc);

    let mut client = Client::new(client_end);
    let client_sender = client.sender().clone();
    let mut server = Server::new(server_end);

    let client_task = Task::spawn(async move {
        client.run(MyCalculatorClient).await.unwrap();
    });
    let server_task = Task::spawn(async move {
        server.run(MyCalculatorServer { scope: Scope::new() }).await.unwrap();
    });

    use fidl_next_examples_calculator::{
        calculator_add_result, calculator_divide_result, CalculatorAddRequest,
        CalculatorClientSender as _, CalculatorDivideRequest, DivisionError,
    };

    let mut buffer =
        client_sender.add(&mut CalculatorAddRequest { a: 16, b: 26 }).unwrap().await.unwrap();
    let result = buffer.decode().unwrap();
    let calculator_add_result::Ref::Response(response) = result.as_ref() else { panic!() };
    assert_eq!(response.sum, 42);

    let mut buffer = client_sender
        .divide(&mut CalculatorDivideRequest { dividend: 100, divisor: 3 })
        .unwrap()
        .await
        .unwrap();
    let result = buffer.decode().unwrap();
    let calculator_divide_result::Ref::Response(response) = result.as_ref() else { panic!() };
    assert_eq!(response.quotient, 33);
    assert_eq!(response.remainder, 1);

    let mut buffer = client_sender
        .divide(&mut CalculatorDivideRequest { dividend: 42, divisor: 0 })
        .unwrap()
        .await
        .unwrap();
    let result = buffer.decode().unwrap();
    let calculator_divide_result::Ref::Err(error) = result.as_ref() else { panic!() };
    assert_eq!(DivisionError::DivideByZero, (*error).into());

    client_sender.clear().unwrap().await.unwrap();

    client_task.await;
    server_task.await;

    println!("All tests passed!");
}
