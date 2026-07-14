use std::sync::{Arc, mpsc};

use iris::{
    Align, Application, Color, Config, Frame, Icon, LayoutOpts, PaintCanvas, TextBuf, Theme,
};
use neenee_intelligence::{ExpertMeeting, ExpertPanel, OpinionHub, OpinionState, ReviewScenario};
use neenee_quant_gui::{AppState, TradingMode, View};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let action = parse_launch_action(std::env::args().skip(1)).map_err(|message| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{message}\n\n{}", usage()),
        )
    })?;
    let LaunchAction::Run(profile) = action else {
        println!("{}", usage());
        return Ok(());
    };
    let mut config = neenee_quant::QuantConfig::from_environment()?;
    profile.apply(&mut config);
    let mut state = GuiState::new(AppState::from_config(config)?)?;
    let cfg = Config::new("neenee intelligence terminal")?
        .size(1480, 900)
        .force_dark();
    Application::run(
        cfg,
        move |frame, _input| {
            build_ui(frame, &mut state);
        },
        None::<fn(PaintCanvas)>,
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchProfile {
    Environment,
    Paper,
    LongportLive,
}

impl LaunchProfile {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "environment" | "env" => Ok(Self::Environment),
            "paper" | "simulation" | "sim" => Ok(Self::Paper),
            "longport-live" | "longbridge-live" => Ok(Self::LongportLive),
            other => Err(format!(
                "unknown quant profile '{other}'; use environment, paper, or longport-live"
            )),
        }
    }

    fn apply(self, config: &mut neenee_quant::QuantConfig) {
        match self {
            Self::Environment => {}
            Self::Paper => {
                config.market_data.source = "synthetic".to_string();
                config.broker.mode = "paper".to_string();
            }
            Self::LongportLive => {
                config.market_data.source = "longport".to_string();
                config.broker.mode = "longport".to_string();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchAction {
    Run(LaunchProfile),
    Help,
}

fn parse_launch_action(args: impl IntoIterator<Item = String>) -> Result<LaunchAction, String> {
    let mut args = args.into_iter();
    let mut selected = None;
    while let Some(argument) = args.next() {
        let profile = match argument.as_str() {
            "-h" | "--help" => return Ok(LaunchAction::Help),
            "--paper" => LaunchProfile::Paper,
            "--longport-live" => LaunchProfile::LongportLive,
            "--profile" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--profile requires a value".to_string())?;
                LaunchProfile::parse(&value)?
            }
            _ if argument.starts_with("--profile=") => {
                LaunchProfile::parse(&argument["--profile=".len()..])?
            }
            _ => return Err(format!("unknown argument '{argument}'")),
        };
        if selected.replace(profile).is_some() {
            return Err("select only one quant launch profile".to_string());
        }
    }
    Ok(LaunchAction::Run(
        selected.unwrap_or(LaunchProfile::Environment),
    ))
}

fn usage() -> &'static str {
    "Usage: neenee-quant-gui [--paper | --longport-live | --profile <PROFILE>]\n\
\n\
Profiles:\n\
  environment    Read NEENEE_QUANT_* variables (default; paper when unset)\n\
  paper          Force synthetic market data and the simulated paper broker\n\
  longport-live  Force LongPort market data and live brokerage; starts disarmed"
}

struct GuiState {
    app: AppState,
    background: tokio::runtime::Runtime,
    background_tx: mpsc::Sender<BackgroundEvent>,
    background_rx: mpsc::Receiver<BackgroundEvent>,
    opinion: Arc<tokio::sync::Mutex<OpinionHub>>,
    opinion_state: OpinionState,
    expert_panel: Option<Arc<tokio::sync::Mutex<ExpertPanel>>>,
    expert_meetings: Vec<ExpertMeeting>,
    intelligence_busy: bool,
    expert_busy: bool,
    intelligence_status: String,
    expert_status: String,
    intelligence_tab: i32,
    expert_scenario: i32,
    symbol: TextBuf,
    interval: TextBuf,
    strategy: TextBuf,
    start: TextBuf,
    end: TextBuf,
    capital: TextBuf,
    quantity: TextBuf,
    price: TextBuf,
    order_id: TextBuf,
    topic_label: TextBuf,
    topic_query: TextBuf,
    watch_label: TextBuf,
    watch_url: TextBuf,
    expert_topic: TextBuf,
    expert_context: TextBuf,
}

enum BackgroundEvent {
    Opinion(Result<OpinionState, String>),
    Meeting(Result<ExpertMeeting, String>),
}

impl GuiState {
    fn new(app: AppState) -> Result<Self, std::io::Error> {
        let background = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (background_tx, background_rx) = mpsc::channel();
        let opinion = OpinionHub::system_default();
        let opinion_state = opinion.state().clone();
        let (expert_panel, expert_meetings, expert_status) = match ExpertPanel::system_default() {
            Ok(panel) => {
                let meetings = panel.meetings().to_vec();
                (
                    Some(Arc::new(tokio::sync::Mutex::new(panel))),
                    meetings,
                    "Expert council ready · 5 roles · 2 rounds · 1 manager".to_string(),
                )
            }
            Err(error) => (None, Vec::new(), error),
        };
        Ok(Self {
            background,
            background_tx,
            background_rx,
            opinion: Arc::new(tokio::sync::Mutex::new(opinion)),
            opinion_state,
            expert_panel,
            expert_meetings,
            intelligence_busy: false,
            expert_busy: false,
            intelligence_status: "Ready to collect configured topics and inspect watched links"
                .to_string(),
            expert_status,
            intelligence_tab: 0,
            expert_scenario: 0,
            symbol: TextBuf::new(32, &app.symbol),
            interval: TextBuf::new(16, &app.interval),
            strategy: TextBuf::new(96, &app.strategy),
            start: TextBuf::new(16, &app.start),
            end: TextBuf::new(16, &app.end),
            capital: TextBuf::new(24, &app.capital),
            quantity: TextBuf::new(24, &app.quantity),
            price: TextBuf::new(24, &app.price),
            order_id: TextBuf::new(32, &app.order_id),
            topic_label: TextBuf::new(64, "Custom topic"),
            topic_query: TextBuf::new(320, ""),
            watch_label: TextBuf::new(96, "Tracked source"),
            watch_url: TextBuf::new(512, ""),
            expert_topic: TextBuf::new(320, ""),
            expert_context: TextBuf::new(4096, ""),
            app,
        })
    }

    fn sync_inputs(&mut self) {
        self.app.symbol = self.symbol.as_str().trim().to_string();
        self.app.interval = self.interval.as_str().trim().to_string();
        self.app.strategy = self.strategy.as_str().trim().to_string();
        self.app.start = self.start.as_str().trim().to_string();
        self.app.end = self.end.as_str().trim().to_string();
        self.app.capital = self.capital.as_str().trim().to_string();
        self.app.quantity = self.quantity.as_str().trim().to_string();
        self.app.price = self.price.as_str().trim().to_string();
        self.app.order_id = self.order_id.as_str().trim().to_string();
    }

    fn poll_background(&mut self) {
        while let Ok(event) = self.background_rx.try_recv() {
            match event {
                BackgroundEvent::Opinion(Ok(state)) => {
                    self.opinion_state = state;
                    self.intelligence_status = format!(
                        "Collected {} ranked signals · {} watched links",
                        self.opinion_state.items.len(),
                        self.opinion_state.watched_links.len()
                    );
                    self.intelligence_busy = false;
                }
                BackgroundEvent::Opinion(Err(error)) => {
                    self.intelligence_status = format!("Intelligence refresh failed: {error}");
                    self.intelligence_busy = false;
                }
                BackgroundEvent::Meeting(Ok(meeting)) => {
                    self.expert_status = format!(
                        "Meeting complete · {:.0}% manager confidence",
                        meeting.conclusion.confidence * 100.0
                    );
                    self.expert_meetings.insert(0, meeting);
                    self.expert_meetings.truncate(20);
                    self.expert_busy = false;
                }
                BackgroundEvent::Meeting(Err(error)) => {
                    self.expert_status = format!("Expert meeting failed: {error}");
                    self.expert_busy = false;
                }
            }
        }
    }

    fn refresh_intelligence(&mut self) {
        if self.intelligence_busy {
            return;
        }
        self.intelligence_busy = true;
        self.intelligence_status = "Collecting top signals and checking links…".to_string();
        let opinion = Arc::clone(&self.opinion);
        let sender = self.background_tx.clone();
        self.background.spawn(async move {
            let result = {
                let mut hub = opinion.lock().await;
                hub.refresh().await.cloned()
            };
            let _ = sender.send(BackgroundEvent::Opinion(result));
        });
    }

    fn add_topic(&mut self) {
        let label = self.topic_label.as_str().to_string();
        let query = self.topic_query.as_str().to_string();
        let result = self.background.block_on(async {
            let mut hub = self.opinion.lock().await;
            hub.add_topic(&label, &query).map(|()| hub.state().clone())
        });
        match result {
            Ok(state) => {
                self.opinion_state = state;
                self.topic_query.set("");
                self.intelligence_status = "Topic added; refresh to collect signals".to_string();
            }
            Err(error) => self.intelligence_status = error,
        }
    }

    fn add_watch(&mut self) {
        let label = self.watch_label.as_str().to_string();
        let url = self.watch_url.as_str().to_string();
        let result = self.background.block_on(async {
            let mut hub = self.opinion.lock().await;
            hub.add_watch(&label, &url).map(|()| hub.state().clone())
        });
        match result {
            Ok(state) => {
                self.opinion_state = state;
                self.watch_url.set("");
                self.intelligence_status = "Link added; refresh to establish baseline".to_string();
            }
            Err(error) => self.intelligence_status = error,
        }
    }

    fn convene_experts(&mut self) {
        if self.expert_busy {
            return;
        }
        let Some(panel) = self.expert_panel.as_ref().map(Arc::clone) else {
            self.expert_status =
                "Configure a neenee AI provider before starting an expert meeting".to_string();
            return;
        };
        let topic = self.expert_topic.as_str().trim().to_string();
        if topic.is_empty() {
            self.expert_status = "Enter a concrete decision question first".to_string();
            return;
        }
        let context = self.expert_context.as_str().trim().to_string();
        let scenario = match self.expert_scenario {
            1 => ReviewScenario::MarketEvent,
            2 => ReviewScenario::TradeRisk,
            3 => ReviewScenario::StrategyReview,
            _ => ReviewScenario::InvestmentThesis,
        };
        self.expert_busy = true;
        self.expert_status = "Council in session · independent review round…".to_string();
        let sender = self.background_tx.clone();
        self.background.spawn(async move {
            let result = {
                let mut panel = panel.lock().await;
                panel.convene(scenario, &topic, &context).await.cloned()
            };
            let _ = sender.send(BackgroundEvent::Meeting(result));
        });
    }
}

fn build_ui(frame: &mut Frame, state: &mut GuiState) {
    state.poll_background();
    let palette = Palette::terminal();
    frame.set_theme(
        Theme::dark()
            .with_bg(palette.page)
            .with_fg(Color::rgba(230, 237, 243, 255))
            .with_accent(Color::rgba(74, 222, 128, 255))
            .with_border(Color::rgba(53, 64, 76, 255))
            .with_hover(Color::rgba(37, 50, 63, 255))
            .with_active(Color::rgba(45, 68, 61, 255))
            .with_disabled(Color::rgba(95, 105, 118, 255))
            .with_error(Color::rgba(248, 113, 113, 255))
            .with_corner_radius(10.0)
            .with_border_width(1.0)
            .with_active_indicator_width(3.0)
            .with_scrollbar_width(7.0)
            .with_scrollbar_radius(4.0)
            .with_font_size(14.0),
    );
    frame.column_ex(
        &LayoutOpts {
            flex: 1.0,
            gap: 12.0,
            pad: 14.0,
            bg: palette.page,
            ..LayoutOpts::default()
        },
        |frame| {
            top_bar(frame, state, palette);
            frame.row_ex(
                &LayoutOpts {
                    flex: 1.0,
                    gap: 12.0,
                    cross: Align::Stretch,
                    ..LayoutOpts::default()
                },
                |frame| {
                    sidebar(frame, state, palette);
                    workspace(frame, state, palette);
                    inspector(frame, state, palette);
                },
            );
        },
    );
}

#[derive(Clone, Copy)]
struct Palette {
    page: Color,
    panel: Color,
    elevated: Color,
    subtle: Color,
    accent_tint: Color,
    positive_tint: Color,
    warning_tint: Color,
    danger_tint: Color,
}

impl Palette {
    fn terminal() -> Self {
        Self {
            page: Color::rgba(9, 14, 20, 255),
            panel: Color::rgba(16, 23, 31, 255),
            elevated: Color::rgba(22, 31, 41, 255),
            subtle: Color::rgba(28, 39, 51, 255),
            accent_tint: Color::rgba(20, 60, 52, 255),
            positive_tint: Color::rgba(18, 55, 39, 255),
            warning_tint: Color::rgba(69, 52, 22, 255),
            danger_tint: Color::rgba(72, 31, 36, 255),
        }
    }
}

fn top_bar(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    frame.row_ex(
        &LayoutOpts {
            height: 58.0,
            gap: 12.0,
            pad: 10.0,
            cross: Align::Center,
            bg: palette.panel,
            radius: 12.0,
            ..LayoutOpts::default()
        },
        |frame| {
            frame.icon(Icon::Activity, 22.0);
            frame.heading("NEENEE  /  DECISION INTELLIGENCE", 3);
            frame.flex(1.0);
            status_pill(
                frame,
                &format!(
                    "{} · {}",
                    state.app.market_data_source,
                    state.app.broker_label()
                ),
                palette.subtle,
            );
            status_pill(
                frame,
                if state.intelligence_busy {
                    "NETWORK  SYNCING"
                } else {
                    "NETWORK  READY"
                },
                if state.intelligence_busy {
                    palette.warning_tint
                } else {
                    palette.positive_tint
                },
            );
            status_pill(
                frame,
                state.app.mode.label(),
                if state.app.mode == TradingMode::TradingArmed {
                    palette.danger_tint
                } else {
                    palette.subtle
                },
            );
            if frame.button(match state.app.mode {
                TradingMode::Disarmed => "Arm trading",
                TradingMode::TradingArmed => "Disarm trading",
            }) {
                state.app.toggle_mode();
            }
        },
    );
}

fn status_pill(frame: &mut Frame, text: &str, color: Color) {
    frame.row_ex(
        &LayoutOpts {
            gap: 5.0,
            pad: 7.0,
            cross: Align::Center,
            bg: color,
            radius: 9.0,
            ..LayoutOpts::default()
        },
        |frame| frame.label_compact_sized(text, 11.0),
    );
}

fn sidebar(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    frame.column_ex(
        &LayoutOpts {
            width: 222.0,
            gap: 7.0,
            pad: 10.0,
            bg: palette.panel,
            radius: 12.0,
            ..LayoutOpts::default()
        },
        |frame| {
            frame.label_compact_sized("DECISION DESK", 11.0);
            nav_item(frame, &mut state.app, View::Dashboard, Icon::Grid);
            nav_item(frame, &mut state.app, View::Intelligence, Icon::Globe);
            nav_item(frame, &mut state.app, View::Experts, Icon::Users);
            frame.separator();
            frame.label_compact_sized("TRADING", 11.0);
            nav_item(frame, &mut state.app, View::Market, Icon::TrendingUp);
            nav_item(frame, &mut state.app, View::Portfolio, Icon::Briefcase);
            nav_item(frame, &mut state.app, View::Orders, Icon::Zap);
            nav_item(frame, &mut state.app, View::Backtest, Icon::BarChart);
            frame.separator();
            frame.label_compact_sized("SYSTEM", 11.0);
            nav_item(frame, &mut state.app, View::Config, Icon::Settings);
            frame.flex(1.0);
            card(frame, palette.elevated, 9.0, |frame| {
                frame.label_compact_sized("RUNTIME PROFILE", 10.0);
                frame.heading(state.app.broker_label(), 5);
                frame.label(&state.app.market_data_source);
                frame.label(&format!(
                    "{} topics · {} watches",
                    state.opinion_state.topics.len(),
                    state.opinion_state.watched_links.len()
                ));
            });
        },
    );
}

fn nav_item(frame: &mut Frame, state: &mut AppState, view: View, icon: Icon) {
    if frame.selectable_icon(icon, view.label(), state.view == view) {
        state.set_view(view);
    }
}

fn workspace(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    frame.column_ex(
        &LayoutOpts {
            flex: 1.0,
            gap: 0.0,
            pad: 0.0,
            bg: palette.page,
            ..LayoutOpts::default()
        },
        |frame| {
            frame.scroll("main-workspace", |frame| match state.app.view {
                View::Dashboard => dashboard_view(frame, state, palette),
                View::Intelligence => intelligence_view(frame, state, palette),
                View::Experts => experts_view(frame, state, palette),
                View::Market => market_view(frame, state, palette),
                View::Backtest => backtest_view(frame, state, palette),
                View::Portfolio => portfolio_view(frame, state, palette),
                View::Orders => orders_view(frame, state, palette),
                View::Config => config_view(frame, state, palette),
            });
        },
    );
}

fn dashboard_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    page_header(
        frame,
        "Decision cockpit",
        "Market state, public signals, expert dissent, and execution safety in one view.",
    );
    frame.row_ex(&card_row(), |frame| {
        metric_card(
            frame,
            "BROKER",
            state.app.broker_label(),
            state.app.mode.label(),
            palette.accent_tint,
        );
        metric_card(
            frame,
            "TOP SIGNALS",
            &state.opinion_state.items.len().to_string(),
            "ranked network results",
            palette.elevated,
        );
        let changed = state
            .opinion_state
            .watched_links
            .iter()
            .filter(|watch| watch.change == neenee_intelligence::LinkChange::Changed)
            .count();
        metric_card(
            frame,
            "LINK CHANGES",
            &changed.to_string(),
            "since the last observation",
            if changed > 0 {
                palette.warning_tint
            } else {
                palette.positive_tint
            },
        );
        let confidence = state
            .expert_meetings
            .first()
            .map(|meeting| format!("{:.0}%", meeting.conclusion.confidence * 100.0))
            .unwrap_or_else(|| "—".to_string());
        metric_card(
            frame,
            "COUNCIL CONFIDENCE",
            &confidence,
            "latest manager conclusion",
            palette.elevated,
        );
    });
    frame.row_ex(&card_row(), |frame| {
        card_with_flex(frame, palette.panel, 12.0, 1.4, |frame| {
            frame.heading("Network pulse", 4);
            frame.label(&state.intelligence_status);
            frame.separator();
            if state.opinion_state.items.is_empty() {
                frame.label(
                    "No signals collected yet. Refresh Intelligence to build a ranked brief.",
                );
            } else {
                for item in state.opinion_state.items.iter().take(4) {
                    frame.label_sized(&item.title, 14.0);
                    frame.label_compact_sized(
                        &format!("{} · score {:.0}", item.topic_label, item.score),
                        11.0,
                    );
                }
            }
            if frame.button("Open intelligence workspace") {
                state.app.set_view(View::Intelligence);
            }
        });
        card_with_flex(frame, palette.panel, 12.0, 1.0, |frame| {
            frame.heading("Expert manager", 4);
            frame.label(&state.expert_status);
            frame.separator();
            if let Some(meeting) = state.expert_meetings.first() {
                frame.label_sized(&meeting.topic, 14.0);
                frame.label(&meeting.conclusion.recommendation);
            } else {
                frame.label(
                    "No meeting archived. Ask a bounded decision question to convene the council.",
                );
            }
            if frame.button("Open expert council") {
                state.app.set_view(View::Experts);
            }
        });
    });
}

