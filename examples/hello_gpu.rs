use anyhow::Result;
use gput::{Body, Response, Router, config::ServerConfig};

#[tokio::main]
async fn main() -> Result<()> {
    let app = Router::new()
        .get(
            "/",
            Response::html(
                "<h1>hello from unreasonable hardware</h1>\n<p>Try <code>/inspect?owl=yes</code>.</p>\n",
            ),
        )
        .get("/plaintext", "Hello, World!\n")
        .get(
            "/inspect",
            Body::new()
                .push("path=")
                .path(128)
                .push("\nquery=")
                .query(128)
                .push("\nbackend=")
                .backend()
                .push("\npath_hash=")
                .path_hash()
                .push("\n"),
        );

    let config = ServerConfig::default();
    eprintln!(
        "gput example ready on http://{}; assembled from buffers, bit shifts, and stubbornness",
        config.bind
    );
    gput::serve(config, app).await
}
