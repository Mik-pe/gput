use std::{io, net::SocketAddr, sync::Arc};

use anyhow::Result;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
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

const HEADER_TERMINATOR: &[u8; 4] = b"\r\n\r\n";

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

async fn read_raw_request<R>(
    stream: &mut R,
    max_request_bytes: usize,
) -> std::result::Result<Vec<u8>, ReadRequestError>
where
    R: AsyncRead + Unpin,
{
    let mut request = Vec::with_capacity(max_request_bytes.min(1_024));
    let mut chunk = [0_u8; 1_024];

    loop {
        let bytes_read = stream.read(&mut chunk).await?;
        if bytes_read == 0 {
            return Err(ReadRequestError::Incomplete);
        }

        request.extend_from_slice(&chunk[..bytes_read]);

        if let Some(header_len) = complete_header_len(&request) {
            if header_len > max_request_bytes {
                return Err(ReadRequestError::TooLarge);
            }

            request.truncate(header_len);
            return Ok(request);
        }

        if request.len() >= max_request_bytes {
            return Err(ReadRequestError::TooLarge);
        }
    }
}

fn complete_header_len(request: &[u8]) -> Option<usize> {
    request
        .windows(HEADER_TERMINATOR.len())
        .position(|window| window == HEADER_TERMINATOR)
        .map(|position| position + HEADER_TERMINATOR.len())
}

async fn write_and_close(stream: &mut TcpStream, response: &[u8]) -> Result<()> {
    stream.write_all(response).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    #[test]
    fn finds_only_a_complete_header_delimiter() {
        assert_eq!(complete_header_len(b"GET / HTTP/1.1\r\nHost: x\r\n"), None);

        let headers = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(complete_header_len(headers), Some(headers.len()));
    }

    #[tokio::test]
    async fn discards_body_and_pipelined_bytes_after_headers() {
        let headers = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut wire = headers.to_vec();
        wire.extend_from_slice(b"ignored bodyGET /hello HTTP/1.1\r\n\r\n");
        let (mut writer, mut reader) = duplex(wire.len());

        writer.write_all(&wire).await.expect("write test request");
        drop(writer);

        let request = read_raw_request(&mut reader, headers.len())
            .await
            .expect("header frame fits exactly");
        assert_eq!(request, headers);
    }

    #[tokio::test]
    async fn rejects_an_incomplete_header_at_the_size_limit() {
        let request = b"GET / HTTP/1.1\r\nHost: unfinished";
        let (mut writer, mut reader) = duplex(request.len());

        writer.write_all(request).await.expect("write test request");
        drop(writer);

        assert!(matches!(
            read_raw_request(&mut reader, request.len()).await,
            Err(ReadRequestError::TooLarge)
        ));
    }
}
