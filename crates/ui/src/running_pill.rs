//! The "● 3" pill: how many subagents are running. One shape shared by the
//! sidebar's chat rows (in place of the working spinner), the titlebar's
//! explorer button, and the explorer's Subagents header, so the number reads
//! the same wherever it turns up.

use gpui::{AnyElement, SharedString, div, prelude::*, px};

use crate::loaders;
use crate::theme::Theme;

/// Pill height. The sidebar's status slot is 13px and its rows 29px; 16px
/// matches the sidebar's pull-request badge so the two never disagree.
const HEIGHT: f32 = 16.0;
const DOT: f32 = 5.0;
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
