use anyhow::Result;

use super::Processor;
use crate::protocol;

#[derive(Debug, Default)]
pub struct CpuProcessor;

impl Processor for CpuProcessor {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn process_batch(&mut self, requests: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        Ok(requests
            .iter()
            .map(|request| protocol::route_request(request, self.name()))
            .collect())
    }
}
