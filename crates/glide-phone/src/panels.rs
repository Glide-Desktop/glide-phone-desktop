//! Window sections for the features beyond the device cards.

use std::collections::BTreeMap;

use glide_sdk::iced::widget::{column, row};
use glide_sdk::prelude::*;
use glide_sdk::widget::{Icon, Tone};

use crate::plugins::runcommand::Command;
use crate::ui::{Message, global_manager, proxy};

pub type PhoneMedia = std::collections::HashMap<String, BTreeMap<String, crate::plugins::remote_media::RemotePlayer>>;
pub type PhoneVolumes = std::collections::HashMap<String, Vec<crate::plugins::phone_volume::PhoneSink>>;

/// State of the feature sections.
#[derive(Debug, Default)]
pub struct Panels {
    pub commands: Vec<CommandRow>,
    pub phone_media: PhoneMedia,
    pub phone_volumes: PhoneVolumes,
    pub device_names: std::collections::HashMap<String, String>,
    /// Paired phones that are connected now.
    pub reachable: Vec<String>,
    pad_down: bool,
    pad_last: Option<glide_sdk::iced::Point>,
    pad_travel: f32,
    pub typed: String,
    pub share_draft: String,
    pub sms: std::collections::HashMap<String, crate::plugins::sms::Inbox>,
    pub keyboard_off: std::collections::HashMap<String, bool>,
    /// Contact names of the shown phone, by number key (refreshed with the state).
    pub names: std::collections::HashMap<String, String>,
    /// Phones whose conversations were already asked for in this window.
    sms_asked: std::collections::HashSet<String>,
    pub open_thread: Option<i64>,
    pub sms_draft: String,
    pub draft_name: String,
    pub draft_command: String,
    /// Where the cursor goes over to the phone ("" = off); read from the daemon once.
    pub share_edge: Option<String>,
    /// Whether phones may browse this computer's files; read from the daemon once.
    pub files_allowed: Option<bool>,
    /// Whether new screenshots go to the phones; read from the daemon once.
    pub send_screenshots: Option<bool>,
}

