//! Context occupancy is read from the replicated chat snapshot, never local CLI state.
use crate::theme::Theme;
use gpui::{IntoElement, PathBuilder, SharedString, canvas, div, point, prelude::*, px};
use zeron_proto::{ChatTokenUsage, ContextUsage};

/// The context ring's trigger chip; the footer ([`crate::account_usage`])
/// opens [`card`] from it on click.
pub fn chip(usage: Option<ContextUsage>, open: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let fraction = usage.and_then(ContextUsage::fraction);
    let color = match fraction {
        Some(f) if f >= 0.9 => theme.danger,
        Some(f) if f >= 0.75 => theme.warning,
        Some(_) => theme.text_muted,
        None => theme.text_faint,
    };
    let label = fraction
        .map(|f| format!("{:.0}%", f * 100.0))
        .unwrap_or_else(|| "—".into());
    ring_chip(
        "context-usage",
        fraction.unwrap_or(0.0) as f32,
        color,
        color,
        label,
        open,
        theme,
    )
}

/// One footer ring indicator: ring + percent, identical geometry for every
/// ring so they sit side by side as equals. `arc` colours the ring's fill,
/// `text` the label; `open` holds the hover wash while its popover is up.
pub(crate) fn ring_chip(
    id: &'static str,
    fraction: f32,
    arc: gpui::Hsla,
    text: gpui::Hsla,
    label: String,
    open: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .h(px(24.0))
        .px(px(6.0))
        .rounded(px(6.0))
        .text_size(px(11.0))
        .text_color(text)
        .cursor_pointer()
        .when(open, |s| s.bg(crate::theme::ink(0.05)))
        .hover(|s| s.bg(crate::theme::ink(0.05)))
        .child(ring(fraction, arc, theme))
        .child(SharedString::from(label))
}

