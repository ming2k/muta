pub mod client;
pub mod fetch;
pub mod html;
pub mod search;
pub mod snapshot;

#[cfg(test)]
mod tests;

pub use fetch::WebFetchTool;
pub use html::html_to_text;
pub use search::WebSearchTool;
pub use snapshot::{WebPageSnapshot, WebSnapshotResult};

muta_contracts::register_tool!(WebFetchFactory => |ctx| {
    ctx.get::<muta_contracts::SharedWebSearchConfig>()
        .cloned()
        .map(WebFetchTool::with_shared_config)
        .unwrap_or_else(|| {
            WebFetchTool::with_config(
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
