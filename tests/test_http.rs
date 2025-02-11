use http_body_util::{combinators::BoxBody, BodyExt};
use hyper::header::{CONNECTION, UPGRADE};
use hyper::service::service_fn;
use hyper::{
    body::{Body, Buf, Bytes},
    Request, Response, StatusCode, Uri,
};
use hyper_reverse_proxy::ReverseProxy;
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client, ResponseFuture},
    rt::{TokioExecutor, TokioIo},
};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use test_context::test_context;
use test_context::AsyncTestContext;
use tokio::net::TcpListener;
use tokio::sync::oneshot::Sender;
use tokio::task::JoinHandle;
use tokiotest_httpserver::handler::HandlerBuilder;
use tokiotest_httpserver::{take_port, HttpTestContext};

struct ProxyTestContext {
    sender: Sender<()>,
    proxy_handler: JoinHandle<Result<(), hyper::Error>>,
    http_back: HttpTestContext,
    port: u16,
}

struct TClient<I> {
    client: Client<HttpConnector, I>,
}

impl<I> TClient<I>
where
    I: Body + Send + Sync + Unpin + Default + 'static,
    <I as Body>::Data: Buf + Send + Sync + 'static,
    <I as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    fn new() -> Self {
        Self {
            client: Client::builder(TokioExecutor::new()).build::<_, I>(HttpConnector::new()),
        }
    }

    fn request(&self, req: Request<I>) -> ResponseFuture {
        self.client.request(req)
    }

    fn get(&self, uri: Uri) -> ResponseFuture {
        self.client.get(uri)
    }
}

#[test_context(ProxyTestContext)]
#[tokio::test]
async fn test_get_error_500(ctx: &mut ProxyTestContext) {
    let client = TClient::<BoxBody<Bytes, Infallible>>::new();
    let resp = client
        .request(
            Request::builder()
                .header("keep-alive", "treu")
                .method("GET")
                .uri(ctx.uri("/500"))
                .body(BoxBody::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(500, resp.status());
}

#[test_context(ProxyTestContext)]
#[tokio::test]
async fn test_upgrade_mismatch(ctx: &mut ProxyTestContext) {
    ctx.http_back.add(
        HandlerBuilder::new("/ws")
            .status_code(StatusCode::SWITCHING_PROTOCOLS)
            .build(),
    );
    let resp = TClient::<BoxBody<Bytes, Infallible>>::new()
        .request(
            Request::builder()
                .header(CONNECTION, "Upgrade")
                .header(UPGRADE, "websocket")
                .method("GET")
                .uri(ctx.uri("/ws"))
                .body(BoxBody::default())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
}

#[test_context(ProxyTestContext)]
#[tokio::test]
async fn test_upgrade_unrequested(ctx: &mut ProxyTestContext) {
    ctx.http_back.add(
        HandlerBuilder::new("/wrong_switch")
            .status_code(StatusCode::SWITCHING_PROTOCOLS)
            .build(),
    );
    let resp = TClient::<BoxBody<Bytes, Infallible>>::new()
        .get(ctx.uri("/wrong_switch"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
}

#[test_context(ProxyTestContext)]
#[tokio::test]
async fn test_get(ctx: &mut ProxyTestContext) {
    ctx.http_back.add(
        HandlerBuilder::new("/foo")
            .status_code(StatusCode::OK)
            .build(),
    );
    let resp = TClient::<BoxBody<Bytes, Infallible>>::new()
        .get(ctx.uri("/foo"))
        .await
        .unwrap();
    assert_eq!(200, resp.status());
}

async fn handle<I>(
    client_ip: IpAddr,
    req: Request<I>,
    backend_port: u16,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible>
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
        Ok(response) => {
            let (p, b) = response.into_parts();
            let b = b.collect().await.expect("to be able to collect");
            Ok(Response::from_parts(p, BoxBody::new(b)))
        }
        Err(_) => Ok(Response::builder()
            .status(502)
            .body(BoxBody::default())
            .unwrap()),
    }
}

impl AsyncTestContext for ProxyTestContext {
    async fn setup() -> ProxyTestContext {
        let http_back = HttpTestContext::setup().await;
        let (sender, mut receiver) = tokio::sync::oneshot::channel::<()>();
        let graceful = hyper_util::server::graceful::GracefulShutdown::new();
        let bp_to_move = http_back.port;

        let port = take_port();

        let proxy_handler: JoinHandle<Result<_, hyper::Error>> = tokio::spawn(async move {
            let listener = TcpListener::bind(SocketAddr::new("127.0.0.1".parse().unwrap(), port))
                .await
                .unwrap();
            loop {
                tokio::select! {
                    conn = listener.accept() => {
                        let (stream, remote) = match conn {
                            Ok(data) => data,
                            Err(err) => {
                                eprintln!("accept error {err:?}");
                                continue;
                            }
                        };
                        let io = TokioIo::new(stream);
                        let builder =
                            hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());

                        let conn = builder.serve_connection_with_upgrades(
                            io,
                            service_fn(move |req| handle(remote.ip(), req, bp_to_move)),
                        );
                        tokio::spawn(graceful.watch(conn.into_owned()));
                    },
                    _ = &mut receiver => {
                        drop(listener);
                        break;
                    }
                }
            }
            tokio::select! {
                _ = graceful.shutdown() => {},
            }
            Ok(())
        });
        ProxyTestContext {
            sender,
            proxy_handler,
            http_back,
            port,
        }
    }
    async fn teardown(self) {
        self.http_back.teardown().await;
        self.sender.send(()).unwrap();
        let _ = tokio::join!(self.proxy_handler);
    }
}
impl ProxyTestContext {
    pub fn uri(&self, path: &str) -> Uri {
        format!("http://{}:{}{}", "localhost", self.port, path)
            .parse::<Uri>()
            .unwrap()
    }
}
