// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next::{
    Client, ClientEnd, ClientSender, Flexible, FlexibleResult, Request, Responder, Response,
    Server, ServerEnd, ServerSender, Transport,
};
use fidl_next_examples_calculator::calculator::prelude::*;
use fuchsia_async::Task;

struct MyCalculatorClient;

impl<T: Transport> CalculatorClientHandler<T> for MyCalculatorClient {
    async fn on_error(
        &mut self,
        sender: &ClientSender<Calculator, T>,
        response: Response<calculator::OnError, T>,
    ) {
        assert_eq!(response.status_code, 100);

        println!("Client received an error event, closing connection");
        sender.close();
    }
}

struct MyCalculatorServer;

impl<T: Transport + 'static> CalculatorServerHandler<T> for MyCalculatorServer {
    async fn add(
        &mut self,
        sender: &ServerSender<Calculator, T>,
        request: Request<calculator::Add, T>,
        responder: Responder<calculator::Add>,
    ) {
        println!("{} + {} = {}", request.a, request.b, request.a + request.b);
        let response = Flexible::Ok(CalculatorAddResponse { sum: request.a + request.b });

        if responder.respond(&sender, response).unwrap().await.is_err() {
            sender.close();
        }
    }

    async fn divide(
        &mut self,
        sender: &ServerSender<Calculator, T>,
        request: Request<calculator::Divide, T>,
        responder: Responder<calculator::Divide>,
    ) {
        let response = if request.divisor != 0 {
            println!(
                "{} / {} = {} rem {}",
                request.dividend,
                request.divisor,
                request.dividend / request.divisor,
                request.dividend % request.divisor,
            );
            FlexibleResult::Ok(CalculatorDivideResponse {
                quotient: request.dividend / request.divisor,
                remainder: request.dividend % request.divisor,
            })
        } else {
            println!("{} / 0 = undefined", request.dividend);
            FlexibleResult::Err(DivisionError::DivideByZero)
        };

        let sender = sender.clone();
        if responder.respond(&sender, response).unwrap().await.is_err() {
            return sender.close();
        }
    }

    async fn clear(&mut self, sender: &ServerSender<Calculator, T>) {
        println!("Cleared, sending an error back to close the connection");

        let sender = sender.clone();
        sender.on_error(CalculatorOnErrorRequest { status_code: 100 }).unwrap().await.unwrap();
    }
}

#[cfg(not(target_os = "fuchsia"))]
type Endpoint = fidl_next::protocol::mpsc::Mpsc;

#[cfg(target_os = "fuchsia")]
type Endpoint = zx::Channel;

fn make_transport() -> (Endpoint, Endpoint) {
    #[cfg(not(target_os = "fuchsia"))]
    {
        fidl_next::protocol::mpsc::Mpsc::new()
    }

    #[cfg(target_os = "fuchsia")]
    {
        zx::Channel::create()
    }
}

async fn create_endpoints(
) -> (ClientSender<Calculator, Endpoint>, Task<()>, ServerSender<Calculator, Endpoint>, Task<()>) {
    let (client_transport, server_transport) = make_transport();

    let client_end = ClientEnd::<Calculator, _>::from_untyped(client_transport);
    let server_end = ServerEnd::<Calculator, _>::from_untyped(server_transport);

    let mut client = Client::new(client_end);
    let client_sender = client.sender().clone();
    let mut server = Server::new(server_end);
    let server_sender = server.sender().clone();

    let client_task = Task::spawn(async move {
        client.run(MyCalculatorClient).await.unwrap();
        println!("client exited");
    });
    let server_task = Task::spawn(async move {
        server.run(MyCalculatorServer).await.unwrap();
        println!("server exited");
    });

    (client_sender, client_task, server_sender, server_task)
}

async fn add(client_sender: &ClientSender<Calculator, Endpoint>) {
    let result = client_sender
        .add(CalculatorAddRequest { a: 16, b: 26 })
        .expect("failed to encode add request")
        .await
        .expect("failed to send, receive, or decode response to add request");
    let response = result.ok().expect("add request failed with an error");

    assert_eq!(response.sum, 42);
}

async fn divide(client_sender: &ClientSender<Calculator, Endpoint>) {
    // Normal division
    let result = client_sender
        .divide(CalculatorDivideRequest { dividend: 100, divisor: 3 })
        .expect("failed to encode divide request")
        .await
        .expect("failed to send, receive, or decode response to divide request");
    let response = result.ok().expect("divide request failed with an error");

    assert_eq!(response.quotient, 33);
    assert_eq!(response.remainder, 1);

    // Cause an error
    let result = client_sender
        .divide(CalculatorDivideRequest { dividend: 42, divisor: 0 })
        .expect("failed to encode divide request")
        .await
        .expect("failed to send, receive, or decode response to divide request");

    let error = result.err().expect("divide request succeeded unexpectedly");
    assert_eq!(DivisionError::DivideByZero, (*error).into());
}

async fn clear(client_sender: &ClientSender<Calculator, Endpoint>) {
    client_sender.clear().unwrap().await.unwrap();
}

#[fuchsia_async::run_singlethreaded]
async fn main() {
    let (client_sender, client_task, _, server_task) = create_endpoints().await;

    add(&client_sender).await;
    divide(&client_sender).await;
    clear(&client_sender).await;

    client_task.await;
    server_task.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_add() {
        let (client_sender, client_task, _, server_task) = create_endpoints().await;

        add(&client_sender).await;

        // Dropping the client task will close the stream
        drop(client_sender);
        drop(client_task);
        server_task.await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_divide() {
        let (client_sender, client_task, _, server_task) = create_endpoints().await;

        divide(&client_sender).await;

        // Dropping the client task will close the stream
        drop(client_sender);
        drop(client_task);
        server_task.await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_clear() {
        let (client_sender, client_task, _, server_task) = create_endpoints().await;

        clear(&client_sender).await;

        // clear() triggers a stream closure, so we can just await both tasks
        drop(client_sender);
        client_task.await;
        server_task.await;
    }
}