fn intelligence_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    page_header(
        frame,
        "Public intelligence",
        "Collect ranked web signals and continuously fingerprint sources you need to watch.",
    );
    card(frame, palette.panel, 12.0, |frame| {
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Topic label", &mut state.topic_label);
            frame.textfield("Search query", &mut state.topic_query);
            if frame.button("Add topic") {
                state.add_topic();
            }
            if frame.button(if state.intelligence_busy {
                "Refreshing…"
            } else {
                "Refresh network"
            }) {
                state.refresh_intelligence();
            }
        });
        frame.label(&state.intelligence_status);
        for error in state.opinion_state.refresh_errors.iter().take(3) {
            frame.label(&format!("Source warning · {error}"));
        }
    });
    frame.tabs("intelligence-tabs", &mut state.intelligence_tab, |frame| {
        frame.tab("Top signals");
        frame.tab("Watched links");
        frame.tab("Topics");
    });
    match state.intelligence_tab {
        1 => watched_links_panel(frame, state, palette),
        2 => topics_panel(frame, state, palette),
        _ => top_signals_panel(frame, state, palette),
    }
}

fn top_signals_panel(frame: &mut Frame, state: &GuiState, palette: Palette) {
    if state.opinion_state.items.is_empty() {
        empty_state(
            frame,
            palette.panel,
            "No ranked signals",
            "Run Refresh network. Search uses the configured neenee web backend and keeps the last good topic result if one source fails.",
        );
        return;
    }
    for item in state.opinion_state.items.iter().take(16) {
        frame.push_id(&item.id);
        card(frame, palette.panel, 10.0, |frame| {
            frame.row_ex(&form_row(), |frame| {
                status_pill(frame, &format!("{:.0}", item.score), palette.accent_tint);
                frame.column_ex(
                    &LayoutOpts {
                        flex: 1.0,
                        gap: 3.0,
                        ..LayoutOpts::default()
                    },
                    |frame| {
                        frame.heading(&item.title, 5);
                        frame.label_compact_sized(&item.topic_label, 11.0);
                        if !item.summary.is_empty() {
                            frame.label(&item.summary);
                        }
                        frame.label_compact_sized(&item.url, 10.0);
                    },
                );
            });
        });
        frame.pop_id();
    }
}

