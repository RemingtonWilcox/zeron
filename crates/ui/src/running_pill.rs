//! The "● 3" pill: how many subagents are running. One shape shared by the
//! sidebar's chat rows (in place of the working spinner), the titlebar's
//! explorer button, and the explorer's Subagents header, so the number reads
//! the same wherever it turns up.

use gpui::{AnyElement, SharedString, Styled as _, div, prelude::*, px};

use crate::loaders;
use crate::theme::Theme;

/// Pill height. The sidebar's status slot is 13px and its rows 29px; 16px
/// matches the sidebar's pull-request badge so the two never disagree.
const HEIGHT: f32 = 16.0;
const DOT: f32 = 5.0;
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
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .line_height(px(HEIGHT))
                .font_weight(gpui::FontWeight::MEDIUM)
                .font_family(theme.font_mono.clone())
                .text_color(tone)
                .child(count_label(count)),
        )
        .into_any_element()
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
                .min_w(px(13.0))
                .h(px(13.0))
                .px(px(3.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(tone)
                .text_size(crate::typography::ui_rems(9.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui::white())
                .child(count_label(count)),
        )
}