#[derive(Debug, Clone)]
pub enum PanelMessage {
    CommandName(String),
    CommandLine(String),
    AddCommand,
    RemoveCommand(String),
    /// (device, player, action)
    PhoneMedia(String, String, &'static str),
    PadPress,
    PadRelease,
    PadRight,
    PadMove(glide_sdk::iced::Point),
    PadScroll(glide_sdk::iced::mouse::ScrollDelta),
    Typed(String),
    SendTyped,
    Special(u64),
    Ping,
    ShareDraft(String),
    Share,
    /// Read this computer's clipboard (the window has focus) and send it to the phone.
    SendClipboard,
    ClipboardRead(Option<String>),
    OpenThread(i64),
    CloseThread,
    SmsDraft(String),
    SendSms,
    /// Off → left → right → top → bottom → off.
    CycleShareEdge,
    /// Let phones browse this computer's home folder, or stop it.
    ToggleComputerFiles,
    /// Send new screenshots to the phones, or stop.
    ToggleScreenshots,
    /// (device, stream, volume 0..=max) — the phone's own volume (Glide only).
    PhoneVolume(String, String, f32),
    PhoneMute(String, String, bool),
}

impl Panels {
    /// The "Control the phone" section's messages; anything else comes back as `Err`.
    fn update_control(&mut self, message: PanelMessage) -> Result<Task<Message>, PanelMessage> {
        match message {
            PanelMessage::PadPress => {
                self.pad_down = true;
                self.pad_travel = 0.0;
            }
            PanelMessage::PadRelease => {
                // A press without movement is a tap: a click on the phone.
                if self.pad_down && self.pad_travel < 4.0 {
                    self.input(serde_json::json!({ "singleclick": true }));
                }
                self.pad_down = false;
            }
            PanelMessage::PadRight => self.input(serde_json::json!({ "rightclick": true })),
            PanelMessage::PadMove(point) => {
                if let Some(last) = self.pad_last.replace(point)
                    && self.pad_down
                {
                    let (dx, dy) = (point.x - last.x, point.y - last.y);
                    self.pad_travel += dx.abs() + dy.abs();
                    self.input(serde_json::json!({ "dx": dx, "dy": dy }));
                }
            }
            PanelMessage::PadScroll(delta) => {
                let (dx, dy) = match delta {
                    glide_sdk::iced::mouse::ScrollDelta::Lines { x, y } => (x * 20.0, y * 20.0),
                    glide_sdk::iced::mouse::ScrollDelta::Pixels { x, y } => (x, y),
                };
                self.input(serde_json::json!({ "scroll": true, "dx": dx, "dy": dy }));
            }
            PanelMessage::Typed(text) => self.typed = text,
            PanelMessage::SendTyped => {
                let text = std::mem::take(&mut self.typed);
                if !text.is_empty() {
                    self.input(serde_json::json!({ "key": text }));
                }
            }
            PanelMessage::Special(key) => self.input(serde_json::json!({ "specialKey": key })),
            PanelMessage::Ping => {
                if let Some(device) = self.reachable.first() {
                    if let Some(proxy) = proxy() {
                        let _ = proxy.ping(device);
                    } else if let Some(manager) = global_manager()
                        && let Ok(guard) = manager.try_lock()
                    {
                        let _ = guard.ping(device);
                    }
                }
            }
            PanelMessage::ShareDraft(text) => self.share_draft = text,
            PanelMessage::Share => {
                let text = std::mem::take(&mut self.share_draft);
                if let Some(device) = self.reachable.first().cloned()
                    && !text.trim().is_empty()
                {
                    share_to_phone(&device, text.trim());
                }
            }
            PanelMessage::SendClipboard => {
                return Ok(
                    glide_sdk::iced::clipboard::read().map(|text| Message::Panel(PanelMessage::ClipboardRead(text)))
                );
            }
            PanelMessage::CycleShareEdge => self.cycle_share_edge(),
            PanelMessage::ToggleComputerFiles => self.toggle_computer_files(),
            PanelMessage::ToggleScreenshots => self.toggle_screenshots(),
            PanelMessage::ClipboardRead(text) => {
                if let (Some(text), Some(device)) = (text.filter(|t| !t.is_empty()), self.reachable.first()) {
                    if let Some(proxy) = proxy() {
                        let _ = proxy.set_clipboard(device, &text);
                    } else if let Some(manager) = global_manager()
                        && let Ok(mut guard) = manager.try_lock()
                    {
                        let _ = guard.set_clipboard(device, &text);
                    }
                }
            }
            other => return Err(other),
        }
        Ok(Task::none())
    }

    fn cycle_share_edge(&mut self) {
        let current = self.share_edge.clone().unwrap_or_default();
        let order = ["", "left", "right", "top", "bottom"];
        let next = order[(order.iter().position(|e| *e == current).unwrap_or(0) + 1) % order.len()];
        let ok = if let Some(proxy) = proxy() {
            proxy.set_share_input_edge(next).unwrap_or(false)
        } else if let Some(manager) = global_manager()
            && let Ok(mut guard) = manager.try_lock()
        {
            guard.set_share_input_edge(crate::edge::Edge::parse(next))
        } else {
            false
        };
        if ok {
            self.share_edge = Some(next.to_string());
        }
    }

    fn toggle_screenshots(&mut self) {
        let next = !self.send_screenshots.unwrap_or(false);
        let ok = if let Some(proxy) = proxy() {
            proxy.set_send_screenshots(next).unwrap_or(false)
        } else if let Some(manager) = global_manager()
            && let Ok(mut guard) = manager.try_lock()
        {
            guard.set_send_screenshots(next);
            true
        } else {
            false
        };
        if ok {
            self.send_screenshots = Some(next);
        }
    }

    fn toggle_computer_files(&mut self) {
        let next = !self.files_allowed.unwrap_or(false);
        let ok = if let Some(proxy) = proxy() {
            proxy.set_computer_files_allowed(next).unwrap_or(false)
        } else if let Some(manager) = global_manager()
            && let Ok(mut guard) = manager.try_lock()
        {
            guard.set_computer_files_allowed(next);
            true
        } else {
            false
        };
        if ok {
            self.files_allowed = Some(next);
        }
    }

