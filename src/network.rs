use std::{io, net::SocketAddr, sync::Arc};

use anyhow::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time,
};
use tracing::{debug, info, warn};

use crate::{
    batcher::{BatcherHandle, SubmitError},
    config::ServerConfig,
    protocol,
};

#[derive(Debug)]
enum ReadRequestError {
    Io(io::Error),
    TooLarge,
    Incomplete,
}

impl From<io::Error> for ReadRequestError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub async fn serve(config: ServerConfig, batcher: BatcherHandle) -> Result<()> {
    let listener = TcpListener::bind(config.bind).await?;
    let local_address = listener.local_addr()?;
    let connection_slots = Arc::new(Semaphore::new(config.max_connections));
    let mut shutdown = Box::pin(tokio::signal::ctrl_c());

    info!(
        bind = %local_address,
        max_connections = config.max_connections,
        "gput is listening"
    );

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (mut stream, peer_address) = accept_result?;
                let batcher = batcher.clone();
                let config = config.clone();

                match Arc::clone(&connection_slots).try_acquire_owned() {
                    Ok(permit) => {
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Err(error) = handle_connection(
                                &mut stream,
                                peer_address,
                                &config,
                                &batcher,
                            )
                            .await
                            {
                                debug!(peer = %peer_address, %error, "connection ended with an error");
                            }
                        });
                    }
                    Err(_) => {
                        tokio::spawn(async move {
                            let response = protocol::service_unavailable_response();
                            let _ = write_and_close(&mut stream, &response).await;
                        });
                    }
                }
            }
            shutdown_result = &mut shutdown => {
                shutdown_result?;
                let metrics = batcher.metrics().snapshot();
                info!(?metrics, "shutdown signal received");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection(
    stream: &mut TcpStream,
    peer_address: SocketAddr,
    config: &ServerConfig,
    batcher: &BatcherHandle,
) -> Result<()> {
    stream.set_nodelay(true)?;

    let request = match time::timeout(
        config.read_timeout,
        read_raw_request(stream, config.max_request_bytes),
    )
    .await
    {
        Err(_) => {
            warn!(peer = %peer_address, "request read timed out");
            return write_and_close(stream, &protocol::request_timeout_response()).await;
        }
        Ok(Err(ReadRequestError::TooLarge)) => {
            return write_and_close(stream, &protocol::payload_too_large_response()).await;
        }
        Ok(Err(ReadRequestError::Incomplete)) => {
            return write_and_close(stream, &protocol::incomplete_request_response()).await;
        }
        Ok(Err(ReadRequestError::Io(error))) => return Err(error.into()),
        Ok(Ok(request)) => request,
    };

    let response = match batcher.submit(request).await {
        Ok(response) => response,
        Err(SubmitError::Overloaded) => protocol::service_unavailable_response(),
        Err(SubmitError::Stopped | SubmitError::Processing(_)) => {
            protocol::internal_server_error_response()
        }
    };

    write_and_close(stream, &response).await
}

async fn read_raw_request(
    stream: &mut TcpStream,
    max_request_bytes: usize,
) -> std::result::Result<Vec<u8>, ReadRequestError> {
    let mut request = Vec::with_capacity(max_request_bytes.min(1_024));
    let mut chunk = [0_u8; 1_024];

    loop {
        let bytes_read = stream.read(&mut chunk).await?;
        if bytes_read == 0 {
            return Err(ReadRequestError::Incomplete);
        }

        request.extend_from_slice(&chunk[..bytes_read]);

        if request.len() > max_request_bytes {
            return Err(ReadRequestError::TooLarge);
        }

        if has_complete_headers(&request) {
            return Ok(request);
        }
    }
}

fn has_complete_headers(request: &[u8]) -> bool {
    request
        .windows(4)
        .any(|window| window == b"\r\n\r\n")
}

async fn write_and_close(stream: &mut TcpStream, response: &[u8]) -> Result<()> {
    stream.write_all(response).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_complete_headers_only_at_full_delimiter() {
        assert!(!has_complete_headers(b"GET / HTTP/1.1\r\nHost: x\r\n"));
        assert!(has_complete_headers(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"));
    }
}
