use std::time::Duration;

use iced::widget::{Column, button, column, container, pick_list, row, rule, scrollable, space, text, text_input};
use iced::{Alignment, Color, Element, Length, Theme};
use kg_protocol::StepState;

use crate::app::{App, Message, Notice, Screen, Session, Setup, Status};

const CARD_WIDTH: f32 = 560.0;

pub fn view(app: &App) -> Element<'_, Message> {
    let header = row![
        text("kyber-gaming").size(22),
        space().width(Length::Fill),
        text(app.connection.as_ref().map(|c| c.name).unwrap_or("Not connected")).size(14).style(muted),
        settings_button(app),
    ]
    .spacing(16)
    .align_y(Alignment::Center);

    let body = match &app.screen {
        Screen::Setup(setup) => setup_view(setup, app.connection.is_some()),
        Screen::Home => home_view(app),
        Screen::Session(session) => session_view(session),
    };

    let mut page = column![header, rule::horizontal(1)].spacing(16).padding(24);
    if let Some(notice) = &app.notice {
        page = page.push(notice_view(notice));
    }
    page.push(container(body).center_x(Length::Fill).padding([24, 0])).into()
}

fn settings_button(app: &App) -> Element<'_, Message> {
    let editable = matches!(app.screen, Screen::Home) && !app.demo;
    button(text("Settings").size(14)).style(button::text).on_press_maybe(editable.then_some(Message::EditSettings)).into()
}

fn notice_view(notice: &Notice) -> Element<'_, Message> {
    let (message, style): (&str, fn(&Theme) -> container::Style) = match notice {
        Notice::Info(m) => (m, container::rounded_box),
        Notice::Error(m) => (m, error_box),
    };
    container(
        row![text(message).size(14).width(Length::Fill), button(text("✕").size(12)).style(button::text).on_press(Message::DismissNotice)]
            .align_y(Alignment::Center),
    )
    .padding(12)
    .width(Length::Fill)
    .style(style)
    .into()
}

fn setup_view(setup: &Setup, can_cancel: bool) -> Element<'_, Message> {
    let mut form = Column::new().spacing(12).width(CARD_WIDTH);
    form = form.push(text(format!("Connect your {} account", setup.provider.name)).size(24));
    form = form.push(
        text("Machines are created and billed on your own account. Credentials stay in your system keychain.")
            .size(14)
            .style(muted),
    );
    for field in setup.provider.fields {
        let value = setup.values.get(field.key).map(String::as_str).unwrap_or_default();
        form = form.push(column![
            text(field.label).size(13),
            text_input(field.label, value).secure(field.secret).on_input(move |v| Message::Field(field.key, v)).padding(10),
        ]
        .spacing(4));
    }

    let complete = setup.provider.is_complete(&setup.values);
    let check_label = if setup.checking { "Checking…" } else { "Check credentials" };
    form = form.push(button(text(check_label)).on_press_maybe((complete && !setup.checking).then_some(Message::Check)));

    if let Some(error) = &setup.error {
        form = form.push(text(error).size(14).style(text::danger));
    }
    if !setup.machine_types.is_empty() {
        form = form.push(rule::horizontal(1));
        form = form.push(text("Machine").size(13));
        form = form.push(pick_list(setup.machine_types.as_slice(), setup.selected.clone(), Message::SelectInSetup).width(Length::Fill));
    }

    let mut actions = row![].spacing(12);
    if can_cancel {
        actions = actions.push(button(text("Cancel")).style(button::secondary).on_press(Message::CancelSetup));
    }
    actions = actions.push(button(text("Save")).style(button::primary).on_press_maybe(setup.selected.is_some().then_some(Message::SaveSetup)));
    scrollable(form.push(actions)).into()
}

fn home_view(app: &App) -> Element<'_, Message> {
    let machine = app.selected_machine();
    let machines: Element<'_, Message> = if app.machine_types.is_empty() {
        text("Loading machines…").size(14).style(muted).into()
    } else {
        pick_list(app.machine_types.as_slice(), machine.cloned(), Message::SelectMachine).width(Length::Fill).into()
    };

    let play = button(container(text("Play").size(28)).center_x(Length::Fill).padding(12))
        .width(Length::Fill)
        .style(button::success)
        .on_press_maybe(machine.is_some().then_some(Message::Play));

    column![
        text("Ready when you are").size(28),
        text("A GPU machine is created when you press play and destroyed when you stop.").size(14).style(muted),
        space().height(8),
        text("Machine").size(13),
        machines,
        space().height(8),
        play,
    ]
    .spacing(8)
    .width(CARD_WIDTH)
    .into()
}

fn session_view(session: &Session) -> Element<'_, Message> {
    let (title, subtitle) = match &session.status {
        Status::Launching => ("Creating your machine", "Asking the provider for a GPU."),
        Status::Booting => ("Booting", "The machine is starting up."),
        Status::Provisioning => ("Setting things up", "Drivers, desktop, Steam and the streaming server."),
        Status::Ready(_) => ("Ready to play", "The streaming server is waiting for you."),
        Status::Stopping => ("Stopping", "Destroying the machine so it stops costing anything."),
    };

    let mut facts = row![fact("Time", format_duration(session.elapsed()))].spacing(32);
    if let Some(cost) = session.cost_eur() {
        facts = facts.push(fact("Cost so far", format!("{cost:.2} €")));
    }
    if let Some(ip) = session.ip {
        facts = facts.push(fact("Address", ip.to_string()));
    }

    let mut steps = Column::new().spacing(6);
    for (name, state) in &session.steps {
        let (glyph, detail) = match state {
            StepState::Running => ("◐", None),
            StepState::Done => ("✓", None),
            StepState::Skipped => ("↷", Some("already done")),
            StepState::Failed { error } => ("✗", Some(error.as_str())),
        };
        let mut line = row![text(glyph).width(20), text(name.as_str())].spacing(8);
        if let Some(detail) = detail {
            line = line.push(text(detail).size(13).style(muted));
        }
        steps = steps.push(line);
    }

    let mut page = column![text(title).size(28), text(subtitle).size(14).style(muted), facts].spacing(12).width(CARD_WIDTH);
    if let Status::Ready(kyber) = &session.status {
        page = page.push(
            container(text(format!("Kyber is listening on {kyber}. Launching the client from here comes with the SSH bridge.")).size(14))
                .padding(12)
                .width(Length::Fill)
                .style(container::rounded_box),
        );
    }
    if !session.steps.is_empty() {
        page = page.push(rule::horizontal(1)).push(steps);
    }
    let stopping = session.status == Status::Stopping;
    page.push(space().height(8))
        .push(button(text(if stopping { "Stopping…" } else { "Stop and destroy the machine" })).style(button::danger).on_press_maybe((!stopping).then_some(Message::Stop)))
        .into()
}

fn fact<'a>(label: &'a str, value: String) -> Element<'a, Message> {
    column![text(label).size(12).style(muted), text(value).size(18)].spacing(2).into()
}

fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}h{:02}", s / 3600, s / 60 % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn muted(theme: &Theme) -> text::Style {
    let palette = theme.extended_palette();
    text::Style { color: Some(palette.background.base.text.scale_alpha(0.6)) }
}

fn error_box(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.danger.weak.color.into()),
        text_color: Some(palette.danger.weak.text),
        border: iced::border::rounded(6),
        ..container::Style::default()
    }
    .color(Color::TRANSPARENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_clock() {
        assert_eq!(format_duration(Duration::from_secs(65)), "1:05");
        assert_eq!(format_duration(Duration::from_secs(3 * 3600 + 7 * 60)), "3h07");
    }
}