fn watched_links_panel(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    card(frame, palette.panel, 12.0, |frame| {
        frame.heading("Follow a source", 4);
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Label", &mut state.watch_label);
            frame.textfield("https://…", &mut state.watch_url);
            if frame.button("Watch link") {
                state.add_watch();
            }
        });
        frame.label("Each refresh sends ETag/Last-Modified validators when available and compares a SHA-256 body fingerprint otherwise.");
    });
    if state.opinion_state.watched_links.is_empty() {
        empty_state(
            frame,
            palette.elevated,
            "No watched links",
            "Add a release page, filing, policy document, or important article to establish a baseline.",
        );
        return;
    }
    for watch in &state.opinion_state.watched_links {
        let tint = match watch.change {
            neenee_intelligence::LinkChange::Changed => palette.warning_tint,
            neenee_intelligence::LinkChange::Error => palette.danger_tint,
            neenee_intelligence::LinkChange::New => palette.accent_tint,
            _ => palette.elevated,
        };
        card(frame, tint, 10.0, |frame| {
            frame.row_ex(&form_row(), |frame| {
                frame.icon(Icon::Link, 18.0);
                frame.column_ex(
                    &LayoutOpts {
                        flex: 1.0,
                        gap: 2.0,
                        ..LayoutOpts::default()
                    },
                    |frame| {
                        let label = if watch.label.is_empty() {
                            &watch.title
                        } else {
                            &watch.label
                        };
                        frame.heading(label, 5);
                        frame.label_compact_sized(&watch.url, 10.0);
                        if !watch.text_preview.is_empty() {
                            frame.label(&watch.text_preview);
                        }
                    },
                );
                status_pill(
                    frame,
                    &format!("{:?} · {} changes", watch.change, watch.change_count),
                    tint,
                );
            });
        });
    }
}

