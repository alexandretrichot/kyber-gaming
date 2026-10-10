mod agent;
mod app;
mod demo;
mod providers;
mod session;
mod settings;
mod view;

use app::App;

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    iced::application(App::boot, App::update, view::view)
        .title(App::title)
        .subscription(App::subscription)
        .theme(|_: &App| iced::Theme::TokyoNight)
        .window_size((760.0, 720.0))
        .centered()
        .run()
}
