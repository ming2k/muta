pub mod client;
pub mod html;
pub mod reader;
pub mod search;
pub mod snapshot;

#[cfg(test)]
mod tests;

pub use html::html_to_text;
pub use reader::WebReaderTool;
pub use search::WebSearchTool;
pub use snapshot::{WebPageSnapshot, WebSnapshotResult};

muta_contracts::register_tool!(WebReaderFactory => |ctx| {
    ctx.get::<muta_contracts::SharedWebSearchConfig>()
        .cloned()
        .map(WebReaderTool::with_shared_config)
        .unwrap_or_else(|| {
            WebReaderTool::with_config(
                ctx.get::<muta_contracts::WebSearchConfig>()
                    .cloned()
                    .unwrap_or_default(),
            )
        })
});

muta_contracts::register_tool!(WebSearchFactory => |ctx| {
    ctx.get::<muta_contracts::SharedWebSearchConfig>()
        .cloned()
        .map(WebSearchTool::with_shared_config)
        .unwrap_or_else(|| {
            WebSearchTool::with_config(
                ctx.get::<muta_contracts::WebSearchConfig>()
                    .cloned()
                    .unwrap_or_default(),
            )
        })
});
