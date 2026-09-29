//! How many subagents are running, drawn the same way wherever it turns up:
//! the "● 3" pill on the sidebar's chat rows (beside the row's own activity
//! indicator) and the Subagents header, and the pulsing button with a count
//! badge on the titlebar's explorer toggle.

use gpui::{AnyElement, SharedString, Styled as _, div, prelude::*, px};

use crate::loaders;
use crate::theme::Theme;

/// Pill height. The sidebar's status slot is 13px and its rows 29px; 16px
/// matches the sidebar's pull-request badge so the two never disagree.
const HEIGHT: f32 = 16.0;
const DOT: f32 = 5.0;
/// Height of the count badge on the titlebar button.
const BADGE: f32 = 13.0;
/// Corner radius of the titlebar's icon buttons.
const BUTTON_RADIUS: f32 = 6.0;
/// Past this the count would outgrow the pill; it reads "99+".
const COUNT_CAP: u32 = 99;

/// The count as drawn: exact up to [`COUNT_CAP`], then "99+".
pub fn count_label(count: u32) -> SharedString {
    if count > COUNT_CAP {
        format!("{COUNT_CAP}+").into()
    } else {
        count.to_string().into()
    }
}

/// The count's digits in a box exactly `height` tall, so the line box, the
/// container and the glyphs share one centre. Digits sit a touch off the
/// centre of a text line (font ascent and descent are not symmetric about the
/// cap height), so `nudge` lifts the text by that optical remainder, and
/// `shift` moves it right for the same reason horizontally (a digit's ink is
/// a little left of the middle of its advance).
fn count_text(
    count: u32,
    size: f32,
    height: f32,
    nudge: f32,
    shift: f32,
    weight: gpui::FontWeight,
    color: gpui::Hsla,
    theme: &Theme,
) -> gpui::Div {
    div()
        .h(px(height))
        .flex_none()
        .flex()
        .items_center()
        .text_size(crate::typography::ui_rems(size))
        .line_height(px(height))
        .font_weight(weight)
        .font_family(theme.font_mono.clone())
        .text_color(color)
        .relative()
        .top(px(-nudge))
        .left(px(shift))
        .child(count_label(count))
}

/// The pill for `count` running subagents; callers draw nothing at zero.
/// `key` scopes the dot's animation state — one per placement.
pub fn running_pill(key: impl Into<SharedString>, count: u32, theme: &Theme) -> AnyElement {
    let tone = theme.busy;
    div()
        .h(px(HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .px(px(6.0))
        .rounded(px(HEIGHT / 2.0))
        .bg(tone.opacity(0.16))
        .child(loaders::pulse_dot(key, DOT, tone))
        .child(count_text(
            count,
            10.0,
            HEIGHT,
            PILL_NUDGE,
            0.0,
            gpui::FontWeight::MEDIUM,
            tone,
            theme,
        ))
        .into_any_element()
}

/// Optical lift of the digits, in px (see [`count_text`]). Measured on
/// rendered captures: without it the digits sit about half a device pixel low
/// in the pill and a full one high in the badge (which had no line height);
/// 0.25 centres both to within half a device pixel, the most a 13px box and
/// 6-7px-tall digits allow at fractional display scales.
const PILL_NUDGE: f32 = 0.25;
const BADGE_NUDGE: f32 = 0.25;
/// Rightward optical shift of the badge's digits, in px.
const BADGE_SHIFT: f32 = 0.4;

/// Marks the titlebar's explorer button while subagents run, so they can be
/// found with the panel closed: the button's face breathes in the activity
/// tone, and a small count badge sits on its corner. Both are drawn over the
/// button, so it keeps its size and position among the titlebar controls.
pub fn mark_files_button(
    button: gpui::Stateful<gpui::Div>,
    count: u32,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let tone = theme.busy;
    button
        .relative()
        .child(div().absolute().inset_0().child(loaders::pulse_glow(
            "files-button-glow",
            BUTTON_RADIUS,
            tone,
            0.10,
            0.30,
        )))
        .child(
            div()
                .absolute()
                .top(px(-3.0))
                .right(px(-3.0))
                .h(px(BADGE))
                .min_w(px(BADGE))
                .px(px(3.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(tone)
                .child(count_text(
                    count,
                    9.0,
                    BADGE,
                    BADGE_NUDGE,
                    BADGE_SHIFT,
                    gpui::FontWeight::SEMIBOLD,
                    gpui::white(),
                    theme,
                )),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_label_caps_at_ninety_nine() {
        assert_eq!(count_label(1).as_ref(), "1");
        assert_eq!(count_label(99).as_ref(), "99");
        assert_eq!(count_label(100).as_ref(), "99+");
    }
}
