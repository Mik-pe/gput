mod cpu;
mod gpu;

use anyhow::Result;
use tracing::{info, warn};

use crate::config::BackendChoice;

pub use cpu::CpuProcessor;
pub use gpu::GpuProcessor;

pub trait Processor: Send + 'static {
    fn name(&self) -> &'static str;
    fn process_batch(&mut self, requests: &[&[u8]]) -> Result<Vec<Vec<u8>>>;
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessorLimits {
    pub max_batch_size: usize,
    pub max_request_bytes: usize,
    pub response_slot_bytes: usize,
}

pub fn create_processor(
    choice: BackendChoice,
    limits: ProcessorLimits,
) -> Result<Box<dyn Processor>> {
    match choice {
        BackendChoice::Cpu => {
            info!("using CPU baseline processor");
            Ok(Box::new(CpuProcessor))
        }
        BackendChoice::Gpu => Ok(Box::new(GpuProcessor::new(limits)?)),
        BackendChoice::Auto => match GpuProcessor::new(limits) {
            Ok(processor) => Ok(Box::new(processor)),
            Err(error) => {
                warn!(
                    %error,
                    "GPU initialization failed; falling back to the CPU baseline"
                );
                Ok(Box::new(CpuProcessor))
            }
        },
    }
}