fn topics_panel(frame: &mut Frame, state: &GuiState, palette: Palette) {
    for topic in &state.opinion_state.topics {
        card(frame, palette.panel, 10.0, |frame| {
            frame.row_ex(&form_row(), |frame| {
                frame.icon(Icon::Search, 17.0);
                frame.column_ex(
                    &LayoutOpts {
                        flex: 1.0,
                        gap: 2.0,
                        ..LayoutOpts::default()
                    },
                    |frame| {
                        frame.heading(&topic.label, 5);
                        frame.label(&topic.query);
                    },
                );
                status_pill(
                    frame,
                    if topic.enabled { "ACTIVE" } else { "PAUSED" },
                    if topic.enabled {
                        palette.positive_tint
                    } else {
                        palette.subtle
                    },
                );
            });
        });
    }
}

fn experts_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    page_header(
        frame,
        "Expert council",
        "Five independent identities, a cross-examination round, and a non-voting meeting manager.",
    );
    card(frame, palette.panel, 12.0, |frame| {
        frame.row_ex(&form_row(), |frame| {
            frame.dropdown(
                "Scenario",
                &mut state.expert_scenario,
                &[
                    "Investment thesis",
                    "Market event",
                    "Trade risk",
                    "Strategy review",
                ],
            );
            frame.textfield("Decision question", &mut state.expert_topic);
        });
        frame.textarea("Evidence and context", &mut state.expert_context, 150.0);
        frame.row_ex(&form_row(), |frame| {
            if frame.button(if state.expert_busy {
                "Council in session…"
            } else {
                "Convene expert council"
            }) {
                state.convene_experts();
            }
            frame.label(&state.expert_status);
        });
        frame.label_compact_sized(
            "The council is advisory: 5 independent calls + 5 cross-examinations + 1 manager synthesis. It never submits an order.",
            11.0,
        );
    });
    let Some(meeting) = state.expert_meetings.first() else {
        empty_state(
            frame,
            palette.elevated,
            "No completed meeting",
            "Configure a neenee AI provider, enter a bounded question, and convene the council.",
        );
        return;
    };
    card(frame, palette.accent_tint, 12.0, |frame| {
        frame.label_compact_sized("MEETING MANAGER CONCLUSION", 11.0);
        frame.heading(&meeting.topic, 4);
        frame.label(&meeting.conclusion.recommendation);
        frame.progress("Manager confidence", meeting.conclusion.confidence);
        render_list(frame, "Consensus", &meeting.conclusion.consensus);
        render_list(frame, "Disagreements", &meeting.conclusion.disagreements);
        render_list(frame, "Next actions", &meeting.conclusion.actions);
        render_list(
            frame,
            "Stop conditions",
            &meeting.conclusion.stop_conditions,
        );
    });
    for contribution in meeting
        .contributions
        .iter()
        .filter(|contribution| contribution.round == 2)
    {
        let label = format!(
            "{} · {} · {:.0}%",
            contribution.expert_name,
            contribution.stance,
            contribution.confidence * 100.0
        );
        frame.collapsing_scoped(&contribution.expert_id, &label, |frame| {
            frame.label(&contribution.analysis);
            render_list(frame, "Risks", &contribution.risks);
            render_list(frame, "Evidence gaps", &contribution.evidence_gaps);
            render_list(frame, "Challenges", &contribution.challenges);
        });
    }
}