    pub fn update(&mut self, message: PanelMessage) -> Task<Message> {
        let message = match self.update_control(message) {
            Ok(task) => return task,
            Err(other) => other,
        };
        match message {
            PanelMessage::CommandName(name) => self.draft_name = name,
            PanelMessage::CommandLine(line) => self.draft_command = line,
            PanelMessage::AddCommand => {
                add_command(&std::mem::take(&mut self.draft_name), &std::mem::take(&mut self.draft_command));
                return Task::done(Message::Refresh);
            }
            PanelMessage::RemoveCommand(key) => {
                remove_command(&key);
                return Task::done(Message::Refresh);
            }
            PanelMessage::OpenThread(thread) => {
                self.open_thread = Some(thread);
                if let Some(device) = self.reachable.first() {
                    if let Some(proxy) = proxy() {
                        let _ = proxy.request_sms_thread(device, thread);
                    } else if let Some(manager) = global_manager()
                        && let Ok(guard) = manager.try_lock()
                    {
                        let _ = guard.request_sms_thread(device, thread);
                    }
                }
            }
            PanelMessage::CloseThread => self.open_thread = None,
            PanelMessage::SmsDraft(text) => self.sms_draft = text,
            PanelMessage::SendSms => self.send_sms(),
            PanelMessage::PhoneVolume(device, name, value) => {
                #[allow(clippy::cast_possible_truncation)]
                let volume = value.round() as i64;
                let sink = self.phone_volumes.get_mut(&device).and_then(|s| s.iter_mut().find(|s| s.name == name));
                // Only whole steps go to the phone, not every pixel of a drag.
                if let Some(sink) = sink
                    && sink.volume != volume
                {
                    sink.volume = volume;
                    if let Some(proxy) = proxy() {
                        let _ = proxy.set_phone_volume(&device, &name, i32::try_from(volume).unwrap_or(0));
                    } else if let Some(manager) = global_manager()
                        && let Ok(mut guard) = manager.try_lock()
                    {
                        let _ = guard.set_phone_volume(&device, &name, volume);
                    }
                }
            }
            PanelMessage::PhoneMute(device, name, muted) => {
                if let Some(sink) =
                    self.phone_volumes.get_mut(&device).and_then(|s| s.iter_mut().find(|s| s.name == name))
                {
                    sink.muted = muted;
                }
                if let Some(proxy) = proxy() {
                    let _ = proxy.set_phone_muted(&device, &name, muted);
                } else if let Some(manager) = global_manager()
                    && let Ok(mut guard) = manager.try_lock()
                {
                    let _ = guard.set_phone_muted(&device, &name, muted);
                }
            }
            PanelMessage::PhoneMedia(device, player, action) => {
                if let Some(proxy) = proxy() {
                    let _ = proxy.phone_media_action(&device, &player, action);
                } else if let Some(manager) = global_manager()
                    && let Ok(guard) = manager.try_lock()
                {
                    let _ = guard.control_phone_media(&device, &player, serde_json::json!({ "action": action }));
                }
            }
            // Handled by update_control above.
            _ => {}
        }
        Task::none()
    }

