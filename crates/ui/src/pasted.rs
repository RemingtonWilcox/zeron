//! Long pastes as attachments: the composer stages a paste past
//! [`MIN_CHARS`] as a chip instead of flooding the input, the sent prompt
//! carries each one as a trailing `<pasted-text>` block the agent reads, and
//! [`extract_badge`] turns those blocks back into a pill in the transcript.

/// Shorter pastes go into the input as typed text.
pub const MIN_CHARS: usize = 1_500;

const OPEN: &str = "\n\n<pasted-text>\n";
const CLOSE: &str = "\n</pasted-text>";
/// The words a prompt carries when the pastes are all there is.
const ONLY_TEXT: &str = "See the pasted text below.";
const PREVIEW_CHARS: usize = 280;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedText {
    pub id: String,
    pub text: String,
}

impl PastedText {
    pub fn new(text: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            text,
        }
    }

    /// The composer's chip for this paste, with its preview card.
    pub fn badge(&self) -> crate::badges::MessageBadge {
        let chars = self.text.chars().count();
        crate::badges::MessageBadge {
            icon: crate::icons::DOCUMENT,
            label: format!("Pasted text · {} chars", grouped(chars)).into(),
            details: vec![detail(0, &self.text)],
            full: vec![self.text.clone().into()],
        }
    }
}

fn detail(ix: usize, text: &str) -> crate::badges::BadgeDetail {
    crate::badges::BadgeDetail {
        location: format!("Paste {}", ix + 1).into(),
        tag: Some(format!("{} chars", grouped(text.chars().count())).into()),
        body: preview(text).into(),
    }
}

pub fn is_long(text: &str) -> bool {
    text.chars().nth(MIN_CHARS - 1).is_some()
}

/// `text` with every paste appended, in order. Folded before review comments,
/// whose block must stay last.
pub fn with_pasted(text: &str, pasted: &[PastedText]) -> String {
    if pasted.is_empty() {
        return text.to_string();
    }
    let mut out = if text.trim().is_empty() {
        ONLY_TEXT.to_string()
    } else {
        text.to_string()
    };
    for paste in pasted {
        out.push_str(OPEN);
        out.push_str(&paste.text);
        out.push_str(CLOSE);
    }
    out
}

/// [`crate::badges::Extractor`] for trailing paste blocks.
pub fn extract_badge(text: &str) -> Option<(String, crate::badges::MessageBadge)> {
    let mut rest = text;
    let mut blocks = Vec::new();
    while let Some(body) = rest.strip_suffix(CLOSE) {
        let Some(at) = body.rfind(OPEN) else {
            break;
        };
        blocks.push(&body[at + OPEN.len()..]);
        rest = &body[..at];
    }
    if blocks.is_empty() {
        return None;
    }
    blocks.reverse();
    let label = match blocks.as_slice() {
        [one] => format!("Pasted text · {} chars", grouped(one.chars().count())),
        many => format!("{} pasted texts", many.len()),
    };
    let details = blocks
        .iter()
        .enumerate()
        .map(|(ix, block)| detail(ix, block))
        .collect();
    let rest = if rest == ONLY_TEXT { "" } else { rest };
    Some((
        rest.to_string(),
        crate::badges::MessageBadge {
            icon: crate::icons::DOCUMENT,
            label: label.into(),
            details,
            full: blocks
                .iter()
                .map(|block| gpui::SharedString::from(block.to_string()))
                .collect(),
        },
    ))
}

