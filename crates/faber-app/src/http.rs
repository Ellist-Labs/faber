// Minimal HttpClient for gpui backed by ureq — faber runs no tokio, and gpui's
// default is a NullHttpClient that fails every request. Used by gpui's image
// loader to fetch remote images (hover-doc badges). One thread per request:
// image fetches are rare and short-lived, so a pool would be overkill.

use std::sync::Arc;

use gpui::http_client::{AsyncBody, HttpClient, Request, Response, Url, http};

pub struct UreqHttpClient {
    agent: ureq::Agent,
}

impl UreqHttpClient {
    pub fn new() -> Arc<Self> {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .new_agent();
        Arc::new(Self { agent })
    }
}

impl HttpClient for UreqHttpClient {
    fn type_name(&self) -> &'static str {
        "UreqHttpClient"
    }

    fn user_agent(&self) -> Option<&http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let agent = self.agent.clone();
        let (tx, rx) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_blocking(&agent, req));
        });
        Box::pin(async move {
            rx.await
                .map_err(|_| anyhow::anyhow!("http worker thread died"))?
        })
    }
}

/// Perform the request synchronously on the worker thread. Only body-less
/// methods (GET/HEAD) are supported — that covers gpui's image loader; anything
/// else fails loudly rather than silently sending an empty body.
fn fetch_blocking(
    agent: &ureq::Agent,
    req: Request<AsyncBody>,
) -> anyhow::Result<Response<AsyncBody>> {
    let (parts, _body) = req.into_parts();
    if parts.method != http::Method::GET && parts.method != http::Method::HEAD {
        anyhow::bail!(
            "UreqHttpClient only supports GET/HEAD, got {}",
            parts.method
        );
    }

    // ureq 3 speaks `http` types natively; rebuild the request body-less.
    let mut builder = http::Request::builder()
        .method(parts.method.clone())
        .uri(parts.uri.clone());
    for (name, value) in &parts.headers {
        builder = builder.header(name, value);
    }
    let request = builder.body(())?;

    let response = agent.run(request)?;
    let (resp_parts, mut body) = response.into_parts();
    let bytes = body.with_config().limit(16 * 1024 * 1024).read_to_vec()?;

    let mut builder = http::Response::builder().status(resp_parts.status);
    for (name, value) in &resp_parts.headers {
        builder = builder.header(name, value);
    }
    Ok(builder.body(AsyncBody::from(bytes))?)
}
