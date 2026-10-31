//! WHIQ: the headless drawing engine the White Hat IQ platform drives (`cadcraft-cli mcp`): MCP
//! over stdio in the shape the platform's craft engines share (document handles, jobs, `busy`,
//! `cancel`, `server_status`, recovered files, `--automation-*-root` confinement, OLD_ copies,
//! `_meta["whiq/files"]`), with CADCraft's tools for DWG and DXF: read, search, the Preview
//! renderer, export and batched edits.

pub mod contract;
pub mod docs;
pub mod files;
pub mod preview;
pub mod read;
pub mod runtime;
pub mod takeoff;
pub mod tools;

pub use runtime::{Config, all_definitions, serve};