/// The full-text viewer a paste pill opens: the image lightbox's scrim with
/// a card holding every paste, scrollable, and a Copy button. `actions` are
/// the caller's extra buttons (the composer's Edit / Remove). A click on the
/// scrim or Escape closes it.
pub fn viewer(
    window: &mut gpui::Window,
    texts: &[gpui::SharedString],
    focus: &gpui::FocusHandle,
    scroll: &gpui::ScrollHandle,
    actions: Vec<gpui::AnyElement>,
    on_close: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    use gpui::{div, prelude::*, px};
    let theme = crate::theme::Theme::of(cx).for_popup();
    let viewport = window.viewport_size();
    let width = (f32::from(viewport.width) * 0.9).min(760.0);
    let height = f32::from(viewport.height) * 0.8;
    let chars: usize = texts.iter().map(|text| text.chars().count()).sum();
    let title = match texts.len() {
        1 => format!("Pasted text · {} chars", grouped(chars)),
        n => format!("{n} pasted texts · {} chars", grouped(chars)),
    };
    let all = texts
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join("\n\n");
    let on_close = std::rc::Rc::new(on_close);
    let close_on_key = on_close.clone();
    let close_button = on_close.clone();
    let body = texts.iter().enumerate().map(|(ix, text)| {
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .when(texts.len() > 1, |part| {
                part.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(gpui::SharedString::from(format!(
                            "Paste {} · {} chars",
                            ix + 1,
                            grouped(text.chars().count())
                        ))),
                )
            })
            .child(div().text_color(theme.text).child(text.clone()))
    });
    let header = div()
        .flex_none()
        .h(px(44.0))
        .px(px(14.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .border_b_1()
        .border_color(crate::theme::hairline(0.08))
        .child(
            crate::icons::icon(crate::icons::DOCUMENT)
                .size(px(14.0))
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(gpui::SharedString::from(title)),
        )
        .child(
            crate::popover::btn_ghost(&theme, "Copy", "paste-viewer-copy")
                .id("paste-viewer-copy")
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(all.clone()));
                }),
        )
        .children(actions)
        .child(
            div()
                .id("paste-viewer-close")
                .size(px(26.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.bg(theme.ink(0.08)))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    close_button(window, cx);
                })
                .child(
                    crate::icons::icon(crate::icons::CLOSE)
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                ),
        );
    let card = crate::popover::popover_card(&theme)
        .w(px(width))
        .max_h(px(height))
        .flex()
        .flex_col()
        .overflow_hidden()
        // Clicks inside the card never reach the scrim's close.
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(header)
        .child(
            div()
                .id("paste-viewer-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .p(px(14.0))
                .flex()
                .flex_col()
                .gap(px(14.0))
                .text_size(crate::typography::ui_rems(13.0))
                .line_height(crate::typography::ui_rems(20.0))
                .children(body),
        );
    gpui::deferred(
        gpui::anchored()
            .position(gpui::point(px(0.0), px(0.0)))
            .child(
                div()
                    .id("paste-viewer")
                    .occlude()
                    .track_focus(focus)
                    .w(viewport.width)
                    .h(viewport.height)
                    .bg(crate::popover::scrim_alpha(0.6))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            cx.stop_propagation();
                            close_on_key(window, cx);
                        }
                    })
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        on_close(window, cx);
                    })
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .child(card),
            ),
    )
    .with_priority(1)
    .into_any_element()
}

fn preview(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

/// `12400` → `12,400`.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paste(text: &str) -> PastedText {
        PastedText {
            id: "p".into(),
            text: text.into(),
        }
    }

    #[test]
    fn pastes_round_trip_into_one_pill() {
        let long = "x".repeat(12_400);
        let sent = with_pasted("summarize these", &[paste(&long), paste("second\nblock")]);
        assert!(sent.starts_with("summarize these\n\n<pasted-text>\n"));
        let (rest, badge) = extract_badge(&sent).unwrap();
        assert_eq!(rest, "summarize these");
        assert_eq!(badge.label, "2 pasted texts");
        assert_eq!(badge.details.len(), 2);
        assert_eq!(badge.details[0].tag.as_deref(), Some("12,400 chars"));
        assert!(badge.details[0].body.ends_with('…'));
        assert_eq!(badge.details[1].body, "second\nblock");
    }

    #[test]
    fn a_paste_alone_is_a_message_and_shows_no_filler() {
        let sent = with_pasted("  ", &[paste("only this")]);
        let (rest, badge) = extract_badge(&sent).unwrap();
        assert_eq!(rest, "");
        assert_eq!(badge.label, "Pasted text · 9 chars");
    }

    #[test]
    fn review_comments_still_trail_the_pastes() {
        let comment = crate::comments::ReviewComment::file("src/a.rs", 3, "fix this");
        let sent = crate::comments::with_comments(&with_pasted("go", &[paste("log")]), &[comment]);
        let (badges_text, badges) = crate::badges::split(&sent);
        assert_eq!(badges_text, "go");
        assert_eq!(badges.len(), 2);
    }

    #[test]
    fn plain_messages_and_quoted_tags_are_left_alone() {
        assert!(extract_badge("no pastes here").is_none());
        assert!(extract_badge("talking about </pasted-text>").is_none());
        assert!(!is_long(&"y".repeat(MIN_CHARS - 1)));
        assert!(is_long(&"y".repeat(MIN_CHARS)));
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
