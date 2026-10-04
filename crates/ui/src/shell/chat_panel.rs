//! The floating Chat panel: general, web-capable conversations kept apart
//! from coding sessions. The sidebar's Chat button raises it over the
//! conversation column, so a coding agent keeps working in view while you
//! talk about something else.
//!
//! Each chat is an ordinary project-less chat on this device, created on its
//! first message like a hand-started side chat. What makes it a *general*
//! chat is where it runs: [`zeron_proto::GENERAL_CHAT_DIR`] under the data
//! dir, whose `CLAUDE.md` / `AGENTS.md` make the agent a conversational
//! assistant; the Claude harness also launches there with web search and
//! fetch allowed and edits and shell denied. The sidebar hides those chats.

use super::*;

const PANEL_WIDTH: f32 = 400.0;
const PANEL_HEIGHT: f32 = 560.0;
/// Clears the conversation column's composer below the panel.
const PANEL_BOTTOM: f32 = 104.0;
const PANEL_RIGHT: f32 = 16.0;

const INSTRUCTIONS: &str = "\
# General chat

This is a general conversation in Zeron, not a coding task in a repository.

- Be a conversational assistant: answer directly, keep it natural, and match the length to the question.
- For anything current or factual (news, prices, releases, docs, schedules), search the web first and cite the sources you used as links.
- Do not create, edit, or delete files, and do not run shell commands, unless the user explicitly asks.
- This folder holds no project; don't explore it.
";

pub(super) struct ChatPanel {
    state: Entity<AppState>,
    transcript: Entity<Transcript>,
    composer: Entity<Composer>,
    /// Minimized panels keep their conversation and draft.
    open: bool,
    _events: Vec<Subscription>,
}

/// Whether `chat` is a general chat (runs in the general-chat folder).
pub(crate) fn is_general_chat(chat: &zeron_proto::Chat) -> bool {
    chat.cwd
        .as_deref()
        .is_some_and(zeron_proto::is_general_chat_dir)
}

/// Create the general-chat folder and its agent instructions when missing.
/// Existing files are the user's to edit and are left alone.
fn ensure_general_dir(data_dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let dir = data_dir.join(zeron_proto::GENERAL_CHAT_DIR);
    std::fs::create_dir_all(&dir)?;
    for (name, body) in [("CLAUDE.md", INSTRUCTIONS), ("AGENTS.md", INSTRUCTIONS)] {
        let path = dir.join(name);
        if !path.exists() {
            std::fs::write(path, body)?;
        }
    }
    Ok(dir)
}

impl Shell {
    /// The sidebar's Chat button: raise the panel, or minimize it.
    pub(super) fn toggle_chat_panel(&mut self, cx: &mut Context<Self>) {
        match self.chat_panel.as_mut() {
            Some(panel) => panel.open = !panel.open,
            None => self.new_general_chat(cx),
        }
        cx.notify();
    }