    #[must_use]
    pub fn view<'a>(&'a self, shell: &'a Shell<Message>) -> Vec<Element<'a, Message>> {
        let mut sections = Vec::new();
        if self.phone_volumes.values().any(|sinks| !sinks.is_empty()) {
            sections.push(phone_volume_group(self, shell));
        }
        if self.phone_media.values().any(|players| !players.is_empty()) {
            sections.push(phone_media_group(self, shell));
        }
        if !self.reachable.is_empty() {
            if self.inbox().is_some_and(|inbox| !inbox.threads.is_empty()) {
                sections.push(messages_group(self, shell));
            }
            sections.push(control_group(self, shell));
        }
        sections.push(commands_group(self, shell));
        sections
    }
}

impl Panels {
    /// After each refresh: load names and, once per phone, ask for its conversations.
    pub fn refreshed(&mut self) {
        if self.send_screenshots.is_none() {
            self.send_screenshots = Some(if let Some(proxy) = proxy() {
                proxy.send_screenshots().unwrap_or(false)
            } else {
                global_manager().and_then(|m| m.try_lock().ok()).is_some_and(|g| g.send_screenshots())
            });
        }
        if self.files_allowed.is_none() {
            self.files_allowed = Some(if let Some(proxy) = proxy() {
                proxy.computer_files_allowed().unwrap_or(false)
            } else {
                global_manager().and_then(|m| m.try_lock().ok()).is_some_and(|g| g.computer_files_allowed())
            });
        }
        if self.share_edge.is_none() {
            self.share_edge = Some(if let Some(proxy) = proxy() {
                proxy.share_input_edge().unwrap_or_default()
            } else {
                global_manager()
                    .and_then(|m| m.try_lock().ok())
                    .and_then(|g| g.share_input_edge())
                    .map_or_else(String::new, |e| e.name().to_string())
            });
        }
        let Some(device) = self.reachable.first().cloned() else { return };
        self.names = crate::plugins::contacts::names(&crate::plugins::contacts::book_dir(&device));
        if self.sms_asked.insert(device.clone()) {
            if let Some(proxy) = proxy() {
                let _ = proxy.request_sms_conversations(&device);
            } else if let Some(manager) = global_manager()
                && let Ok(guard) = manager.try_lock()
            {
                let _ = guard.request_sms_conversations(&device);
            }
        }
    }

    fn inbox(&self) -> Option<&crate::plugins::sms::Inbox> {
        self.sms.get(self.reachable.first()?)
    }

    fn send_sms(&mut self) {
        let text = std::mem::take(&mut self.sms_draft);
        let (Some(device), Some(thread)) = (self.reachable.first().cloned(), self.open_thread) else { return };
        let Some(addresses) =
            self.inbox().and_then(|i| i.threads.get(&thread)).and_then(|t| t.last()).map(|m| m.addresses.clone())
        else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        if let Some(proxy) = proxy() {
            let _ = proxy.send_sms(&device, addresses, &text);
        } else if let Some(manager) = global_manager()
            && let Ok(guard) = manager.try_lock()
        {
            let _ = guard.send_sms(&device, &addresses, &text);
        }
    }

