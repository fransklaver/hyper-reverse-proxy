
# hyper-reverse-proxy

[![License][license-img]](LICENSE)
[![CI][ci-img]][ci-url]
[![docs][docs-img]][docs-url]
[![version][version-img]][version-url]

[license-img]: https://img.shields.io/crates/l/hyper-reverse-proxy.svg
[ci-img]: https://github.com/felipenoris/hyper-reverse-proxy/workflows/CI/badge.svg
[ci-url]: https://github.com/felipenoris/hyper-reverse-proxy/actions/workflows/main.yml
[docs-img]: https://docs.rs/hyper-reverse-proxy/badge.svg
[docs-url]: https://docs.rs/hyper-reverse-proxy
[version-img]: https://img.shields.io/crates/v/hyper-reverse-proxy.svg
[version-url]: https://crates.io/crates/hyper-reverse-proxy

A simple reverse proxy, to be used with [Hyper].

The implementation ensures that [Hop-by-hop headers] are stripped correctly in both directions,
and adds the client's IP address to a comma-space-separated list of forwarding addresses in the
`X-Forwarded-For` header.

The implementation is based on Go's [`httputil.ReverseProxy`].

[Hyper]: http://hyper.rs/
[Hop-by-hop headers]: http://www.w3.org/Protocols/rfc2616/rfc2616-sec13.html
[`httputil.ReverseProxy`]: https://golang.org/pkg/net/http/httputil/#ReverseProxy

# Example

Add these dependencies to your `Cargo.toml` file.

```toml
[dependencies]
hyper-reverse-proxy = "?"
hyper = { version = "?", features = ["full"] }
tokio = { version = "?", features = ["full"] }
lazy_static = "?"
hyper-trust-dns = { version = "?", features = [
  "rustls-http2",
  "dnssec-ring",
  "dns-over-https-rustls",
  "rustls-webpki",
  "https-only"
] }
```

The following example will set up a reverse proxy listening on `127.0.0.1:13900`,
and will proxy these calls:

* `"/target/first"` will be proxied to `http://127.0.0.1:13901`

* `"/target/second"` will be proxied to `http://127.0.0.1:13902`

* All other URLs will be handled by `debug_request` function, that will display request information.

```rust,no_run
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
```

### A word about Security

Handling outgoing requests can be a security nightmare. This crate does not control the client for the outgoing requests, as it needs to be supplied to the proxy call. The following chapters may give you an overview on how you can secure your client using the `hyper-trust-dns` crate.

> You can see them being used in the example. 

#### HTTPS

You should use a secure transport in order to know who you are talking to and so you can trust the connection. By default `hyper-trust-dns` enables the feature flag `https-only` which will panic if you supply a transport scheme which isn't `https`. It is a healthy default as it's not only you needing to trust the source but also everyone else seeing the content on unsecure connections.

> ATTENTION: if you are running on a host with added certificates in your cert store, make sure to audit them in a interval, so neither old certificates nor malicious certificates are considered as valid by your client.

#### TLS 1.2

By default `tls 1.2` is disabled in favor of `tls 1.3`, because many parts of `tls 1.2` can be considered as attach friendly. As not yet all services support it `tls 1.2` can be enabled via the `rustls-tls-12` feature.

> ATTENTION: make sure to audit the services you connect to on an interval

#### DNSSEC

As dns queries and entries aren't "trustworthy" by default from a security standpoint. `DNSSEC` adds a new cryptographic layer for verification. To enable it use the `dnssec-ring` feature.

#### HTTP/2

By default only rustlss `http1` feature is enabled for dns queries. While `http/3` might be just around the corner. `http/2` support can be enabled using the `rustls-http2` feature.

#### DoT & DoH

DoT and DoH provide you with a secure transport between you and your dns.

By default none of them are enabled. If you would like to enabled them, you can do so using the features `doh` and `dot`.

Recommendations:
 - If you need to monitor network activities in relation to accessed ports, use dot with the `dns-over-rustls` feature flag
 - If you are out in the wild and have no need to monitor based on ports, doh with the `dns-over-https-rustls` feature flag as it will blend in with other `https` traffic

It is highly recommended to use one of them.

> Currently only includes dns queries as `esni` or `ech` is still in draft by the `ietf`
