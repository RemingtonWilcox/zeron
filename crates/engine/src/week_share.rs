//! Each chat's share of its account's weekly limit. Providers report only the
//! account's percent, so every rise between two readings is split among the
//! chats that ran in between, by [`ChatTokenUsage::weighted`]. An estimate:
//! usage outside Zeron during a chat's run lands on that chat.

use std::collections::HashMap;

use zeron_proto::{ChatTokenUsage, HarnessId};

/// Harnesses whose accounts report a weekly window to split. Pi and others
/// may ride the same accounts, so counting them too would split one rise
/// twice.
pub(crate) const TRACKED: [HarnessId; 2] = [HarnessId::ClaudeCode, HarnessId::Codex];

/// One successful probe of a harness's live account.
#[derive(Debug, Clone, PartialEq)]
pub struct WeekReading {
    pub harness: HarnessId,
    pub account: String,
    /// The weekly window's used fraction, `0.0..=1.0`.
    pub used: f64,
}

#[derive(Default)]
pub(crate) struct WeekShares {
    last: HashMap<HarnessId, WeekReading>,
    /// chat → its harness and weighted tokens at the last reading (or when
    /// its turn started, if no reading has come since).
    ran: HashMap<String, (HarnessId, f64)>,
}

impl WeekShares {
    /// A turn started: the chat takes part in the next split, measured from
    /// `tokens`, its totals now.
    pub(crate) fn note_turn(&mut self, chat_id: &str, harness: HarnessId, tokens: ChatTokenUsage) {
        if !TRACKED.contains(&harness) {
            return;
        }
        self.ran
            .entry(chat_id.to_string())
            .or_insert((harness, tokens.weighted()));
    }

    /// The chats a reading for `harness` splits among; recount them first.
    pub(crate) fn chats(&self, harness: HarnessId) -> Vec<String> {
        self.ran
            .iter()
            .filter(|(_, (h, _))| *h == harness)
            .map(|(chat, _)| chat.clone())
            .collect()
    }

    /// Split the rise since the previous reading of the same account among
    /// the chats that ran, by the weighted tokens each used since then.
    /// `now` holds those chats' current totals; chats no longer `live` leave
    /// the pool. Returns each chat's share in millionths of the limit.
    pub(crate) fn apply(
        &mut self,
        reading: WeekReading,
        now: &HashMap<String, ChatTokenUsage>,
        live: impl Fn(&str) -> bool,
    ) -> Vec<(String, u64)> {
        let harness = reading.harness;
        let rise = match self.last.insert(harness, reading.clone()) {
            // A drop is the window resetting: what's used now is all new.
            Some(last) if last.account == reading.account => {
                if reading.used >= last.used {
                    reading.used - last.used
                } else {
                    reading.used
                }
            }
            // First reading, or another account: nothing to compare against.
            _ => 0.0,
        };
        let mut used = Vec::new();
        for (chat, (h, base)) in &self.ran {
            if *h != harness {
                continue;
            }
            let current = now.get(chat).map_or(*base, |tokens| tokens.weighted());
            used.push((chat.clone(), (current - base).max(0.0), current));
        }
        let total: f64 = used.iter().map(|(_, delta, _)| delta).sum();
        let mut shares = Vec::new();
        for (chat, delta, current) in used {
            if rise > 0.0 && total > 0.0 {
                let ppm = (rise * delta / total * 1e6).round() as u64;
                if ppm > 0 {
                    shares.push((chat.clone(), ppm));
                }
            }
            if live(&chat) {
                self.ran.insert(chat, (harness, current));
            } else {
                self.ran.remove(&chat);
            }
        }
        shares
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(output: u64) -> ChatTokenUsage {
        ChatTokenUsage {
            output,
            ..Default::default()
        }
    }

    fn reading(used: f64) -> WeekReading {
        WeekReading {
            harness: HarnessId::ClaudeCode,
            account: "a".into(),
            used,
        }
    }

    #[test]
    fn a_rise_is_split_by_tokens_used_since_the_last_reading() {
        let mut shares = WeekShares::default();
        shares.note_turn("big", HarnessId::ClaudeCode, tokens(1_000));
        shares.note_turn("small", HarnessId::ClaudeCode, tokens(0));
        shares.note_turn("codex", HarnessId::Codex, tokens(0));
        assert!(shares.apply(reading(0.10), &HashMap::new(), |_| true).is_empty());

        let now = HashMap::from([
            ("big".to_string(), tokens(1_300)),
            ("small".to_string(), tokens(100)),
        ]);
        let mut split = shares.apply(reading(0.14), &now, |chat| chat == "big");
        split.sort();
        assert_eq!(
            split,
            [("big".to_string(), 30_000), ("small".to_string(), 10_000)]
        );
        // The finished chat left the pool; the codex chat was never in it.
        assert_eq!(shares.chats(HarnessId::ClaudeCode), ["big"]);
    }

    #[test]
    fn a_reset_counts_from_zero_and_a_new_account_starts_over() {
        let mut shares = WeekShares::default();
        shares.note_turn("c", HarnessId::ClaudeCode, tokens(0));
        shares.apply(reading(0.90), &HashMap::new(), |_| true);
        let now = HashMap::from([("c".to_string(), tokens(10))]);
        assert_eq!(
            shares.apply(reading(0.02), &now, |_| true),
            [("c".to_string(), 20_000)]
        );
        let other = WeekReading {
            account: "b".into(),
            ..reading(0.50)
        };
        let now = HashMap::from([("c".to_string(), tokens(20))]);
        assert!(shares.apply(other, &now, |_| true).is_empty());
    }
}
