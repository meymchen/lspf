//! Date-time Hover server without an additional date-time dependency.

mod example_support;

use std::sync::Arc;

use lspf::types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind, Position, Range};
use lspf::{CancellationToken, LspError, Server, ServerContext};

struct State;

struct DateTime {
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

fn numbers<const N: usize>(value: &str, separator: char) -> Option<[u32; N]> {
    let values = value
        .split(separator)
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    values.try_into().ok()
}

fn valid(value: DateTime) -> Option<DateTime> {
    let leap = value.year.is_multiple_of(4)
        && (!value.year.is_multiple_of(100) || value.year.is_multiple_of(400));
    let days = match value.month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    (value.day >= 1
        && value.day <= days
        && value.hour < 24
        && value.minute < 60
        && value.second < 60)
        .then_some(value)
}

fn parse(value: &str) -> Option<DateTime> {
    let value = value.trim();
    if let Some((date, time)) = value.split_once("T") {
        let [year, month, day] = numbers(date, '-')?;
        let [hour, minute, second] = numbers(time, ':')?;
        return valid(DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        });
    }
    if value.contains(':') {
        let [hour, minute, second] = numbers(value, ':')?;
        return valid(DateTime {
            year: 1900,
            month: 1,
            day: 1,
            hour,
            minute,
            second,
        });
    }
    if value.contains('/') {
        let [day, month, short_year] = numbers(value, '/')?;
        let year = if short_year <= 68 {
            2000 + short_year
        } else {
            1900 + short_year
        };
        return valid(DateTime {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        });
    }
    let [year, month, day] = numbers(value, '-')?;
    valid(DateTime {
        year,
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
    })
}

fn markdown(value: &DateTime) -> String {
    format!(
        "# {:02}/{:02}/{:04}\n\n| Format | Value |\n|:-|-:|\n\
         | `%H:%M:%S` | {:02}:{:02}:{:02} |\n\
         | `%d/%m/%y` | {:02}/{:02}/{:02} |\n\
         | `%Y-%m-%d` | {:04}-{:02}-{:02} |\n\
         | `%Y-%m-%dT%H:%M:%S` | {:04}-{:02}-{:02}T{:02}:{:02}:{:02} |",
        value.day,
        value.month,
        value.year,
        value.hour,
        value.minute,
        value.second,
        value.day,
        value.month,
        value.year % 100,
        value.year,
        value.month,
        value.day,
        value.year,
        value.month,
        value.day,
        value.hour,
        value.minute,
        value.second,
    )
}

async fn hover(
    _: Arc<State>,
    ctx: ServerContext,
    params: HoverParams,
    _: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let position = params.text_document_position_params.position;
    let uri = params.text_document_position_params.text_document.uri;
    let text = example_support::text(&ctx, &uri)?;
    let Some(line) = text.lines().nth(position.line as usize) else {
        return Ok(None);
    };
    let Some(value) = parse(line) else {
        return Ok(None);
    };
    Ok(Some(Hover {
        contents: HoverContents::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown(&value),
        }),
        range: Some(Range::new(
            Position::new(position.line, 0),
            Position::new(position.line, line.len() as u32),
        )),
    }))
}

#[tokio::main]
async fn main() -> lspf::Result<()> {
    let server = Server::builder(State)
        .feature(lspf::features::hover(), hover)
        .build()
        .expect("hover registration is valid");
    example_support::serve(server).await
}