fn market_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    page_header(
        frame,
        "Market lens",
        "Quotes, candles, and depth from the active market-data adapter.",
    );
    card(frame, palette.panel, 12.0, |frame| {
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Symbol", &mut state.symbol);
            frame.dropdown(
                "Kind",
                &mut state.app.market_kind,
                &["quote", "klines", "depth"],
            );
            frame.textfield("Interval", &mut state.interval);
            if frame.button("Fetch market data") {
                state.sync_inputs();
                state.app.fetch_market_data();
            }
        });
    });
}

fn backtest_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    page_header(
        frame,
        "Strategy lab",
        "Test a bounded strategy hypothesis before it reaches an execution workflow.",
    );
    card(frame, palette.panel, 12.0, |frame| {
        frame.textfield("Strategy", &mut state.strategy);
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Symbol", &mut state.symbol);
            frame.textfield("Interval", &mut state.interval);
            frame.textfield("Start", &mut state.start);
            frame.textfield("End", &mut state.end);
        });
        frame.textfield("Initial capital", &mut state.capital);
        if frame.button("Run backtest") {
            state.sync_inputs();
            state.app.run_backtest();
        }
    });
}

fn portfolio_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    page_header(
        frame,
        "Portfolio",
        "Exposure, reservations, positions, and account capacity.",
    );
    frame.row_ex(&card_row(), |frame| {
        metric_card(
            frame,
            "ACCOUNT",
            &state.app.account_summary,
            "live runtime snapshot",
            palette.elevated,
        );
        metric_card(
            frame,
            "POSITIONS",
            &state.app.positions_summary,
            "current symbol filter",
            palette.elevated,
        );
    });
    card(frame, palette.panel, 12.0, |frame| {
        frame.label(&state.app.open_orders_summary);
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Symbol filter", &mut state.symbol);
            if frame.button("Refresh positions") {
                state.sync_inputs();
                state.app.refresh_positions();
            }
        });
    });
}

