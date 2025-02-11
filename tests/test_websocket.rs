use hyper::service::service_fn;
use hyper::{
    body::{Body, Buf, Incoming},
    Request, Response,
};
use hyper_reverse_proxy::ReverseProxy;
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::{TokioExecutor, TokioIo},
};
use std::convert::Infallible;
use std::net::IpAddr;
use std::{process::exit, time::Duration};
use test_context::test_context;
use test_context::AsyncTestContext;
use tokio::net::TcpListener;
use tokio::sync::oneshot::Sender;
use tokio::task::JoinHandle;
use tokiotest_httpserver::{take_port, HttpTestContext};

use async_tungstenite::tokio::{accept_async, connect_async};
use futures::StreamExt;
use tungstenite::Message;

struct ProxyTestContext {
    sender: Sender<()>,
    proxy_handler: JoinHandle<Result<(), hyper::Error>>,
    ws_handler: JoinHandle<()>,
    http_back: HttpTestContext,
    port: u16,
}

#[test_context(ProxyTestContext)]
#[tokio::test]
async fn test_websocket(ctx: &mut ProxyTestContext) {
    eprintln!("try and connect to port {}", ctx.port);
    let (mut client, _) = connect_async(format!("ws://127.0.0.1:{}", ctx.port))
        .await
        .unwrap();

    eprintln!("send ping");
    client.send(Message::Ping("hello".into())).await.unwrap();
    eprintln!("await pong");
    let msg = client.next().await.unwrap().unwrap();

    eprintln!("see what we've got here");
    assert!(
        matches!(&msg, Message::Pong(inner) if inner == "hello".as_bytes()),
        "did not get pong, but {:?}",
        msg
    );

    let msg = client.next().await.unwrap().unwrap();

    assert!(
        matches!(&msg, Message::Text(inner) if inner == "All done"),
        "did not get text, but {:?}",
        msg
    );
}

async fn handle<I>(
    client_ip: IpAddr,
    req: Request<I>,
    backend_port: u16,
) -> Result<Response<Incoming>, Infallible>
where
    I: Body + Send + Sync + Unpin + 'static,
    <I as Body>::Data: Buf + Send + Sync + 'static,
    <I as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    let proxy = ReverseProxy::new(
        Client::builder(TokioExecutor::new()).build::<_, I>(HttpConnector::new()),
    );
    match proxy
        .call(
            client_ip,
            format!("http://127.0.0.1:{}", backend_port).as_str(),
            req,
        )
        .await
    {
        Ok(response) => Ok(response),
        Err(err) => panic!("did not expect error: {:?}", err),
    }
}

impl AsyncTestContext for ProxyTestContext {
    async fn setup() -> ProxyTestContext {
        let http_back = HttpTestContext::setup().await;
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            println!("Unit test executed too long, perhaps its stuck...");
            exit(1);
        });

        let (sender, mut receiver) = tokio::sync::oneshot::channel::<()>();
        let ws_port = take_port();

        let ws_server = TcpListener::bind(("127.0.0.1", ws_port)).await.unwrap();
        let ws_handler = tokio::spawn(async move {
            if let Ok((stream, _)) = ws_server.accept().await {
                let mut websocket = accept_async(stream).await.unwrap();

                let msg = websocket.next().await.unwrap().unwrap();
                assert!(
                    matches!(&msg, Message::Ping(inner) if inner == "hello".as_bytes()),
                    "did not get ping, but: {:?}",
                    msg
                );

                websocket
                    .send(Message::Pong(Vec::from(b"hello")))
                    .await
                    .unwrap();

                websocket
                    .send(Message::Text("All done".to_string()))
                    .await
                    .unwrap();
            }
        });

        let graceful = hyper_util::server::graceful::GracefulShutdown::new();
        let port = take_port();

        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let proxy_handler: JoinHandle<Result<_, hyper::Error>> = tokio::spawn(async move {
            loop {
                tokio::select! {
                    conn = listener.accept() => {
                        let (stream, remote) = conn.unwrap();
                        let io = TokioIo::new(stream);

                        let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                        let conn = builder.serve_connection_with_upgrades( io, service_fn(move |req| handle(remote.ip(), req, ws_port)));
                        let conn = graceful.watch(conn.into_owned());
                        tokio::spawn(async move {
                            eprintln!("handle a request");
                            conn.await
                        });
                    },
                    _ = &mut receiver => {
                        drop(listener);
                        break;
                    }
                }
            }
            tokio::select! {
                _ = graceful.shutdown() => {},
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    eprintln!("graceful shutdown timed out");
                }
            }
            Ok(())
        });

        ProxyTestContext {
            sender,
            proxy_handler,
            ws_handler,
            http_back,
            port,
        }
    }
    async fn teardown(self) {
        self.sender.send(()).unwrap();
        let _ = tokio::join!(self.proxy_handler, self.ws_handler);
        self.http_back.teardown().await;
    }
}
