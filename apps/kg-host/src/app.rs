use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Subscription, Task, task};
use kg_protocol::{Provision, StepState};
use kg_provider::{InstanceId, MachineType, Provider, ProviderAccess};

use crate::agent::{Agent, NotYetBridged};
use crate::demo::{DemoAgent, DemoProvider};
use crate::providers::{self, PROVIDERS, ProviderUi};
use crate::session::{self, Plan, SessionEvent};
use crate::settings::{self, Settings};

pub struct App {
    pub demo: bool,
    pub settings: Settings,
    pub connection: Option<Connection>,
    pub machine_types: Vec<MachineChoice>,
    pub screen: Screen,
    pub notice: Option<Notice>,
}

pub struct Connection {
    pub name: &'static str,
    pub provider: Arc<dyn Provider>,
    pub agent: Arc<dyn Agent>,
}

pub enum Screen {
    Setup(Setup),
    Home,
    Session(Session),
}

pub struct Setup {
    pub provider: &'static ProviderUi,
    pub values: BTreeMap<&'static str, String>,
    pub checking: bool,
    pub machine_types: Vec<MachineChoice>,
    pub selected: Option<MachineChoice>,
    pub error: Option<String>,
}

pub struct Session {
    pub started: Instant,
    pub now: Instant,
    pub hourly_price_eur: Option<f64>,
    pub instance: Option<InstanceId>,
    pub ip: Option<IpAddr>,
    pub steps: Vec<(String, StepState)>,
    pub status: Status,
    handle: task::Handle,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Launching,
    Booting,
    Provisioning,
    Ready(SocketAddr),
    Stopping,
}

#[derive(Debug, Clone)]
pub enum Notice {
    Info(String),
    Error(String),
}

/// A machine type as offered in pick lists.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineChoice(pub MachineType);

