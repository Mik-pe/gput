mod cpu;
mod gpu;
mod shader_strings;

use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::{
    config::BackendChoice,
    router::{CompiledRouter, Router, builtin_router},
};

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
    create_processor_with_router(choice, limits, builtin_router())
}

pub fn create_processor_with_router(
    choice: BackendChoice,
    limits: ProcessorLimits,
    router: Router,
) -> Result<Box<dyn Processor>> {
    let router = Arc::new(router.compile()?);
    create_compiled_processor(choice, limits, router)
}

fn create_compiled_processor(
    choice: BackendChoice,
    limits: ProcessorLimits,
    router: Arc<CompiledRouter>,
) -> Result<Box<dyn Processor>> {
    match choice {
        BackendChoice::Cpu => {
            info!(
                routes = router.gpu_layout().route_count,
                "using CPU baseline processor"
            );
            Ok(Box::new(CpuProcessor::from_compiled(router)))
        }
        BackendChoice::Gpu => {
            router.validate_gpu_response_slot(limits.response_slot_bytes)?;
            Ok(Box::new(GpuProcessor::from_compiled(limits, router)?))
        }
        BackendChoice::Auto => {
            router.validate_gpu_response_slot(limits.response_slot_bytes)?;
            match GpuProcessor::from_compiled(limits, Arc::clone(&router)) {
                Ok(processor) => Ok(Box::new(processor)),
                Err(error) => {
                    warn!(
                        %error,
                        "GPU initialization failed; falling back to the CPU baseline"
                    );
                    Ok(Box::new(CpuProcessor::from_compiled(router)))
                }
            }
        }
    }
}