    /// A fresh general chat in the panel; nothing is written until its first
    /// message. Starts on the agent the current chat uses, or Claude.
    pub(super) fn new_general_chat(&mut self, cx: &mut Context<Self>) {
        let (data_dir, device, config) = {
            let state = self.state.read(cx);
            let config = state
                .selected_chat_row()
                .and_then(|chat| chat.config.clone())
                .unwrap_or(zeron_proto::ChatConfig {
                    harness: zeron_proto::HarnessId::ClaudeCode,
                    model: None,
                    reasoning: None,
                    model_options: serde_json::Map::new(),
                    sandbox: zeron_proto::SandboxLevel::ReadOnly,
                });
            (state.data_dir.clone(), state.local_device_id.clone(), config)
        };
        let (Some(data_dir), Some(device_id)) = (data_dir, device) else {
            return;
        };
        let dir = match ensure_general_dir(&data_dir) {
            Ok(dir) => dir,
            Err(err) => {
                tracing::warn!(error = %err, "general chat folder not created");
                return;
            }
        };
        let chat: zeron_proto::Chat = match serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "deviceId": device_id,
            "title": null,
            "archived": false,
            "cwd": dir.to_string_lossy(),
            "branch": null,
            "createdAt": Utc::now(),
            "config": zeron_proto::ChatConfig {
                sandbox: zeron_proto::SandboxLevel::ReadOnly,
                ..config
            },
            "roomGen": 2,
        })) {
            Ok(chat) => chat,
            Err(err) => {
                tracing::warn!(error = %err, "general chat row not built");
                return;
            }
        };
        self.show_in_chat_panel(chat, true, cx);
    }

    /// Open `chat` in the panel, replacing whatever it showed.
    fn show_in_chat_panel(&mut self, chat: zeron_proto::Chat, unsaved: bool, cx: &mut Context<Self>) {
        let chat_id = chat.id.clone();
        let parent = self.state.clone();
        let state = cx.new(|cx| AppState::side_chat_state(&parent, chat, unsaved, cx));
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let links = Self::session_links(Some(chat_id), cx);
        transcript.update(cx, |transcript, _| {
            transcript.set_workspace_link_handler(links)
        });
        let composer = cx.new(|cx| {
            let mut composer = Composer::new(state.clone(), cx);
            composer.set_side_chat(cx);
            composer
        });
        let events = vec![
            cx.subscribe(&composer, {
                let transcript = transcript.clone();
                move |_: &mut Self, _, event, cx| match event {
                    ComposerEvent::Sent {
                        chat_id,
                        message_id,
                    } => transcript.update(cx, |t, cx| {
                        t.on_own_send(chat_id.clone(), message_id.clone(), cx)
                    }),
                    ComposerEvent::Queued {
                        chat_id,
                        message_id,
                    } => transcript.update(cx, |t, cx| {
                        t.on_own_queued_send(chat_id.clone(), message_id.clone(), cx)
                    }),
                    _ => {}
                }
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        self.chat_panel = Some(ChatPanel {
            state,
            transcript,
            composer,
            open: true,
            _events: events,
        });
        cx.notify();
    }

    /// Reopen an earlier general chat in the panel.
    fn open_general_chat(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let chat = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .cloned();
        if let Some(chat) = chat {
            self.show_in_chat_panel(chat, false, cx);
        }
    }

    /// Earlier general chats, newest first.
    fn recent_general_chats(&self, cx: &App) -> Vec<(String, SharedString)> {
        let mut chats: Vec<_> = self
            .state
            .read(cx)
            .chats
            .iter()
            .filter(|chat| !chat.archived && is_general_chat(chat))
            .collect();
        chats.sort_by_key(|chat| std::cmp::Reverse(chat.last_message_at.unwrap_or(chat.created_at)));
        chats
            .into_iter()
            .take(12)
            .map(|chat| {
                let title = chat
                    .title
                    .clone()
                    .or_else(|| chat.last_message_preview.clone())
                    .unwrap_or_else(|| "New chat".into());
                (chat.id.clone(), title.into())
            })
            .collect()
    }

    /// The sidebar's Chat row, above the session list.
    pub(super) fn render_chat_button(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let active = self.chat_panel.as_ref().is_some_and(|panel| panel.open);
        div()
            .px(px(Theme::SPACE_SM))
            .pt(px(Theme::SPACE_XS))
            .child(
                div()
                    .id("sidebar-chat")
                    .h(px(29.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(Theme::SPACE_SM))
                    .rounded(px(8.0))
                    .px(px(Theme::SPACE_SM))
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(motion::hover_blend(
                        "sidebar-chat",
                        theme.text.opacity(0.8),
                        theme.text,
                    ))
                    .bg(if active {
                        theme.glass_hover()
                    } else {
                        motion::hover_blend(
                            "sidebar-chat",
                            theme.glass_hover().opacity(0.0),
                            theme.glass_hover(),
                        )
                    })
                    .on_hover(motion::hover_listener("sidebar-chat"))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_chat_panel(cx)))
                    .child(
                        icon(icons::CHAT_ROUND_LINE)
                            .size(px(16.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from("Chat")),
            )
            .into_any_element()
    }

    fn panel_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        hint: &'static str,
        theme: &Theme,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .size(px(24.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(theme.ink(0.08)))
            .role(gpui::Role::Button)
            .aria_label(hint)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                on_click(this, cx);
            }))
            .child(icon(glyph).size(px(14.0)).text_color(theme.text_muted))
    }

    /// The floating panel, when open.
    pub(super) fn render_chat_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.chat_panel.as_ref().filter(|panel| panel.open)?;
        let (transcript, composer) = (panel.transcript.clone(), panel.composer.clone());
        let theme = Theme::of(cx).for_popup();
        composer.update(cx, |composer, cx| {
            composer.set_dock_frame(crate::composer_dock::DockFrame::settled(true), cx);
            composer.set_available_width(PANEL_WIDTH - 2.0 * Theme::SPACE_SM, cx);
        });
        let recent = self.recent_general_chats(cx);
        let current = panel.state.read(cx).selected_chat.clone();
        let history = (!recent.is_empty()).then(|| {
            let rows = recent.into_iter().map(|(id, title)| {
                let selected = current.as_deref() == Some(id.as_str());
                div()
                    .id(SharedString::from(format!("chat-panel-recent-{id}")))
                    .h(px(24.0))
                    .px(px(Theme::SPACE_SM))
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(if selected { theme.text } else { theme.text_muted })
                    .when(selected, |row| row.bg(theme.ink(0.06)))
                    .hover(|style| style.bg(theme.ink(0.08)))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_general_chat(&id, cx)))
                    .child(div().min_w_0().truncate().child(title))
            });
            div()
                .flex_none()
                .max_h(px(120.0))
                .overflow_hidden()
                .px(px(Theme::SPACE_XS))
                .pb(px(Theme::SPACE_XS))
                .border_b_1()
                .border_color(crate::theme::hairline(0.08))
                .flex()
                .flex_col()
                .children(rows)
        });
        let header = div()
            .flex_none()
            .h(px(36.0))
            .px(px(Theme::SPACE_SM))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.08))
            .child(
                icon(icons::CHAT_ROUND_LINE)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .pl(px(6.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from("Chat")),
            )
            .child(self.panel_button(
                "chat-panel-new",
                icons::PLUS,
                "New chat",
                &theme,
                |this, cx| this.new_general_chat(cx),
                cx,
            ))
            .child(self.panel_button(
                "chat-panel-minimize",
                icons::WINDOW_MINIMIZE,
                "Minimize",
                &theme,
                |this, cx| {
                    if let Some(panel) = this.chat_panel.as_mut() {
                        panel.open = false;
                    }
                    cx.notify();
                },
                cx,
            ))
            .child(self.panel_button(
                "chat-panel-close",
                icons::CLOSE,
                "Close",
                &theme,
                |this, cx| {
                    this.chat_panel = None;
                    cx.notify();
                },
                cx,
            ));
        Some(
            div()
                .id("chat-panel")
                .absolute()
                .right(px(PANEL_RIGHT))
                .bottom(px(PANEL_BOTTOM))
                .w(px(PANEL_WIDTH))
                .h(px(PANEL_HEIGHT))
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.bg)
                // The panel owns its clicks and scrolls; none reach the chat
                // underneath.
                .occlude()
                .child(header)
                .children(history)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(div().size_full().child(transcript)),
                )
                .child(div().flex_none().child(composer))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_chats_are_told_apart_by_their_folder() {
        let chat = |cwd: Option<&str>| -> zeron_proto::Chat {
            serde_json::from_value(serde_json::json!({
                "id": "c", "deviceId": "d", "title": null, "archived": false,
                "cwd": cwd, "branch": null, "createdAt": Utc::now(),
            }))
            .unwrap()
        };
        assert!(is_general_chat(&chat(Some(r"C:\Users\me\AppData\Local\Zeron\general-chat"))));
        assert!(is_general_chat(&chat(Some("/home/me/.zeron/general-chat/"))));
        assert!(!is_general_chat(&chat(Some(r"C:\code\general-chat-app"))));
        assert!(!is_general_chat(&chat(None)));
    }

    #[test]
    fn the_folder_gets_instructions_once_and_keeps_edits() {
        let dir = tempfile::tempdir().unwrap();
        let general = ensure_general_dir(dir.path()).unwrap();
        assert!(general.join("AGENTS.md").is_file());
        std::fs::write(general.join("CLAUDE.md"), "mine").unwrap();
        ensure_general_dir(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(general.join("CLAUDE.md")).unwrap(), "mine");
    }
}