fn orders_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    page_header(
        frame,
        "Execution",
        "Order entry stays physically separated from intelligence and expert advice.",
    );
    card(
        frame,
        if state.app.mode == TradingMode::TradingArmed {
            palette.danger_tint
        } else {
            palette.warning_tint
        },
        12.0,
        |frame| {
            frame.heading(state.app.mode.label(), 4);
            frame.label("Account mutation is blocked until the top-bar trading control is explicitly armed.");
            frame.label(&state.app.recent_order_summary);
            frame.label(&state.app.open_orders_summary);
        },
    );
    card(frame, palette.panel, 12.0, |frame| {
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Symbol", &mut state.symbol);
            frame.dropdown("Side", &mut state.app.order_side, &["buy", "sell"]);
            frame.dropdown("Type", &mut state.app.order_type, &["market", "limit"]);
        });
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Quantity", &mut state.quantity);
            frame.textfield("Limit price", &mut state.price);
        });
        if frame.button("Submit order") {
            state.sync_inputs();
            state.app.submit_order();
            state.order_id.set(&state.app.order_id);
        }
        frame.separator();
        frame.row_ex(&form_row(), |frame| {
            frame.textfield("Order id", &mut state.order_id);
            if frame.button("Cancel order") {
                state.sync_inputs();
                state.app.cancel_order();
            }
        });
    });
}

