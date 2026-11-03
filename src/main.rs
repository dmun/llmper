use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mlua::{Function, Lua, RegistryKey, Table, Value};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tui_textarea::TextArea;

type NativeRender = fn(&App, Rect, &mut Frame);
type NativeKey = fn(&mut App, KeyEvent) -> bool;

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

// A component is a render spec plus an optional key handler. `render` is either
// the name of a built-in renderer or a Lua function returning a string to draw.
// `key` is an optional Lua function: return true to swallow a key, false to
// fall through to the built-in handler for the renderer.
struct Component {
    render: Render,
    key: Option<RegistryKey>,
}

enum Render {
    Native(String),
    Lua(RegistryKey),
}

struct Ui {
    main: Component,
    input: Component,
    status: Option<Component>,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            main: Component {
                render: Render::Native("transcript".into()),
                key: None,
            },
            input: Component {
                render: Render::Native("textarea".into()),
                key: None,
            },
            status: Some(Component {
                render: Render::Native("status".into()),
                key: None,
            }),
        }
    }
}

impl Ui {
    fn load(lua: &Lua) -> mlua::Result<Self> {
        let ui: Option<Table> = lua.globals().get("ui")?;
        let Some(ui) = ui else {
            return Ok(Ui::default());
        };
        Ok(Ui {
            main: parse_component(lua, &ui, "main", "transcript")?,
            input: parse_component(lua, &ui, "input", "textarea")?,
            status: if ui.contains_key("status")? {
                Some(parse_component(lua, &ui, "status", "status")?)
            } else {
                None
            },
        })
    }
}

fn parse_component(lua: &Lua, ui: &Table, slot: &str, default: &str) -> mlua::Result<Component> {
    let Some(spec) = ui.get::<Option<Table>>(slot)? else {
        return Ok(Component {
            render: Render::Native(default.into()),
            key: None,
        });
    };
    let render = match spec.get::<Value>("render")? {
        Value::Nil => Render::Native(default.into()),
        Value::String(s) => Render::Native(s.to_str()?.to_string()),
        Value::Function(f) => Render::Lua(lua.create_registry_value(f)?),
        _ => {
            return Err(mlua::Error::RuntimeError(format!(
                "ui.{slot}.render must be a string or function"
            )))
        }
    };
    let key = match spec.get::<Value>("key")? {
        Value::Nil => None,
        Value::Function(f) => Some(lua.create_registry_value(f)?),
        _ => {
            return Err(mlua::Error::RuntimeError(format!(
                "ui.{slot}.key must be a function"
            )))
        }
    };
    Ok(Component { render, key })
}

struct App {
    lua: Lua,
    config: Config,
    client: Client,
    llm_tx: mpsc::UnboundedSender<Message>,
    history: Vec<Message>,
    waiting: bool,
    textarea: TextArea<'static>,
    ui: Ui,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lua = Lua::new();
    if std::path::Path::new("init.lua").exists() {
        lua.load(&std::fs::read_to_string("init.lua")?).exec()?;
    }
    let config = Config::load(&lua)?;
    let ui = Ui::load(&lua)?;
    let client = Client::new();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(lua, config, ui, client))
}

async fn run(
    lua: Lua,
    config: Config,
    ui: Ui,
    client: Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = ratatui::init();

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if ev_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let (llm_tx, mut llm_rx) = mpsc::unbounded_channel();
    let mut app = App {
        lua,
        config,
        client,
        llm_tx,
        history: Vec::new(),
        waiting: false,
        textarea: TextArea::default(),
        ui,
    };

    let mut tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            ev = ev_rx.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if key.code == KeyCode::Esc { break; }
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            break;
                        }
                        handle_key(&mut app, key);
                    }
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

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    // Lua key handler first (augment): true swallows, false falls through.
    if let Some(rk) = &app.ui.input.key {
        let s = key_to_string(key);
        let handled = app
            .lua
            .registry_value::<Function>(rk)
            .and_then(|f| f.call::<bool>(s))
            .unwrap_or(false);
        if handled {
            return true;
        }
    }
    // Built-in handler for the input's renderer.
    let name = match &app.ui.input.render {
        Render::Native(n) => n.clone(),
        Render::Lua(_) => return false,
    };
    match native_key(&name) {
        Some(h) => h(app, key),
        None => false,
    }
}