    /// Sends a mousepad request to the first reachable phone.
    fn input(&self, body: serde_json::Value) {
        let Some(device) = self.reachable.first() else { return };
        if let Some(proxy) = proxy() {
            let _ = proxy.control_phone_input(device, &body.to_string());
        } else if let Some(manager) = global_manager()
            && let Ok(guard) = manager.try_lock()
        {
            let _ = guard.control_phone_input(device, body);
        }
    }
}

/// A link opens in the phone's browser; other text arrives as shared text.
fn share_to_phone(device: &str, text: &str) {
    let is_url = (text.starts_with("http://") || text.starts_with("https://")) && !text.contains(char::is_whitespace);
    if let Some(proxy) = proxy() {
        let _ = if is_url { proxy.share_url(device, text) } else { proxy.share_text(device, text) };
    } else if let Some(manager) = global_manager()
        && let Ok(guard) = manager.try_lock()
    {
        let _ = if is_url { guard.share_url(device, text) } else { guard.share_text(device, text) };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRow {
    pub key: String,
    pub name: String,
    pub command: String,
}

#[must_use]
pub fn command_rows(entries: &BTreeMap<String, Command>) -> Vec<CommandRow> {
    let mut rows: Vec<CommandRow> = entries
        .iter()
        .map(|(key, c)| CommandRow { key: key.clone(), name: c.name.clone(), command: c.command.clone() })
        .collect();
    rows.sort_by_key(|row| row.name.to_lowercase());
    rows
}

#[must_use]
pub fn command_rows_from_json(json: &str) -> Vec<CommandRow> {
    serde_json::from_str::<BTreeMap<String, Command>>(json).map(|entries| command_rows(&entries)).unwrap_or_default()
}

pub fn add_command(name: &str, line: &str) {
    if let Some(proxy) = proxy() {
        let _ = proxy.add_command(name, line);
    } else if let Some(manager) = global_manager()
        && let Ok(mut guard) = manager.try_lock()
    {
        let _ = guard.add_command(name, line);
    }
}

pub fn remove_command(key: &str) {
    if let Some(proxy) = proxy() {
        let _ = proxy.remove_command(key);
    } else if let Some(manager) = global_manager()
        && let Ok(mut guard) = manager.try_lock()
    {
        guard.remove_command(key);
    }
}

/// Commands the phone can run on this computer (KDE Connect's Run command settings).
fn commands_group<'a>(panels: &'a Panels, shell: &'a Shell<Message>) -> Element<'a, Message> {
    let paint = shell.paint();
    let mut group = widget::Group::new(paint)
        .title(shell.t("Команди для телефону", "Commands for the phone"))
        .description(shell.t(
            "Телефон може запускати ці команди на комп’ютері й бачити їхній вивід",
            "The phone can run these commands on this computer and see their output",
        ));
    for item in &panels.commands {
        group = group.push(widget::ListRow::new(paint, &item.name).subtitle(item.command.as_str()).suffix(
            widget::icon_button(
                paint,
                Icon::Close,
                shell.t("Видалити команду", "Remove command"),
                Some(Message::Panel(PanelMessage::RemoveCommand(item.key.clone()))),
                false,
            ),
        ));
    }
    let ready = !panels.draft_name.trim().is_empty() && !panels.draft_command.trim().is_empty();
    let editor = column![
        widget::text_field(paint, shell.t("Назва", "Name"), &panels.draft_name, |v| Message::Panel(
            PanelMessage::CommandName(v)
        )),
        widget::text_field(
            paint,
            shell.t("Команда, напр. systemctl suspend", "Command, e.g. systemctl suspend"),
            &panels.draft_command,
            |v| Message::Panel(PanelMessage::CommandLine(v))
        ),
        row![widget::compact_button(
            paint,
            shell.t("Додати", "Add"),
            Tone::Tonal,
            ready.then_some(Message::Panel(PanelMessage::AddCommand))
        )],
    ]
    .spacing(8);
    group.push(editor).into()
}

/// Players running on the phone, with their controls (KDE Connect's media remote).
/// The phone's own volumes, set from here (Glide only).
fn phone_volume_group<'a>(panels: &'a Panels, shell: &'a Shell<Message>) -> Element<'a, Message> {
    let paint = shell.paint();
    let mut group = widget::Group::new(paint).title(shell.t("Гучність телефону", "Phone volume"));
    for (device, sinks) in &panels.phone_volumes {
        for sink in sinks {
            #[allow(clippy::cast_precision_loss)]
            let (value, max) = (sink.volume as f32, sink.max_volume as f32);
            let (dev, name) = (device.clone(), sink.name.clone());
            let slider = widget::slider(paint, 0.0..=max, value, move |v| {
                Message::Panel(PanelMessage::PhoneVolume(dev.clone(), name.clone(), v))
            });
            let mute = widget::compact_button(
                paint,
                if sink.muted { "🔇" } else { "🔊" },
                Tone::Neutral,
                Some(Message::Panel(PanelMessage::PhoneMute(device.clone(), sink.name.clone(), !sink.muted))),
            );
            let label = if sink.description.is_empty() { sink.name.clone() } else { sink.description.clone() };
            group = group.push(
                widget::ListRow::new(paint, label)
                    .subtitle(format!("{}%", sink.volume * 100 / sink.max_volume.max(1)))
                    .suffix(mute),
            );
            group = group.push(slider);
        }
    }
    group.into()
}

fn phone_media_group<'a>(panels: &'a Panels, shell: &'a Shell<Message>) -> Element<'a, Message> {
    let paint = shell.paint();
    let mut group = widget::Group::new(paint).title(shell.t("Медіа на телефоні", "Media on the phone"));
    for (device, players) in &panels.phone_media {
        for player in players.values() {
            let title = if player.title.is_empty() { player.player.clone() } else { player.title.clone() };
            let mut subtitle = [player.artist.as_str(), player.player.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            if let Some(name) = panels.device_names.get(device) {
                subtitle = format!("{subtitle} — {name}");
            }
            let button = |label: &'a str, action: &'static str, enabled: bool| {
                widget::compact_button(
                    paint,
                    label,
                    Tone::Neutral,
                    enabled.then(|| {
                        Message::Panel(PanelMessage::PhoneMedia(device.clone(), player.player.clone(), action))
                    }),
                )
            };
            let controls = row![
                button("⏮", "Previous", player.can_go_previous),
                button(if player.is_playing { "⏸" } else { "▶" }, "PlayPause", true),
                button("⏭", "Next", player.can_go_next),
            ]
            .spacing(6);
            group = group.push(widget::ListRow::new(paint, title).subtitle(subtitle).suffix(controls));
        }
    }
    group.into()
}

/// What this computer lets the phone do: the shared cursor's edge, its files, its screenshots.
fn settings_rows<'a>(panels: &'a Panels, shell: &'a Shell<Message>) -> Element<'a, Message> {
    let paint = shell.paint();
    // Moving the cursor past this screen edge carries it (and the keyboard) to the phone.
    let edge_label = match panels.share_edge.as_deref().unwrap_or_default() {
        "left" => shell.t("Курсор на телефон: зліва", "Cursor to the phone: left edge"),
        "right" => shell.t("Курсор на телефон: справа", "Cursor to the phone: right edge"),
        "top" => shell.t("Курсор на телефон: зверху", "Cursor to the phone: top edge"),
        "bottom" => shell.t("Курсор на телефон: знизу", "Cursor to the phone: bottom edge"),
        _ => shell.t("Курсор на телефон: вимкнено", "Cursor to the phone: off"),
    };
    let edge = row![
        widget::compact_button(paint, edge_label, Tone::Neutral, Some(Message::Panel(PanelMessage::CycleShareEdge))),
        glide_sdk::iced::widget::text(shell.t("Ctrl+Alt+Esc повертає курсор", "Ctrl+Alt+Esc brings it back")).size(12),
    ]
    .spacing(8)
    .align_y(glide_sdk::iced::Alignment::Center);
    // The phone may browse this computer's home folder (hidden files never).
    let files_label = if panels.files_allowed.unwrap_or(false) {
        shell.t("Файли комп’ютера на телефоні: увімкнено", "This computer's files on the phone: on")
    } else {
        shell.t("Файли комп’ютера на телефоні: вимкнено", "This computer's files on the phone: off")
    };
    let files = widget::compact_button(
        paint,
        files_label,
        Tone::Neutral,
        Some(Message::Panel(PanelMessage::ToggleComputerFiles)),
    );
    let shots_label = if panels.send_screenshots.unwrap_or(false) {
        shell.t("Нові знімки екрана на телефон: увімкнено", "New screenshots to the phone: on")
    } else {
        shell.t("Нові знімки екрана на телефон: вимкнено", "New screenshots to the phone: off")
    };
    let shots = widget::compact_button(
        paint,
        shots_label,
        Tone::Neutral,
        Some(Message::Panel(PanelMessage::ToggleScreenshots)),
    );
    column![edge, files, shots].spacing(10).into()
}

/// KDE Connect's remote input for the phone: a touchpad, typing and special keys.
fn control_group<'a>(panels: &'a Panels, shell: &'a Shell<Message>) -> Element<'a, Message> {
    use glide_sdk::iced::widget::{container, mouse_area, text};
    let paint = shell.paint();
    let device = panels.reachable.first().and_then(|id| panels.device_names.get(id)).cloned().unwrap_or_default();
    let pad = mouse_area(
        container(text(shell.t("Сенсорна панель телефону", "Phone touchpad")).size(13))
            .center_x(glide_sdk::iced::Fill)
            .center_y(140.0)
            .style(move |_| glide_sdk::iced::widget::container::Style {
                background: Some(paint.fill_2().into()),
                border: glide_sdk::iced::Border { radius: 12.0.into(), ..Default::default() },
                ..Default::default()
            }),
    )
    .on_press(Message::Panel(PanelMessage::PadPress))
    .on_release(Message::Panel(PanelMessage::PadRelease))
    .on_right_press(Message::Panel(PanelMessage::PadRight))
    .on_move(|point| Message::Panel(PanelMessage::PadMove(point)))
    .on_scroll(|delta| Message::Panel(PanelMessage::PadScroll(delta)));
    let key = |label: &'a str, code: u64| {
        widget::compact_button(paint, label, Tone::Neutral, Some(Message::Panel(PanelMessage::Special(code))))
    };
    let keys =
        row![key("←", 4), key("↑", 5), key("↓", 7), key("→", 6), key("⌫", 1), key("⏎", 12), key("Esc", 14)].spacing(6);
    let typing = row![
        widget::text_field(
            paint,
            shell.t("Надрукувати на телефоні", "Type on the phone"),
            &panels.typed,
            |v| { Message::Panel(PanelMessage::Typed(v)) }
        ),
        widget::compact_button(
            paint,
            shell.t("Надрукувати", "Type"),
            Tone::Tonal,
            Some(Message::Panel(PanelMessage::SendTyped))
        ),
    ]
    .spacing(8);
    let share = row![
        widget::text_field(
            paint,
            shell.t("Текст або посилання для телефону", "Text or link for the phone"),
            &panels.share_draft,
            |v| { Message::Panel(PanelMessage::ShareDraft(v)) }
        ),
        widget::compact_button(
            paint,
            shell.t("Надіслати", "Send"),
            Tone::Tonal,
            Some(Message::Panel(PanelMessage::Share))
        ),
        widget::compact_button(paint, "Ping", Tone::Neutral, Some(Message::Panel(PanelMessage::Ping))),
        widget::compact_button(
            paint,
            shell.t("Буфер обміну", "Clipboard"),
            Tone::Neutral,
            Some(Message::Panel(PanelMessage::SendClipboard))
        ),
    ]
    .spacing(8);
    let settings = settings_rows(panels, shell);
    let keyboard_off = panels.reachable.first().and_then(|id| panels.keyboard_off.get(id)).copied().unwrap_or(false);
    let description = if keyboard_off {
        format!("{device} · {}", shell.t("клавіатура телефону зараз недоступна", "the phone's keyboard is unavailable"))
    } else {
        device
    };
    widget::Group::new(paint)
        .title(shell.t("Керувати телефоном", "Control the phone"))
        .description(description)
        .push(column![pad, keys, typing, share, settings].spacing(10))
        .into()
}

/// The phone's text messages: conversations, or one thread with a reply box.
fn messages_group<'a>(panels: &'a Panels, shell: &'a Shell<Message>) -> Element<'a, Message> {
    let paint = shell.paint();
    let mut group = widget::Group::new(paint).title(shell.t("Повідомлення", "Messages"));
    let Some(inbox) = panels.inbox() else { return group.into() };
    let names = &panels.names;
    let who = |message: &crate::plugins::sms::Message| {
        let number = message.addresses.join(", ");
        message
            .addresses
            .first()
            .and_then(|a| names.get(&crate::plugins::contacts::number_key(a)).cloned())
            .unwrap_or(number)
    };
    if let Some(thread) = panels.open_thread
        && let Some(messages) = inbox.threads.get(&thread)
    {
        let title = messages.last().map(&who).unwrap_or_default();
        group = group.suffix(widget::compact_button(
            paint,
            shell.t("До розмов", "All conversations"),
            Tone::Neutral,
            Some(Message::Panel(PanelMessage::CloseThread)),
        ));
        group = group.push(widget::ListRow::new(paint, title).strong());
        for message in messages.iter().rev().take(15).rev() {
            let label = if message.incoming() { "←" } else { "→" };
            group = group.push(widget::ListRow::new(paint, format!("{label} {}", message.body)));
        }
        let reply = row![
            widget::text_field(paint, shell.t("Відповідь", "Reply"), &panels.sms_draft, |v| {
                Message::Panel(PanelMessage::SmsDraft(v))
            }),
            widget::compact_button(
                paint,
                shell.t("Надіслати", "Send"),
                Tone::Tonal,
                Some(Message::Panel(PanelMessage::SendSms))
            ),
        ]
        .spacing(8);
        return group.push(reply).into();
    }
    for (thread, last) in inbox.conversations().into_iter().take(20) {
        group = group.push(widget::ListRow::new(paint, who(last)).subtitle(last.body.as_str()).suffix(
            widget::compact_button(
                paint,
                shell.t("Відкрити", "Open"),
                Tone::Neutral,
                Some(Message::Panel(PanelMessage::OpenThread(thread))),
            ),
        ));
    }
    group.into()
}
