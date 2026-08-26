//! Document Color server for CSS-style `#rgb` and `#rrggbb`.

mod example_support;

use std::sync::Arc;

use lspf::types::{
    Color, ColorInformation, ColorPresentation, ColorPresentationParams, ColorProviderOptions,
    DocumentColorParams, Position, Range,
};
use lspf::{CancellationToken, LspError, Server, ServerContext};

struct State;

fn hex_value(chars: &str) -> Option<u32> {
    let expanded = if chars.len() == 3 {
        chars.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        chars.to_string()
    };
    u32::from_str_radix(&expanded, 16).ok()
}

async fn document_colors(
    _: Arc<State>,
    ctx: ServerContext,
    params: DocumentColorParams,
    _: CancellationToken,
) -> Result<Vec<ColorInformation>, LspError> {
    let text = example_support::text(&ctx, &params.text_document.uri)?;
    let mut colors = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        for (start, _) in line.match_indices('#') {
            let available = &line[start + 1..];
            let length = if available
                .get(..6)
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
                && available
                    .as_bytes()
                    .get(6)
                    .is_none_or(|b| !b.is_ascii_alphanumeric())
            {
                6
            } else if available
                .get(..3)
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
                && available
                    .as_bytes()
                    .get(3)
                    .is_none_or(|b| !b.is_ascii_alphanumeric())
            {
                3
            } else {
                continue;
            };
            let Some(value) = hex_value(&available[..length]) else {
                continue;
            };
            colors.push(ColorInformation {
                range: Range::new(
                    Position::new(line_number as u32, start as u32),
                    Position::new(line_number as u32, (start + length + 1) as u32),
                ),
                color: Color {
                    red: ((value >> 16) & 0xff) as f32 / 255.0,
                    green: ((value >> 8) & 0xff) as f32 / 255.0,
                    blue: (value & 0xff) as f32 / 255.0,
                    alpha: 1.0,
                },
            });
        }
    }
    Ok(colors)
}

async fn color_presentation(
    _: Arc<State>,
    _: ServerContext,
    params: ColorPresentationParams,
    _: CancellationToken,
) -> Result<Vec<ColorPresentation>, LspError> {
    let red = (params.color.red * 255.0) as u32;
    let green = (params.color.green * 255.0) as u32;
    let blue = (params.color.blue * 255.0) as u32;
    Ok(vec![ColorPresentation {
        label: format!("#{:06x}", (red << 16) | (green << 8) | blue),
        text_edit: None,
        additional_text_edits: None,
    }])
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(
            lspf::features::document_color(ColorProviderOptions {}),
            document_colors,
        )
        .feature(lspf::features::color_presentation(), color_presentation)
        .build()
        .expect("color registrations are valid");
    example_support::serve(server).await
}
