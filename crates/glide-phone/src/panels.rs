//! Window sections for the features beyond the device cards.

use std::collections::BTreeMap;

use glide_sdk::iced::widget::{column, row};
use glide_sdk::prelude::*;
use glide_sdk::widget::{Icon, Tone};

use crate::plugins::runcommand::Command;
use crate::ui::{Message, global_manager, proxy};

/// State of the feature sections.
#[derive(Debug, Default)]
pub struct Panels {
    pub commands: Vec<CommandRow>,
    pub draft_name: String,
    pub draft_command: String,
}

#[derive(Debug, Clone)]
pub enum PanelMessage {
    CommandName(String),
    CommandLine(String),
    AddCommand,
    RemoveCommand(String),
}

impl Panels {
    pub fn update(&mut self, message: PanelMessage) -> Task<Message> {
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
        }
        Task::none()
    }

    #[must_use]
    pub fn view<'a>(&'a self, shell: &'a Shell<Message>) -> Vec<Element<'a, Message>> {
        vec![commands_group(self, shell)]
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
