use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_reverse_proxy::ReverseProxy;
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::{TokioExecutor, TokioIo},
};
use std::net::IpAddr;
use std::{convert::Infallible, net::SocketAddr};
use tokio::net::TcpListener;

fn debug_request(req: &Request<Incoming>) -> Response<BoxBody<Bytes, Infallible>> {
    let body_str = format!("{:?}", req);
    Response::new(BoxBody::new(Full::new(Bytes::from(body_str))))
}

async fn handle(
    client_ip: IpAddr,
    req: Request<Incoming>,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    let proxy_client =
        ReverseProxy::new(Client::builder(TokioExecutor::new()).build(HttpConnector::new()));

    let url = if req.uri().path().starts_with("/target/first") {
        "http://127.0.0.1:13901"
    } else if req.uri().path().starts_with("/target/second") {
        "http://127.0.0.1:13902"
    } else {
        return Ok(debug_request(&req));
    };

    match proxy_client.call(client_ip, url, req).await {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            let body = body.collect().await.expect("to be able collect the body");
            Ok(Response::from_parts(parts, BoxBody::new(body)))
        }
        Err(_error) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(BoxBody::default())
            .unwrap()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bind_addr = "127.0.0.1:8000";
    let addr: SocketAddr = bind_addr.parse().expect("Could not parse ip:port.");

    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, remote) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            let remote_addr = remote.ip();
            let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            if let Err(err) = builder
                .serve_connection_with_upgrades(io, service_fn(move |req| handle(remote_addr, req)))
                .await
            {
                eprintln!("Failed to serve connection {err:?}");
            }
        });
    }
}