fn submit(app: &mut App) {
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

    let client = app.client.clone();
    let config = app.config.clone();
    let history = app.history.clone();
    let tx = app.llm_tx.clone();
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
    let area = frame.area();
    let input_h = 3u16;
    let status_h = if app.ui.status.is_some() { 1 } else { 0 };
    let main_h = area.height.saturating_sub(input_h + status_h);

    let main_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: main_h,
    };
    let input_y = area.y + main_h;
    let input_area = Rect {
        x: area.x,
        y: input_y,
        width: area.width,
        height: input_h.min(area.height),
    };
    let status_area = Rect {
        x: area.x,
        y: input_y + input_h,
        width: area.width,
        height: status_h,
    };

    render_component(frame, app, &app.ui.main, main_area);
    render_component(frame, app, &app.ui.input, input_area);
    if let Some(status) = &app.ui.status {
        render_component(frame, app, status, status_area);
    }
}

fn render_component(frame: &mut Frame, app: &App, comp: &Component, area: Rect) {
    match &comp.render {
        Render::Native(name) => match native_render(name) {
            Some(f) => f(app, area, frame),
            None => eprintln!("unknown renderer: {name}"),
        },
        Render::Lua(rk) => {
            if let Ok(f) = app.lua.registry_value::<Function>(rk) {
                if let Ok(state) = lua_state_table(&app.lua, app) {
                    if let Ok(Some(text)) =
                        f.call::<Option<String>>((state, area.width, area.height))
                    {
                        frame.render_widget(Paragraph::new(text), area);
                    }
                }
            }
        }
    }
}

fn lua_state_table(lua: &Lua, app: &App) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("model", app.config.model.clone())?;
    t.set("waiting", app.waiting)?;
    t.set("messages", app.history.len())?;
    Ok(t)
}

fn native_render(name: &str) -> Option<NativeRender> {
    match name {
        "transcript" => Some(render_transcript),
        "textarea" => Some(render_textarea),
        "status" => Some(render_status),
        _ => None,
    }
}

fn native_key(name: &str) -> Option<NativeKey> {
    match name {
        "textarea" => Some(key_textarea),
        _ => None,
    }
}

fn render_transcript(app: &App, area: Rect, frame: &mut Frame) {
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
    let scroll = text
        .lines()
        .count()
        .saturating_sub(area.height as usize) as u16;
    let para = Paragraph::new(text).block(Block::bordered().title("llemper"));
    frame.render_widget(para.scroll((scroll, 0)), area);
}

fn render_textarea(app: &App, area: Rect, frame: &mut Frame) {
    let mut ta = app.textarea.clone();
    ta.set_block(Block::bordered().title("Enter: send | Esc: quit"));
    frame.render_widget(&ta, area);
}

fn render_status(app: &App, area: Rect, frame: &mut Frame) {
    let text = format!(
        "{} | {} msgs{}",
        app.config.model,
        app.history.len(),
        if app.waiting { " | …" } else { "" }
    );
    frame.render_widget(Paragraph::new(text), area);
}

fn key_textarea(app: &mut App, key: KeyEvent) -> bool {
    if key.code == KeyCode::Enter {
        submit(app);
        return true;
    }
    app.textarea.input(Event::Key(key));
    true
}

fn key_to_string(key: KeyEvent) -> String {
    let mut s = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        s.push_str("ctrl-");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        s.push_str("alt-");
    }
    match key.code {
        KeyCode::Enter => s.push_str("enter"),
        KeyCode::Esc => s.push_str("esc"),
        KeyCode::Backspace => s.push_str("backspace"),
        KeyCode::Tab => s.push_str("tab"),
        KeyCode::Up => s.push_str("up"),
        KeyCode::Down => s.push_str("down"),
        KeyCode::Left => s.push_str("left"),
        KeyCode::Right => s.push_str("right"),
        KeyCode::Char(c) => s.push(c),
        KeyCode::F(n) => s.push_str(&format!("f{n}")),
        other => s.push_str(&format!("{other:?}")),
    }
    s
}
