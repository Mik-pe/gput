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
    let mut pending = Vec::with_capacity(config.max_request_bytes.min(4_096));

    loop {
        let request = match time::timeout(
            config.read_timeout,
            read_raw_request(stream, &mut pending, config.max_request_bytes),
        )
        .await
        {
            Err(_) if pending.is_empty() => {
                stream.shutdown().await?;
                return Ok(());
            }
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
            Ok(Ok(None)) => {
                stream.shutdown().await?;
                return Ok(());
            }
            Ok(Ok(Some(request))) => request,
        };

        let close_after_response = connection_should_close(&request);
        let response = match batcher.submit(request).await {
            Ok(response) => response,
            Err(SubmitError::Overloaded) => protocol::service_unavailable_response(),
            Err(SubmitError::Stopped | SubmitError::Processing(_)) => {
                protocol::internal_server_error_response()
            }
        };

        stream.write_all(&response).await?;
        if close_after_response {
            stream.shutdown().await?;
            return Ok(());
        }
    }
}

async fn read_raw_request<R>(
    stream: &mut R,
    pending: &mut Vec<u8>,
    max_request_bytes: usize,
) -> std::result::Result<Option<Vec<u8>>, ReadRequestError>
where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0_u8; 1_024];

    loop {
        if let Some(header_len) = complete_header_len(pending) {
            if header_len > max_request_bytes {
                return Err(ReadRequestError::TooLarge);
            }

            let remainder = pending.split_off(header_len);
            let request = std::mem::replace(pending, remainder);
            return Ok(Some(request));
        }

        if pending.len() >= max_request_bytes {
            return Err(ReadRequestError::TooLarge);
        }

        let bytes_read = stream.read(&mut chunk).await?;
        if bytes_read == 0 {
            return if pending.is_empty() {
                Ok(None)
            } else {
                Err(ReadRequestError::Incomplete)
            };
        }

        pending.extend_from_slice(&chunk[..bytes_read]);
    }
}

fn complete_header_len(request: &[u8]) -> Option<usize> {
    request
        .windows(HEADER_TERMINATOR.len())
        .position(|window| window == HEADER_TERMINATOR)
        .map(|position| position + HEADER_TERMINATOR.len())
}

fn connection_should_close(request: &[u8]) -> bool {
    let Some(request_line_end) = request.windows(2).position(|window| window == b"\r\n") else {
        return true;
    };
    let request_line = &request[..request_line_end];

    if !request_line.ends_with(b" HTTP/1.1") {
        return true;
    }

    connection_header_has_token(request, b"close")
}

fn connection_header_has_token(request: &[u8], token: &[u8]) -> bool {
    for raw_line in request.split(|byte| *byte == b'\n').skip(1) {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() {
            break;
        }

        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        if !trim_ascii(&line[..colon]).eq_ignore_ascii_case(b"connection") {
            continue;
        }

        return line[colon + 1..]
            .split(|byte| *byte == b',')
            .map(trim_ascii)
            .any(|value| value.eq_ignore_ascii_case(token));
    }

    false
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
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
    async fn preserves_pipelined_bytes_for_the_next_request() {
        let first = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let second = b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut wire = first.to_vec();
        wire.extend_from_slice(second);
        let (mut writer, mut reader) = duplex(wire.len());
        let mut pending = Vec::new();

        writer
            .write_all(&wire)
            .await
            .expect("write pipelined requests");
        drop(writer);

        let first_read = read_raw_request(&mut reader, &mut pending, 4_096)
            .await
            .expect("first request is framed")
            .expect("first request exists");
        let second_read = read_raw_request(&mut reader, &mut pending, 4_096)
            .await
            .expect("second request is framed")
            .expect("second request exists");
        let eof = read_raw_request(&mut reader, &mut pending, 4_096)
            .await
            .expect("clean EOF is not an incomplete request");

        assert_eq!(first_read, first);
        assert_eq!(second_read, second);
        assert!(eof.is_none());
    }

    #[tokio::test]
    async fn rejects_an_incomplete_header_at_the_size_limit() {
        let request = b"GET / HTTP/1.1\r\nHost: unfinished";
        let (mut writer, mut reader) = duplex(request.len());
        let mut pending = Vec::new();

        writer.write_all(request).await.expect("write test request");
        drop(writer);

        assert!(matches!(
            read_raw_request(&mut reader, &mut pending, request.len()).await,
            Err(ReadRequestError::TooLarge)
        ));
    }

    #[test]
    fn http11_is_persistent_unless_the_client_closes_it() {
        assert!(!connection_should_close(
            b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
        assert!(connection_should_close(
            b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        ));
        assert!(connection_should_close(b"GET / HTTP/1.0\r\n\r\n"));
    }
}