fn config_view(frame: &mut Frame, state: &mut GuiState, palette: Palette) {
    let config = &state.app.config;
    page_header(
        frame,
        "Runtime configuration",
        "Read-only effective settings for trading, intelligence, and safety.",
    );
    frame.row_ex(&card_row(), |frame| {
        card_with_flex(frame, palette.panel, 12.0, 1.0, |frame| {
            frame.heading("Trading runtime", 4);
            frame.label(&format!("Market data · {}", state.app.market_data_source));
            frame.label(&format!("Broker · {}", config.broker.mode));
            frame.label(&format!("LongPort auth · {}", config.longport.auth_mode));
            frame.label(&format!(
                "Account currency · {}",
                config
                    .longport
                    .account_currency
                    .as_deref()
                    .unwrap_or("account default")
            ));
            frame.label(&format!("Paper cash · {}", config.paper.starting_cash));
            frame.label(&format!("Commission bps · {}", config.paper.commission_bps));
        });
        card_with_flex(frame, palette.panel, 12.0, 1.0, |frame| {
            frame.heading("Risk boundary", 4);
            frame.label(&format!(
                "Max order · {}",
                config.paper.risk.max_order_notional
            ));
            frame.label(&format!(
                "Max exposure · {}",
                config.paper.risk.max_gross_exposure
            ));
            frame.label(&format!(
                "Short selling · {}",
                config.paper.risk.allow_short_selling
            ));
            frame.label(&format!(
                "Audit · {}",
                config
                    .paper
                    .audit_log
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "disabled".to_string())
            ));
            frame.label("Expert conclusions are advisory and have no order adapter.");
        });
    });
    card(frame, palette.elevated, 10.0, |frame| {
        frame.label(&state.app.config_summary);
    });
}

