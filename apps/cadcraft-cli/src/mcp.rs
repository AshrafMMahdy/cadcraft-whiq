use super::args::{McpArgs, McpTarget};
use super::error::{CliError, Result};

pub(crate) fn mcp(a: &McpArgs) -> Result {
    let backend: Box<dyn cadcraft_mcp::Backend> = match a.target() {
        McpTarget::Connect { addr, lazy: true, launch } => Box::new(cadcraft_mcp::Remote::lazy(&addr, launch)),
        McpTarget::Connect { addr, .. } => match cadcraft_mcp::Remote::connect(&addr) {
            Ok(r) => Box::new(r),
            Err(source) => return Err(CliError::McpConnect { addr, source }),
        },
        // The headless engine the platform drives: jobs, busy, cancel, roots, recovered files.
        McpTarget::Headless => return Ok(cadcraft_mcp::whiq::serve(a.whiq.config(), std::io::BufReader::new(std::io::stdin()), std::io::stdout())?),
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    cadcraft_mcp::Server::new(backend).serve(stdin.lock(), stdout.lock())?;
    Ok(())
}
