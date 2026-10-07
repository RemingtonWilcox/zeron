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
//! fetch allowed and edits and shell denied. The sidebar lists those chats in
//! their own Chats section, and opening one raises the panel.

use super::*;

/// Clears the conversation column's composer below the panel.
const PANEL_BOTTOM: f32 = 104.0;
const PANEL_RIGHT: f32 = 16.0;
const MIN_WIDTH: f32 = 320.0;
const MIN_HEIGHT: f32 = 300.0;
/// The resize handles' hit band along the top and left edges.
const EDGE: f32 = 6.0;

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

/// Drag marker for the panel's resize handles.
pub(super) struct ChatPanelResize;

/// Which edges a resize drag moves. The panel is anchored bottom-right, so
/// it grows up and to the left.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edges {
    Top,
    Left,
    TopLeft,
}

/// Size and transient UI that outlive the chat shown in the panel.
pub(super) struct ChatPanelLayout {
    width: f32,
    height: f32,
    /// Pointer and size where the current resize began.
    resize: Option<(Point<f32>, (f32, f32), Edges)>,
}

impl Default for ChatPanelLayout {
    fn default() -> Self {
        Self {
            width: 400.0,
            height: 560.0,
            resize: None,
        }
    }
}

/// A resize sample: the size `anchor` grows to when the pointer moves from
/// `start` to `at`, within `[MIN, max]`.
fn resized(
    start: Point<f32>,
    from: (f32, f32),
    edges: Edges,
    at: Point<f32>,
    max: (f32, f32),
) -> (f32, f32) {
    let (mut width, mut height) = from;
    if matches!(edges, Edges::Left | Edges::TopLeft) {
        width = (from.0 + start.x - at.x).clamp(MIN_WIDTH, max.0.max(MIN_WIDTH));
    }
    if matches!(edges, Edges::Top | Edges::TopLeft) {
        height = (from.1 + start.y - at.y).clamp(MIN_HEIGHT, max.1.max(MIN_HEIGHT));
    }
    (width, height)
}