fn inspector(frame: &mut Frame, state: &GuiState, palette: Palette) {
    const WIDTH: f32 = 318.0;
    const PADDING: f32 = 10.0;
    const CARD_PADDING: f32 = 12.0;
    const LABEL_WIDTH: f32 = WIDTH - 2.0 * PADDING;
    const CARD_LABEL_WIDTH: f32 = LABEL_WIDTH - 2.0 * CARD_PADDING;

    frame.column_ex(
        &LayoutOpts {
            width: WIDTH,
            gap: 10.0,
            pad: PADDING,
            bg: palette.panel,
            radius: 12.0,
            ..LayoutOpts::default()
        },
        |frame| {
            frame.label_compact_sized("CONTROL PLANE", 11.0);
            card(frame, palette.elevated, 9.0, |frame| {
                frame.heading("Safety", 5);
                frame.label_wrapped(&state.app.risk_status, CARD_LABEL_WIDTH);
                frame.label_wrapped(&state.app.open_orders_summary, CARD_LABEL_WIDTH);
            });
            card(frame, palette.elevated, 9.0, |frame| {
                frame.heading("Last action", 5);
                frame.label_wrapped(&state.app.last_action, CARD_LABEL_WIDTH);
                frame.label_wrapped(&state.app.account_summary, CARD_LABEL_WIDTH);
            });
            frame.label_compact_sized("OUTPUT", 11.0);
            frame.size_next(0.0, 380.0);
            frame.scroll("result-scroll", |frame| {
                frame.column_ex(
                    &LayoutOpts {
                        gap: 4.0,
                        ..LayoutOpts::default()
                    },
                    |frame| {
                        for line in state.app.last_result.lines().take(100) {
                            frame.label_wrapped(line, LABEL_WIDTH);
                        }
                    },
                );
            });
        },
    );
}

fn page_header(frame: &mut Frame, title: &str, subtitle: &str) {
    frame.column_ex(
        &LayoutOpts {
            gap: 3.0,
            pad: 4.0,
            ..LayoutOpts::default()
        },
        |frame| {
            frame.heading(title, 2);
            frame.label(subtitle);
        },
    );
}

fn card(frame: &mut Frame, background: Color, radius: f32, body: impl FnOnce(&mut Frame)) {
    frame.column_ex(
        &LayoutOpts {
            gap: 8.0,
            pad: 12.0,
            bg: background,
            radius,
            ..LayoutOpts::default()
        },
        body,
    );
}

fn card_with_flex(
    frame: &mut Frame,
    background: Color,
    radius: f32,
    flex: f32,
    body: impl FnOnce(&mut Frame),
) {
    frame.column_ex(
        &LayoutOpts {
            flex,
            gap: 8.0,
            pad: 12.0,
            bg: background,
            radius,
            ..LayoutOpts::default()
        },
        body,
    );
}

fn metric_card(frame: &mut Frame, label: &str, value: &str, caption: &str, background: Color) {
    card_with_flex(frame, background, 12.0, 1.0, |frame| {
        frame.label_compact_sized(label, 10.0);
        frame.heading(value, 4);
        frame.label_compact_sized(caption, 11.0);
    });
}

fn empty_state(frame: &mut Frame, background: Color, title: &str, detail: &str) {
    card(frame, background, 12.0, |frame| {
        frame.icon(Icon::Database, 24.0);
        frame.heading(title, 4);
        frame.label(detail);
    });
}

fn render_list(frame: &mut Frame, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    frame.label_compact_sized(title, 11.0);
    for item in items.iter().take(8) {
        frame.label(&format!("• {item}"));
    }
}

fn form_row() -> LayoutOpts {
    LayoutOpts {
        gap: 10.0,
        cross: Align::Center,
        ..LayoutOpts::default()
    }
}

fn card_row() -> LayoutOpts {
    LayoutOpts {
        gap: 10.0,
        cross: Align::Stretch,
        ..LayoutOpts::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn launch_profiles_have_explicit_paper_and_live_entries() {
        assert_eq!(
            parse_launch_action(arguments(&["--paper"])),
            Ok(LaunchAction::Run(LaunchProfile::Paper))
        );
        assert_eq!(
            parse_launch_action(arguments(&["--profile", "longport-live"])),
            Ok(LaunchAction::Run(LaunchProfile::LongportLive))
        );
        assert_eq!(
            parse_launch_action(Vec::new()),
            Ok(LaunchAction::Run(LaunchProfile::Environment))
        );
    }

    #[test]
    fn paper_profile_overrides_live_environment_selection() {
        let mut config = neenee_quant::QuantConfig::default();
        config.market_data.source = "longport".to_string();
        config.broker.mode = "longport".to_string();

        LaunchProfile::Paper.apply(&mut config);

        assert_eq!(config.market_data.source, "synthetic");
        assert_eq!(config.broker.mode, "paper");
    }

    #[test]
    fn conflicting_or_unknown_profiles_are_rejected() {
        assert!(parse_launch_action(arguments(&["--paper", "--longport-live"])).is_err());
        assert!(parse_launch_action(arguments(&["--profile", "unknown"])).is_err());
        assert_eq!(
            parse_launch_action(arguments(&["--help"])),
            Ok(LaunchAction::Help)
        );
    }
}
