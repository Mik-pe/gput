use std::sync::Arc;

use anyhow::Result;

use super::Processor;
use crate::{
    builtin_router,
    router::{CompiledRouter, Router},
};

#[derive(Debug)]
pub struct CpuProcessor {
    router: Arc<CompiledRouter>,
}

impl CpuProcessor {
    pub fn with_router(router: Router) -> Result<Self> {
        Ok(Self::from_compiled(Arc::new(router.compile()?)))
    }

    pub(crate) fn from_compiled(router: Arc<CompiledRouter>) -> Self {
        Self { router }
    }
}

impl Default for CpuProcessor {
    fn default() -> Self {
        Self::with_router(builtin_router()).expect("the built-in router must compile")
    }
}

impl Processor for CpuProcessor {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn process_batch(&mut self, requests: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        Ok(requests
            .iter()
            .map(|request| self.router.route_request(request, self.name()))
            .collect())
    }
}
