#![forbid(unsafe_code)]

pub mod batcher;
pub mod config;
pub mod network;
pub mod packet;
pub mod processor;
pub mod protocol;
pub mod router;
mod server;

pub use router::{Router, builtin_router, response, routing};
pub use server::serve;
