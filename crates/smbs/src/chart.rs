//! Shared parsing for smbs.biz `FusionCharts` XML documents.

use std::fmt::Display;

use crate::{ErrorCode, SmbsError};

/// Parses a decoded chart into `(period, rate)` rows.
///
/// Requires a `<chart>` root with `caption`, only `<set>` and `<styles>`
/// children, labels that `parse_label` accepts, positive plain decimals, and
/// strictly ascending periods inside `from..=to`. Anything else is
/// `source_changed` (or `source_parse_failure` for malformed XML), never
/// partial data.
pub(crate) fn parse_rows<P: Copy + Ord + Display>(
    text: &str,
    caption: &str,
    source_url: &str,
    from: P,
    to: P,
    parse_label: impl Fn(&str) -> Option<P>,
) -> Result<Vec<(P, f64)>, SmbsError> {
    let changed =
        |message: String| SmbsError::source(ErrorCode::SourceChanged, message, source_url);
    // The JSP emits blank lines before the XML declaration, which XML forbids.
    let document = roxmltree::Document::parse(text.trim_start()).map_err(|error| {
        SmbsError::source(
            ErrorCode::SourceParseFailure,
            format!("smbs.biz returned malformed chart XML: {error}."),
            source_url,
        )
    })?;
    let chart = document.root_element();
    if chart.tag_name().name() != "chart" {
        return Err(changed(format!(
            "Expected a <chart> document, got <{}>.",
            chart.tag_name().name()
        )));
    }
    let actual_caption = chart.attribute("caption").unwrap_or_default();
    if actual_caption != caption {
        return Err(changed(format!(
            "Expected chart caption \"{caption}\", got \"{actual_caption}\"."
        )));
    }

    let mut rows: Vec<(P, f64)> = Vec::new();
    for node in chart.children().filter(roxmltree::Node::is_element) {
        // Rows must be direct <set> children; any other container (such as a
        // multi-series <dataset>) would otherwise read as an empty series.
        match node.tag_name().name() {
            "set" => {}
            "styles" => continue,
            other => return Err(changed(format!("Unexpected chart element <{other}>."))),
        }
        let label = node.attribute("label").unwrap_or_default();
        let value = node.attribute("value").unwrap_or_default();
        let period = parse_label(label)
            .ok_or_else(|| changed(format!("Unexpected row label \"{label}\".")))?;
        let rate = parse_rate(value)
            .ok_or_else(|| changed(format!("Unexpected rate \"{value}\" for {period}.")))?;
        if period < from || period > to {
            return Err(changed(format!(
                "Row {period} is outside the requested range {from}..{to}."
            )));
        }
        if rows.last().is_some_and(|(last, _)| *last >= period) {
            return Err(changed(format!(
                "Row {period} is out of order or repeated."
            )));
        }
        rows.push((period, rate));
    }
    Ok(rows)
}

/// Accepts plain positive decimals such as `1490.11`. Any decimal with at most
/// 15 significant digits round-trips exactly through `f64` formatting.
fn parse_rate(value: &str) -> Option<f64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) || whole.len() + fraction.len() > 15 {
        return None;
    }
    value.parse::<f64>().ok().filter(|rate| *rate > 0.0)
}
