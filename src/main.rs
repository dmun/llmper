use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use mlua::Lua;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

#[derive(Clone, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Clone)]
struct Config {
    url: String,
    key: Option<String>,
    model: String,
    system: String,
}

impl Config {
    fn load(lua: &Lua) -> mlua::Result<Self> {
        let g = lua.globals();
        let str = |name: &str, default: &str| -> mlua::Result<String> {
            Ok(g.get::<Option<String>>(name)?.unwrap_or_else(|| default.to_string()))
        };
        let key = g.get::<Option<String>>("llm_api_key")?.unwrap_or_default();
        Ok(Config {
            url: str("llm_url", "https://api.deepseek.com/chat/completions")?,
            key: if key.is_empty() { None } else { Some(key) },
            model: str("llm_model", "deepseek-chat")?,
            system: str("llm_system", "You are a helpful assistant.")?,
        })
    }
}

struct App {
    textarea: TextArea<'static>,
    history: Vec<Message>,
    waiting: bool,
}

impl App {
    fn new() -> Self {
        Self {
            textarea: TextArea::default(),
            history: Vec::new(),
            waiting: false,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lua = Lua::new();
    if std::path::Path::new("init.lua").exists() {
        lua.load(&std::fs::read_to_string("init.lua")?).exec()?;
    }
    let config = Config::load(&lua)?;
    let client = Client::new();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(config, client))
}

async fn run(config: Config, client: Client) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = ratatui::init();
    let mut app = App::new();

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if ev_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let (llm_tx, mut llm_rx) = mpsc::unbounded_channel();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            ev = ev_rx.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Enter => submit(&mut app, &client, &config, &llm_tx),
                        _ => {
                            app.textarea.input(Event::Key(key));
                        }
                    },
                    _ => {}
                }
            }
            msg = llm_rx.recv() => {
                if let Some(m) = msg {
                    app.history.push(m);
                    app.waiting = false;
                }
            }
            _ = tick.tick() => {}
        }
        terminal.draw(|f| draw(f, &mut app))?;
    }

    ratatui::restore();
    Ok(())
}

fn submit(app: &mut App, client: &Client, config: &Config, tx: &mpsc::UnboundedSender<Message>) {
    let text = app.textarea.lines().join("\n");
    if text.trim().is_empty() || app.waiting {
        return;
    }
    app.textarea = TextArea::default();
    let msg = Message {
        role: "user".to_string(),
        content: text,
    };
    app.history.push(msg.clone());
    app.waiting = true;

    let client = client.clone();
    let config = config.clone();
    let history = app.history.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let reply = chat(&client, &config, &history).await;
        let _ = tx.send(reply);
    });
}

async fn chat(client: &Client, cfg: &Config, history: &[Message]) -> Message {
    let mut messages = Vec::with_capacity(history.len() + 1);
    if !cfg.system.is_empty() {
        messages.push(Message {
            role: "system".to_string(),
            content: cfg.system.clone(),
        });
    }
    messages.extend_from_slice(history);

    let body = ChatRequest {
        model: cfg.model.clone(),
        messages,
    };
    let mut req = client.post(&cfg.url).json(&body);
    if let Some(key) = &cfg.key {
        req = req.bearer_auth(key);
    }
    match req.send().await {
        Ok(resp) => match resp.json::<ChatResponse>().await {
            Ok(r) => r
                .choices
                .into_iter()
                .next()
                .map(|c| c.message)
                .unwrap_or_else(|| err_msg("empty response")),
            Err(e) => err_msg(&format!("bad response: {e}")),
        },
        Err(e) => err_msg(&format!("request failed: {e}")),
    }
}

fn err_msg(s: &str) -> Message {
    Message {
        role: "assistant".to_string(),
        content: format!("[error] {s}"),
    }
}

fn draw(frame: &mut Frame, app: &mut App) {
    let [hist_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());

    let mut text = String::new();
    for m in &app.history {
        let name = match m.role.as_str() {
            "user" => "You",
            "assistant" => "Assistant",
            "system" => "System",
            other => other,
        };
        text.push_str(&format!("{name}: {}\n", m.content));
    }
    if app.waiting {
        text.push_str("\nAssistant: …");
    }

    let scroll = text.lines().count().saturating_sub(hist_area.height as usize) as u16;
    let para = Paragraph::new(text).block(Block::bordered().title("llemper"));
    frame.render_widget(para.scroll((scroll, 0)), hist_area);

    app.textarea
        .set_block(Block::bordered().title("Enter: send | Esc: quit"));
    frame.render_widget(&app.textarea, input_area);
}
