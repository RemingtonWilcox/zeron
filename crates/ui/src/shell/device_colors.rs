//! A color per device, so sessions on different computers read apart at a
//! glance (a dot beside "project @ device"). Only shown once there are two
//! or more devices; with one there is nothing to tell apart.

use gpui::Hsla;

use crate::state::AppState;

/// Hues spaced to stay distinct in light and dark themes.
const HUES: [f32; 6] = [212.0, 32.0, 145.0, 330.0, 268.0, 182.0];

/// `device_id`'s color among the workspace's devices, or `None` with fewer
/// than two devices.
pub(crate) fn device_color(state: &AppState, device_id: &str) -> Option<Hsla> {
    let mut ids: Vec<&str> = state.devices.iter().map(|d| d.id.as_str()).collect();
    if ids.len() < 2 {
        return None;
    }
    ids.sort_unstable();
    let slot = slots(&ids).into_iter().find(|(id, _)| *id == device_id)?.1;
    Some(gpui::hsla(HUES[slot] / 360.0, 0.62, 0.58, 1.0))
}

/// Each id takes its hashed hue, or the next free one, in id order: the same
/// devices always get the same colors, and the first six never collide.
fn slots<'a>(ids: &[&'a str]) -> Vec<(&'a str, usize)> {
    let mut taken = [false; HUES.len()];
    ids.iter()
        .map(|id| {
            let preferred = fnv(id) as usize % HUES.len();
            let slot = (0..HUES.len())
                .map(|step| (preferred + step) % HUES.len())
                .find(|slot| !taken[*slot])
                .unwrap_or(preferred);
            taken[slot] = true;
            (*id, slot)
        })
        .collect()
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_keep_distinct_stable_colors() {
        let ids = ["desktop", "macbook", "mac-mini", "vps"];
        let first = slots(&ids);
        let mut used: Vec<usize> = first.iter().map(|(_, slot)| *slot).collect();
        used.sort_unstable();
        used.dedup();
        assert_eq!(used.len(), ids.len(), "no two devices share a color");
        assert_eq!(first, slots(&ids), "same devices, same colors");
    }

    #[test]
    fn one_device_gets_no_color() {
        let mut state = AppState::new();
        state.devices = serde_json::from_value(serde_json::json!([
            {"id":"desktop","name":"Desktop","platform":"windows","lastSeenAt":null}
        ]))
        .unwrap();
        assert!(device_color(&state, "desktop").is_none());
        state.devices.push(
            serde_json::from_value(serde_json::json!(
                {"id":"macbook","name":"MacBook","platform":"macos","lastSeenAt":null}
            ))
            .unwrap(),
        );
        assert!(device_color(&state, "desktop").is_some());
        assert_ne!(device_color(&state, "desktop"), device_color(&state, "macbook"));
    }
}