impl fmt::Display for MachineChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = &self.0;
        match &t.gpu {
            Some(gpu) if gpu.count > 1 => write!(f, "{}x {} {} GB", gpu.count, gpu.model, gpu.vram_gb)?,
            Some(gpu) => write!(f, "{} {} GB", gpu.model, gpu.vram_gb)?,
            None => write!(f, "{}", t.id.0)?,
        }
        write!(f, "  ·  {} vCPU  ·  {} GB RAM", t.vcpus, t.ram_gb)?;
        if let Some(price) = t.hourly_price_eur {
            write!(f, "  ·  {price:.2} €/h")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    MachineTypes(Result<Vec<MachineType>, String>),
    Reconciled(Result<usize, String>),
    Field(&'static str, String),
    Check,
    Checked(Result<Vec<MachineType>, String>),
    SelectInSetup(MachineChoice),
    SaveSetup,
    EditSettings,
    CancelSetup,
    SelectMachine(MachineChoice),
    Play,
    Session(SessionEvent),
    Stop,
    Stopped(Result<(), String>),
    Tick,
    DismissNotice,
}

fn gpu_only(types: Vec<MachineType>) -> Vec<MachineChoice> {
    let mut gpus: Vec<_> = types.into_iter().filter(|t| t.gpu.is_some()).map(MachineChoice).collect();
    gpus.sort_by(|a, b| a.0.hourly_price_eur.partial_cmp(&b.0.hourly_price_eur).unwrap_or(std::cmp::Ordering::Equal));
    gpus
}

impl App {
    pub fn boot() -> (Self, Task<Message>) {
        let demo = std::env::var_os("KG_DEMO").is_some();
        let settings = Settings::load().unwrap_or_default();
        let mut app = Self { demo, settings, connection: None, machine_types: vec![], screen: Screen::Home, notice: None };

        if demo {
            app.connection = Some(Connection {
                name: "Demo",
                provider: Arc::new(DemoProvider::new()),
                agent: Arc::new(DemoAgent::new()),
            });
        } else {
            match settings::load_access() {
                Ok(Some(access)) => match connect(&access) {
                    Ok(connection) => app.connection = Some(connection),
                    Err(e) => app.notice = Some(Notice::Error(e)),
                },
                Ok(None) => {}
                Err(e) => app.notice = Some(Notice::Error(format!("cannot read the keychain: {e:#}"))),
            }
        }

        let Some(connection) = &app.connection else {
            app.screen = Screen::Setup(Setup::new(&PROVIDERS[0], None));
            return (app, Task::none());
        };
        let provider = connection.provider.clone();
        let types = Task::perform(async move { provider.machine_types().await.map_err(|e| e.to_string()) }, Message::MachineTypes);
        let provider = connection.provider.clone();
        let cleanup = Task::perform(
            async move { session::reconcile(provider.as_ref()).await.map_err(|e| e.to_string()) },
            Message::Reconciled,
        );
        (app, Task::batch([types, cleanup]))
    }

    pub fn title(&self) -> String {
        match &self.screen {
            Screen::Session(s) if matches!(s.status, Status::Ready(_)) => "kyber-gaming · playing".into(),
            Screen::Session(_) => "kyber-gaming · starting".into(),
            _ => "kyber-gaming".into(),
        }
    }

    pub fn selected_machine(&self) -> Option<&MachineChoice> {
        let wanted = self.settings.machine_type.as_ref()?;
        self.machine_types.iter().find(|m| &m.0.id == wanted)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        match self.screen {
            Screen::Session(_) => iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick),
            _ => Subscription::none(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::MachineTypes(Ok(types)) => {
                self.machine_types = gpu_only(types);
                if self.selected_machine().is_none() {
                    self.settings.machine_type = self.machine_types.first().map(|m| m.0.id.clone());
                }
            }
            Message::MachineTypes(Err(e)) => self.notice = Some(Notice::Error(format!("cannot list machines: {e}"))),
            Message::Reconciled(Ok(0)) => {}
            Message::Reconciled(Ok(n)) => self.notice = Some(Notice::Info(format!("Cleaned up {n} leftover(s) from a previous session."))),
            Message::Reconciled(Err(e)) => self.notice = Some(Notice::Error(format!("cleanup failed: {e}"))),

            Message::Field(key, value) => {
                if let Screen::Setup(setup) = &mut self.screen {
                    setup.values.insert(key, value);
                    setup.machine_types.clear();
                    setup.selected = None;
                    setup.error = None;
                }
            }
            Message::Check => {
                if let Screen::Setup(setup) = &mut self.screen {
                    let access = setup.provider.access(&setup.values);
                    match connect(&access) {
                        Ok(connection) => {
                            setup.checking = true;
                            let provider = connection.provider;
                            return Task::perform(
                                async move { provider.machine_types().await.map_err(|e| e.to_string()) },
                                Message::Checked,
                            );
                        }
                        Err(e) => setup.error = Some(e),
                    }
                }
            }
            Message::Checked(result) => {
                if let Screen::Setup(setup) = &mut self.screen {
                    setup.checking = false;
                    match result {
                        Ok(types) => {
                            setup.machine_types = gpu_only(types);
                            setup.selected = setup.machine_types.first().cloned();
                            if setup.machine_types.is_empty() {
                                setup.error = Some("These credentials work, but no GPU machine is offered here.".into());
                            }
                        }
                        Err(e) => setup.error = Some(e),
                    }
                }
            }
            Message::SelectInSetup(choice) => {
                if let Screen::Setup(setup) = &mut self.screen {
                    setup.selected = Some(choice);
                }
            }
            Message::SaveSetup => {
                if let Screen::Setup(setup) = &self.screen {
                    let access = setup.provider.access(&setup.values);
                    let result = connect(&access)
                        .and_then(|connection| settings::save_access(&access).map(|()| connection).map_err(|e| format!("{e:#}")));
                    match result {
                        Ok(connection) => {
                            self.machine_types = setup.machine_types.clone();
                            self.settings.machine_type = setup.selected.as_ref().map(|m| m.0.id.clone());
                            self.persist_settings();
                            self.connection = Some(connection);
                            self.screen = Screen::Home;
                        }
                        Err(e) => {
                            if let Screen::Setup(setup) = &mut self.screen {
                                setup.error = Some(e);
                            }
                        }
                    }
                }
            }
            Message::EditSettings => {
                let provider = self.connection.as_ref().and_then(|c| providers::find(c.provider.kind())).unwrap_or(&PROVIDERS[0]);
                let mut setup = Setup::new(provider, self.selected_machine().cloned());
                if let Ok(Some(access)) = settings::load_access() {
                    setup.prefill(&access);
                }
                self.screen = Screen::Setup(setup);
            }
            Message::CancelSetup => self.screen = Screen::Home,
            Message::SelectMachine(choice) => {
                self.settings.machine_type = Some(choice.0.id);
                self.persist_settings();
            }

            Message::Play => return self.play(),
            Message::Session(event) => return self.on_session_event(event),
            Message::Stop => return self.stop(None),
            Message::Stopped(result) => {
                self.screen = Screen::Home;
                self.notice = Some(match result {
                    Ok(()) => Notice::Info("Machine destroyed, nothing is running anymore.".into()),
                    Err(e) => Notice::Error(format!("could not destroy the machine, check your provider console: {e}")),
                });
            }
            Message::Tick => {
                if let Screen::Session(session) = &mut self.screen {
                    session.now = Instant::now();
                }
            }
            Message::DismissNotice => self.notice = None,
        }
        Task::none()
    }

    fn persist_settings(&mut self) {
        if !self.demo
            && let Err(e) = self.settings.save()
        {
            self.notice = Some(Notice::Error(format!("cannot save settings: {e:#}")));
        }
    }

    fn play(&mut self) -> Task<Message> {
        let (Some(connection), Some(machine)) = (&self.connection, self.selected_machine().cloned()) else {
            return Task::none();
        };
        let (provider, agent) = (connection.provider.clone(), connection.agent.clone());
        let ssh_public_key = if self.demo { Ok("ssh-ed25519 AAAA demo".into()) } else { settings::ssh_public_key() };
        let ssh_public_key = match ssh_public_key {
            Ok(key) => key,
            Err(e) => {
                self.notice = Some(Notice::Error(format!("cannot prepare the SSH key: {e:#}")));
                return Task::none();
            }
        };
        let plan = Plan {
            machine_type: machine.0.id.clone(),
            ssh_public_key,
            provision: Provision {
                runtime: self.settings.runtime.clone(),
                kyber_password: settings::session_password(),
                resolution: self.settings.resolution,
            },
        };
        let poll = Duration::from_secs(if self.demo { 1 } else { 3 });
        let events = session::start(provider, agent, plan, poll);
        let (task, handle) = Task::run(events, Message::Session).abortable();

        let now = Instant::now();
        self.notice = None;
        self.screen = Screen::Session(Session {
            started: now,
            now,
            hourly_price_eur: machine.0.hourly_price_eur,
            instance: None,
            ip: None,
            steps: vec![],
            status: Status::Launching,
            handle,
        });
        task
    }

    fn on_session_event(&mut self, event: SessionEvent) -> Task<Message> {
        let Screen::Session(session) = &mut self.screen else {
            return Task::none();
        };
        match event {
            SessionEvent::Cleaned { leftovers: 0 } => {}
            SessionEvent::Cleaned { leftovers } => {
                self.notice = Some(Notice::Info(format!("Cleaned up {leftovers} leftover(s) before starting.")));
            }
            SessionEvent::Launched { instance } => {
                session.instance = Some(instance);
                session.status = Status::Booting;
            }
            SessionEvent::Booted { ip } => {
                session.ip = Some(ip);
                session.status = Status::Provisioning;
            }
            SessionEvent::Step { name, state } => match session.steps.iter_mut().find(|(n, _)| *n == name) {
                Some(entry) => entry.1 = state,
                None => session.steps.push((name, state)),
            },
            SessionEvent::Ready { kyber } => session.status = Status::Ready(kyber),
            // Nobody may be watching, so a failed session never keeps a paying machine around.
            SessionEvent::Failed(error) => return self.stop(Some(error)),
        }
        Task::none()
    }

    fn stop(&mut self, failure: Option<String>) -> Task<Message> {
        let (Screen::Session(session), Some(connection)) = (&mut self.screen, &self.connection) else {
            return Task::none();
        };
        session.handle.abort();
        if let Some(error) = &failure {
            self.notice = Some(Notice::Error(error.clone()));
        }
        session.status = Status::Stopping;
        let provider = connection.provider.clone();
        match session.instance.clone() {
            Some(instance) => Task::perform(
                async move { session::stop(provider, instance).await.map_err(|e| e.to_string()) },
                move |result| match (&result, &failure) {
                    (Ok(()), Some(error)) => Message::Stopped(Err(format!("{error} (the machine was destroyed)"))),
                    _ => Message::Stopped(result),
                },
            ),
            // Stopped before the provider answered: the instance may exist without us knowing its id.
            None => Task::perform(
                async move { session::reconcile(provider.as_ref()).await.map(drop).map_err(|e| e.to_string()) },
                Message::Stopped,
            ),
        }
    }
}

impl Setup {
    fn new(provider: &'static ProviderUi, selected: Option<MachineChoice>) -> Self {
        let values = provider.fields.iter().map(|f| (f.key, f.default.to_string())).collect();
        Self { provider, values, checking: false, machine_types: vec![], selected, error: None }
    }

    fn prefill(&mut self, access: &ProviderAccess) {
        use secrecy::ExposeSecret;
        if access.kind != self.provider.kind {
            return;
        }
        let Ok(serde_json::Value::Object(config)) = serde_json::from_str(access.config.expose_secret()) else {
            return;
        };
        for field in self.provider.fields {
            if let Some(serde_json::Value::String(value)) = config.get(field.key) {
                self.values.insert(field.key, value.clone());
            }
        }
    }
}

impl Session {
    pub fn elapsed(&self) -> Duration {
        self.now.duration_since(self.started)
    }

    pub fn cost_eur(&self) -> Option<f64> {
        self.hourly_price_eur.map(|price| price * self.elapsed().as_secs_f64() / 3600.0)
    }
}

fn connect(access: &ProviderAccess) -> Result<Connection, String> {
    let name = providers::find(&access.kind).map(|p| p.name).unwrap_or("Unknown provider");
    let provider: Arc<dyn Provider> = Arc::from(providers::registry().connect(access).map_err(|e| e.to_string())?);
    Ok(Connection { name, provider, agent: Arc::new(NotYetBridged) })
}