/// Whether `chat` is a general chat: it runs in the general-chat folder under
/// `data_dir`, or, for a chat hosted on another device (whose data dir is
/// unknown here), in a `general-chat` folder directly inside a Zeron data dir
/// (`…\Zeron\general-chat` on Windows, `~/.zeron/general-chat` elsewhere). Only
/// the sidebar uses this; the harness keeps its exact-path check.
pub(crate) fn is_general_chat(
    chat: &zeron_proto::Chat,
    data_dir: Option<&std::path::Path>,
) -> bool {
    let Some(cwd) = chat.cwd.as_deref() else {
        return false;
    };
    if data_dir.is_some_and(|data_dir| zeron_proto::is_general_chat_dir(cwd, data_dir)) {
        return true;
    }
    let mut parts = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']);
    parts.next() == Some(zeron_proto::GENERAL_CHAT_DIR)
        && parts
            .next()
            .is_some_and(|parent| parent.trim_start_matches('.').eq_ignore_ascii_case("zeron"))
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
            (
                state.data_dir.clone(),
                state.local_device_id.clone(),
                config,
            )
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
    pub(super) fn show_in_chat_panel(
        &mut self,
        chat: zeron_proto::Chat,
        unsaved: bool,
        cx: &mut Context<Self>,
    ) {
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
                let state = state.clone();
                move |this: &mut Self, _, event, cx| match event {
                    ComposerEvent::ContinueInSideChat(config) => {
                        this.continue_in_side_chat(&state, config.clone(), cx);
                    }
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

    /// The root's drag-move listener for [`ChatPanelResize`].
    pub(super) fn on_chat_panel_drag(
        &mut self,
        event: &gpui::DragMoveEvent<ChatPanelResize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start, from, edges)) = self.chat_panel_layout.resize else {
            return;
        };
        let at = gpui::point(
            f32::from(event.event.position.x),
            f32::from(event.event.position.y),
        );
        let viewport = window.viewport_size();
        let max = (
            f32::from(viewport.width) - 160.0,
            f32::from(viewport.height) - PANEL_BOTTOM - Theme::TITLEBAR_HEIGHT - 24.0,
        );
        let (width, height) = resized(start, from, edges, at, max);
        self.chat_panel_layout.width = width;
        self.chat_panel_layout.height = height;
        cx.notify();
    }

    fn chat_panel_resize_handle(
        &self,
        edges: Edges,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (id, cursor) = match edges {
            Edges::Top => ("chat-panel-resize-top", gpui::CursorStyle::ResizeUpDown),
            Edges::Left => ("chat-panel-resize-left", gpui::CursorStyle::ResizeLeftRight),
            Edges::TopLeft => (
                "chat-panel-resize-corner",
                gpui::CursorStyle::ResizeUpLeftDownRight,
            ),
        };
        let handle = div().id(id).absolute().cursor(cursor);
        let handle = match edges {
            Edges::Top => handle.top_0().left(px(EDGE * 2.0)).right_0().h(px(EDGE)),
            Edges::Left => handle.left_0().top(px(EDGE * 2.0)).bottom_0().w(px(EDGE)),
            Edges::TopLeft => handle.top_0().left_0().size(px(EDGE * 2.0)),
        };
        handle
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let layout = &mut this.chat_panel_layout;
                    let at = gpui::point(f32::from(event.position.x), f32::from(event.position.y));
                    layout.resize = Some((at, (layout.width, layout.height), edges));
                }),
            )
            .on_drag(ChatPanelResize, |_, _: Point<gpui::Pixels>, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DragGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.chat_panel_layout.resize = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.chat_panel_layout.resize = None),
            )
    }

    /// Open `chat_id` in the panel when it is a general chat; false otherwise.
    /// The chat already in the panel just comes back up, draft and all.
    pub(super) fn open_general_chat(&mut self, chat_id: &str, cx: &mut Context<Self>) -> bool {
        if let Some(panel) = self.chat_panel.as_mut()
            && panel.state.read(cx).selected_chat.as_deref() == Some(chat_id)
        {
            panel.open = true;
            cx.notify();
            return true;
        }
        let state = self.state.read(cx);
        let chat = state
            .chats
            .iter()
            .find(|chat| chat.id == chat_id && is_general_chat(chat, state.data_dir.as_deref()))
            .cloned();
        let Some(chat) = chat else {
            return false;
        };
        self.show_in_chat_panel(chat, false, cx);
        true
    }

    /// The chat the open panel shows.
    pub(super) fn chat_panel_chat_id(&self, cx: &App) -> Option<String> {
        let panel = self.chat_panel.as_ref().filter(|panel| panel.open)?;
        panel.state.read(cx).selected_chat.clone()
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
        let (width, height) = (self.chat_panel_layout.width, self.chat_panel_layout.height);
        composer.update(cx, |composer, cx| {
            composer.set_dock_frame(crate::composer_dock::DockFrame::settled(true), cx);
            composer.set_available_width(width - 2.0 * Theme::SPACE_SM, cx);
        });
        let title: SharedString = panel
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|chat| chat.title.clone())
            .map(|title| transcript::single_line(&title))
            .unwrap_or_else(|| "New chat".into())
            .into();
        let header = div()
            .flex_none()
            .h(px(36.0))
            .pl(px(Theme::SPACE_SM + 2.0))
            .pr(px(Theme::SPACE_XS))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.08))
            .child(
                icon(icons::CHAT_ROUND_LINE)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(px(6.0))
                    .truncate()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title),
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
                .w(px(width))
                .h(px(height))
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
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(div().size_full().child(transcript)),
                )
                .child(div().flex_none().child(composer))
                .child(self.chat_panel_resize_handle(Edges::Top, cx))
                .child(self.chat_panel_resize_handle(Edges::Left, cx))
                .child(self.chat_panel_resize_handle(Edges::TopLeft, cx))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn resizing_grows_up_and_left_within_bounds() {
        let start = gpui::point(500.0, 300.0);
        let from = (400.0, 560.0);
        let max = (900.0, 700.0);
        assert_eq!(
            resized(start, from, Edges::TopLeft, gpui::point(400.0, 250.0), max),
            (500.0, 610.0)
        );
        assert_eq!(
            resized(start, from, Edges::Top, gpui::point(400.0, 250.0), max),
            (400.0, 610.0)
        );
        assert_eq!(
            resized(start, from, Edges::Left, gpui::point(900.0, 900.0), max),
            (MIN_WIDTH, 560.0)
        );
        assert_eq!(
            resized(
                start,
                from,
                Edges::TopLeft,
                gpui::point(-2000.0, -2000.0),
                max
            ),
            max
        );
    }

    #[test]
    fn general_chats_are_told_apart_by_their_folder() {
        let chat = |cwd: Option<&str>| -> zeron_proto::Chat {
            serde_json::from_value(serde_json::json!({
                "id": "c", "deviceId": "d", "title": null, "archived": false,
                "cwd": cwd, "branch": null, "createdAt": Utc::now(),
            }))
            .unwrap()
        };
        let general = |cwd| is_general_chat(&chat(cwd), Some(Path::new("/home/me/.zeron")));
        assert!(general(Some("/home/me/.zeron/general-chat")));
        // A project folder that shares the name is a coding session.
        assert!(!general(Some("/home/me/code/general-chat")));
        assert!(!general(None));
        // Hosted on another device: recognized by its place in a Zeron data dir.
        assert!(general(Some(
            r"C:\Users\me\AppData\Local\Zeron\general-chat"
        )));
        assert!(general(Some("/Users/me/.zeron/general-chat/")));
        assert!(is_general_chat(
            &chat(Some("/home/me/.zeron/general-chat")),
            None
        ));
        assert!(!general(Some(r"C:\code\zeron-app\general-chat")));
    }

    #[test]
    fn the_folder_gets_instructions_once_and_keeps_edits() {
        let dir = tempfile::tempdir().unwrap();
        let general = ensure_general_dir(dir.path()).unwrap();
        assert!(general.join("AGENTS.md").is_file());
        std::fs::write(general.join("CLAUDE.md"), "mine").unwrap();
        ensure_general_dir(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(general.join("CLAUDE.md")).unwrap(),
            "mine"
        );
    }
}
