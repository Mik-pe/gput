use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result as AnyResult;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::processor::Processor;

struct Job {
    request: Vec<u8>,
    reply: oneshot::Sender<std::result::Result<Vec<u8>, Arc<str>>>,
}

#[derive(Debug, Clone)]
pub struct BatcherConfig {
    pub max_batch_size: usize,
    pub max_batch_wait: Duration,
    pub queue_depth: usize,
}

#[derive(Debug, Default)]
pub struct BatcherMetrics {
    accepted_requests: AtomicU64,
    rejected_requests: AtomicU64,
    completed_requests: AtomicU64,
    failed_requests: AtomicU64,
    batches: AtomicU64,
    max_batch_seen: AtomicUsize,
    total_processing_nanos: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatcherMetricsSnapshot {
    pub accepted_requests: u64,
    pub rejected_requests: u64,
    pub completed_requests: u64,
    pub failed_requests: u64,
    pub batches: u64,
    pub max_batch_seen: usize,
    pub total_processing_nanos: u64,
}

impl BatcherMetrics {
    pub fn snapshot(&self) -> BatcherMetricsSnapshot {
        BatcherMetricsSnapshot {
            accepted_requests: self.accepted_requests.load(Ordering::Relaxed),
            rejected_requests: self.rejected_requests.load(Ordering::Relaxed),
            completed_requests: self.completed_requests.load(Ordering::Relaxed),
            failed_requests: self.failed_requests.load(Ordering::Relaxed),
            batches: self.batches.load(Ordering::Relaxed),
            max_batch_seen: self.max_batch_seen.load(Ordering::Relaxed),
            total_processing_nanos: self.total_processing_nanos.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("request queue is full")]
    Overloaded,
    #[error("batch worker has stopped")]
    Stopped,
    #[error("processor failed: {0}")]
    Processing(Arc<str>),
}

#[derive(Clone)]
pub struct BatcherHandle {
    sender: Sender<Job>,
    metrics: Arc<BatcherMetrics>,
}

impl BatcherHandle {
    pub async fn submit(&self, request: Vec<u8>) -> std::result::Result<Vec<u8>, SubmitError> {
        let (reply, response) = oneshot::channel();

        match self.sender.try_send(Job { request, reply }) {
            Ok(()) => {
                self.metrics
                    .accepted_requests
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.metrics
                    .rejected_requests
                    .fetch_add(1, Ordering::Relaxed);
                return Err(SubmitError::Overloaded);
            }
            Err(TrySendError::Disconnected(_)) => return Err(SubmitError::Stopped),
        }

        response
            .await
            .map_err(|_| SubmitError::Stopped)?
            .map_err(SubmitError::Processing)
    }

    pub fn metrics(&self) -> Arc<BatcherMetrics> {
        Arc::clone(&self.metrics)
    }
}

pub fn spawn_batcher(
    processor: Box<dyn Processor>,
    config: BatcherConfig,
) -> AnyResult<(BatcherHandle, thread::JoinHandle<()>)> {
    let (sender, receiver) = bounded(config.queue_depth);
    let metrics = Arc::new(BatcherMetrics::default());
    let worker_metrics = Arc::clone(&metrics);
    let backend = processor.name();

    let worker = thread::Builder::new()
        .name(format!("gput-{backend}-batcher"))
        .spawn(move || worker_loop(processor, receiver, config, worker_metrics))?;

    Ok((BatcherHandle { sender, metrics }, worker))
}

fn worker_loop(
    mut processor: Box<dyn Processor>,
    receiver: Receiver<Job>,
    config: BatcherConfig,
    metrics: Arc<BatcherMetrics>,
) {
    let mut jobs = Vec::with_capacity(config.max_batch_size);

    while let Ok(first) = receiver.recv() {
        let collection_started = Instant::now();
        let deadline = collection_started + config.max_batch_wait;
        jobs.clear();
        jobs.push(first);

        while jobs.len() < config.max_batch_size {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            match receiver.recv_timeout(remaining) {
                Ok(job) => jobs.push(job),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }

        let batch_size = jobs.len();
        metrics.batches.fetch_add(1, Ordering::Relaxed);
        metrics
            .max_batch_seen
            .fetch_max(batch_size, Ordering::Relaxed);

        let processing_started = Instant::now();
        let result = {
            let requests = jobs
                .iter()
                .map(|job| job.request.as_slice())
                .collect::<Vec<_>>();
            processor.process_batch(&requests)
        };
        let processing_elapsed = processing_started.elapsed();
        metrics
            .total_processing_nanos
            .fetch_add(duration_as_u64_nanos(processing_elapsed), Ordering::Relaxed);

        debug!(
            backend = processor.name(),
            batch_size,
            collection_micros = collection_started.elapsed().as_micros(),
            processing_micros = processing_elapsed.as_micros(),
            "processed request batch"
        );

        match result {
            Ok(responses) if responses.len() == batch_size => {
                metrics
                    .completed_requests
                    .fetch_add(batch_size as u64, Ordering::Relaxed);

                for (job, response) in jobs.drain(..).zip(responses) {
                    let _ = job.reply.send(Ok(response));
                }
            }
            Ok(responses) => {
                let message: Arc<str> = format!(
                    "processor returned {} responses for {batch_size} requests",
                    responses.len()
                )
                .into();
                fail_batch(&mut jobs, message, &metrics);
            }
            Err(processing_error) => {
                error!(
                    backend = processor.name(),
                    error = %processing_error,
                    "processor batch failed"
                );
                fail_batch(&mut jobs, processing_error.to_string().into(), &metrics);
            }
        }
    }
}

fn fail_batch(jobs: &mut Vec<Job>, message: Arc<str>, metrics: &BatcherMetrics) {
    metrics
        .failed_requests
        .fetch_add(jobs.len() as u64, Ordering::Relaxed);

    for job in jobs.drain(..) {
        let _ = job.reply.send(Err(Arc::clone(&message)));
    }
}

fn duration_as_u64_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use anyhow::Result;

    use super::*;

    struct EchoProcessor {
        max_batch_seen: Arc<AtomicUsize>,
    }

    impl Processor for EchoProcessor {
        fn name(&self) -> &'static str {
            "echo"
        }

        fn process_batch(&mut self, requests: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
            self.max_batch_seen
                .fetch_max(requests.len(), Ordering::Relaxed);
            Ok(requests.iter().map(|request| request.to_vec()).collect())
        }
    }

    #[tokio::test]
    async fn batches_concurrent_submissions() {
        let max_batch_seen = Arc::new(AtomicUsize::new(0));
        let processor = EchoProcessor {
            max_batch_seen: Arc::clone(&max_batch_seen),
        };
        let (batcher, worker) = spawn_batcher(
            Box::new(processor),
            BatcherConfig {
                max_batch_size: 8,
                max_batch_wait: Duration::from_millis(20),
                queue_depth: 16,
            },
        )
        .expect("worker starts");

        let first = batcher.submit(b"one".to_vec());
        let second = batcher.submit(b"two".to_vec());
        let third = batcher.submit(b"three".to_vec());
        let (first, second, third) = tokio::join!(first, second, third);

        assert_eq!(first.expect("first response"), b"one");
        assert_eq!(second.expect("second response"), b"two");
        assert_eq!(third.expect("third response"), b"three");
        assert!(max_batch_seen.load(Ordering::Relaxed) >= 3);

        drop(batcher);
        worker.join().expect("worker exits cleanly");
    }
}