/// The footer's 16px progress ring: a faint full track under a `fraction` arc
/// starting at twelve o'clock. Shared with the account usage indicator.
pub(crate) fn ring(fraction: f32, color: gpui::Hsla, theme: &Theme) -> impl IntoElement {
    let track = theme.text_faint.opacity(0.25);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let mut arc = |fraction: f32, color| {
                if fraction <= 0.0 {
                    return;
                }
                let steps = (64.0 * fraction).ceil().max(2.0) as usize;
                let mut path = PathBuilder::stroke(px(1.8));
                for i in 0..=steps {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
                    let p = point(
                        center.x + px(6.0 * angle.cos()),
                        center.y + px(6.0 * angle.sin()),
                    );
                    if i == 0 {
                        path.move_to(p);
                    } else {
                        path.line_to(p);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            arc(1.0, track);
            arc(fraction.clamp(0.0, 1.0), color);
        },
    )
    .size(px(16.0))
}

pub(crate) fn with_separators(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// whether the indicator has anything to measure against: harnesses that
/// never report a window (antigravity) get no indicator at all, rather than a
/// permanently empty ring.
pub fn has_window(usage: Option<ContextUsage>) -> bool {
    usage
        .and_then(|usage| usage.window)
        .is_some_and(|window| window > 0)
}

fn details(usage: Option<ContextUsage>) -> String {
    match usage.unwrap_or_default() {
        ContextUsage {
            tokens: Some(tokens),
            window: Some(window),
        } if window > 0 => {
            format!(
                "{} / {} tokens\n{} tokens remaining",
                with_separators(tokens),
                with_separators(window),
                with_separators(window.saturating_sub(tokens))
            )
        }
        ContextUsage {
            tokens: Some(tokens),
            ..
        } => format!(
            "{} tokens used\nContext limit not reported",
            with_separators(tokens)
        ),
        ContextUsage {
            window: Some(window),
            ..
        } if window > 0 => format!(
            "{} token capacity\nWaiting for context usage",
            with_separators(window)
        ),
        _ => "Context usage not reported by this harness yet".into(),
    }
}

/// `1,234` below ten thousand, then `12.3K`, `4.5M`, `1.2B`.
fn compact(count: u64) -> String {
    let scaled = |div: f64, unit: &str| {
        let value = count as f64 / div;
        if value < 100.0 {
            format!("{value:.1}{unit}")
        } else {
            format!("{value:.0}{unit}")
        }
    };
    match count {
        0..10_000 => with_separators(count),
        10_000..1_000_000 => scaled(1e3, "K"),
        1_000_000..1_000_000_000 => scaled(1e6, "M"),
        _ => scaled(1e9, "B"),
    }
}

/// The chat's running bill, in the providers' own terms: every provider
/// session it used, cache included.
fn chat_rows(usage: ChatTokenUsage) -> [(&'static str, String); 5] {
    [
        ("Total", compact(usage.total())),
        ("Output", compact(usage.output)),
        ("Input", compact(usage.input)),
        ("Cache read", compact(usage.cache_read)),
        ("Cache write", compact(usage.cache_write)),
    ]
}

/// Why a long chat's total dwarfs what was written: every step re-sends the
/// conversation, served from the provider's cache.
fn cache_note(usage: ChatTokenUsage) -> Option<&'static str> {
    (usage.total() > 0 && usage.cache_read >= usage.total() / 5 * 4)
        .then_some("Mostly cache reads: each step re-reads the conversation.")
}

fn usage_table(usage: ChatTokenUsage, theme: &Theme) -> gpui::Div {
    let rows = chat_rows(usage)
        .into_iter()
        .enumerate()
        .map(|(ix, (label, value))| {
            div()
                .flex()
                .flex_row()
                .justify_between()
                .gap(px(24.0))
                .text_color(if ix == 0 {
                    theme.text
                } else {
                    theme.text_muted
                })
                .child(SharedString::from(label))
                .child(SharedString::from(value))
        });
    div()
        .px(px(8.0))
        .pb(px(6.0))
        .min_w(px(200.0))
        .flex()
        .flex_col()
        .text_size(px(12.0))
        .line_height(px(19.0))
        .whitespace_nowrap()
        .children(rows)
        .when_some(cache_note(usage), |table, note| {
            table.child(
                div()
                    .pt(px(4.0))
                    .text_size(px(11.0))
                    .line_height(px(15.0))
                    .text_color(theme.text_muted.opacity(0.7))
                    .child(SharedString::from(note)),
            )
        })
}

fn section(theme: &Theme, text: String) -> gpui::Div {
    div()
        .px(px(8.0))
        .pb(px(6.0))
        .text_size(px(12.0))
        .line_height(px(19.0))
        .whitespace_nowrap()
        .text_color(theme.text_muted)
        .child(SharedString::from(text))
}

/// The context ring's popover content: the window now, then what the chat
/// has used so far when its host has counted it.
pub fn card(
    usage: Option<ContextUsage>,
    tokens: Option<ChatTokenUsage>,
    theme: &Theme,
) -> gpui::Div {
    crate::popover::popover_card(theme)
        .flex()
        .flex_col()
        .child(crate::popover::menu_heading(theme, "Context window"))
        .child(section(theme, details(usage)))
        .when_some(
            tokens.filter(|tokens| tokens.total() > 0),
            |card, tokens| {
                card.child(crate::popover::menu_heading(theme, "This chat"))
                    .child(usage_table(tokens, theme))
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_usage_reads_compactly() {
        assert_eq!(compact(9_999), "9,999");
        assert_eq!(compact(12_345), "12.3K");
        assert_eq!(compact(4_560_000), "4.6M");
        assert_eq!(compact(250_000_000), "250M");
        let usage = ChatTokenUsage {
            input: 1_200,
            output: 34_000,
            cache_read: 5_000_000,
            cache_write: 80_000,
        };
        assert_eq!(
            chat_rows(usage).map(|(label, value)| format!("{label} {value}")),
            [
                "Total 5.1M",
                "Output 34.0K",
                "Input 1,200",
                "Cache read 5.0M",
                "Cache write 80.0K"
            ]
        );
        assert!(cache_note(usage).is_some(), "5.0M of 5.1M came from cache");
        let written = ChatTokenUsage {
            output: 10,
            ..Default::default()
        };
        assert!(cache_note(written).is_none());
    }
    #[test]
    fn indicator_needs_a_reported_window() {
        assert!(!has_window(None));
        assert!(!has_window(Some(ContextUsage {
            tokens: Some(1_200),
            window: None,
        })));
        assert!(!has_window(Some(ContextUsage {
            tokens: Some(1_200),
            window: Some(0),
        })));
        assert!(has_window(Some(ContextUsage {
            tokens: None,
            window: Some(200_000),
        })));
    }

    #[test]
    fn missing_usage_is_distinct_from_zero_and_overflow() {
        assert!(details(None).contains("not reported"));
        assert!(
            details(Some(ContextUsage {
                tokens: Some(0),
                window: Some(200)
            }))
            .contains("200 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(250),
                window: Some(200)
            }))
            .contains("0 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(10),
                window: Some(0)
            }))
            .contains("limit not reported")
        );
    }

    #[test]
    fn token_counts_are_grouped_by_thousands() {
        assert_eq!(with_separators(0), "0");
        assert_eq!(with_separators(999), "999");
        assert_eq!(with_separators(5417), "5,417");
        assert_eq!(with_separators(1_048_576), "1,048,576");
        assert_eq!(
            details(Some(ContextUsage {
                tokens: Some(5417),
                window: Some(1_048_576)
            })),
            "5,417 / 1,048,576 tokens\n1,043,159 tokens remaining"
        );
    }
}
