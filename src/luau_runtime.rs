use mlua::{Lua, MultiValue, Table, Value, VmState, chunk::ChunkMode};
use serde_json::{Value as Json, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
    time::{Duration, Instant},
};

const MAX_INPUT: usize = 4 * 1024 * 1024;
const MAX_EVENTS: usize = 2_048;
const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CAPTURES: usize = 64;
const CAPTURE_STOP: &str = "SLATT stopped after expression capture";
pub(crate) const DEFAULT_TIMEOUT_MS: i64 = 3_000;
const BASE_GLOBALS: &[&str] = &[
    "_VERSION",
    "assert",
    "bit32",
    "buffer",
    "coroutine",
    "debug",
    "error",
    "gcinfo",
    "getfenv",
    "getmetatable",
    "ipairs",
    "math",
    "newproxy",
    "next",
    "os",
    "pairs",
    "pcall",
    "rawequal",
    "rawget",
    "rawlen",
    "rawset",
    "select",
    "setfenv",
    "setmetatable",
    "string",
    "table",
    "tonumber",
    "tostring",
    "type",
    "typeof",
    "unpack",
    "utf8",
    "vector",
    "xpcall",
];

#[derive(Default)]
struct Trace {
    events: Vec<Json>,
    dropped: usize,
    lookups: HashMap<(String, Option<usize>, bool), usize>,
    captures: BTreeMap<String, Vec<u8>>,
    capture_bytes: usize,
    expression_captured: bool,
}

impl Trace {
    fn push(&mut self, event: Json) {
        if self.events.len() == MAX_EVENTS {
            self.dropped += 1;
        } else {
            self.events.push(event);
        }
    }

    fn lookup(&mut self, key: &Value, line: Option<usize>, available: bool) {
        let label = key_label(key);
        let Some(name) = label.as_str() else {
            self.push(
                json!({"event":"global_lookup","key":label,"available":available,"line":line}),
            );
            return;
        };
        let group = (name.to_owned(), line, available);
        if let Some(&index) = self.lookups.get(&group) {
            let count = self.events[index]["count"].as_u64().unwrap_or(1);
            self.events[index]["count"] = json!(count.saturating_add(1));
            return;
        }
        if self.events.len() < MAX_EVENTS {
            self.lookups.insert(group, self.events.len());
        }
        self.push(json!({"event":"global_lookup","key":label,"available":available,"line":line,"count":1}));
    }

    fn capture(&mut self, label: String, value: Value) -> mlua::Result<()> {
        if label.is_empty()
            || label.len() > 64
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(mlua::Error::runtime("invalid Slatt capture label"));
        }
        if self.captures.contains_key(&label) {
            return Err(mlua::Error::runtime(format!(
                "duplicate Slatt capture {label}"
            )));
        }
        let len = match &value {
            Value::String(data) => data.as_bytes().len(),
            Value::Buffer(data) => data.len(),
            _ => {
                return Err(mlua::Error::runtime(
                    "Slatt capture requires a string or buffer",
                ));
            }
        };
        if self.captures.len() == MAX_CAPTURES || len > MAX_CAPTURE_BYTES - self.capture_bytes {
            return Err(mlua::Error::runtime("Slatt capture limit exceeded"));
        }
        let bytes = match value {
            Value::String(data) => data.as_bytes().to_vec(),
            Value::Buffer(data) => data.to_vec(),
            _ => unreachable!("capture type already checked"),
        };
        self.capture_bytes += len;
        self.captures.insert(label, bytes);
        Ok(())
    }
}

fn line(lua: &Lua) -> Option<usize> {
    (0..=3).find_map(|level| {
        lua.inspect_stack(level, |frame| frame.current_line())
            .flatten()
    })
}

struct SnapshotBudget {
    values: usize,
    bytes: usize,
}

fn value(value: Value, depth: usize, budget: &mut SnapshotBudget) -> mlua::Result<Json> {
    if budget.values == 0 {
        return Ok(json!({"type":"omitted"}));
    }
    budget.values -= 1;
    Ok(match value {
        Value::Nil => json!({"type":"nil"}),
        Value::Boolean(v) => json!({"type":"boolean","value":v}),
        Value::Integer(v) => json!({"type":"integer","value":v}),
        Value::Number(v) if v.is_finite() => json!({"type":"number","value":v}),
        Value::Number(v) => json!({"type":"number","bits":format!("{:016x}", v.to_bits())}),
        Value::String(v) if v.as_bytes().len() <= budget.bytes => {
            let bytes = v.as_bytes();
            budget.bytes -= bytes.len();
            match std::str::from_utf8(&bytes) {
                Ok(text) => json!({"type":"string","value":text}),
                Err(_) => json!({"type":"bytes","value":bytes.as_ref()}),
            }
        }
        Value::String(v) => json!({"type":"string","length":v.as_bytes().len(),"truncated":true}),
        Value::Table(table) if depth < 4 => {
            let mut entries = Vec::new();
            for pair in table.pairs::<Value, Value>() {
                if budget.values < 2 {
                    break;
                }
                let (key, item) = pair?;
                entries.push([
                    self::value(key, depth + 1, budget)?,
                    self::value(item, depth + 1, budget)?,
                ]);
            }
            entries.sort_by_cached_key(|pair| pair[0].to_string());
            json!({"type":"table","entries":entries,"truncated":budget.values < 2})
        }
        v => json!({"type":v.type_name()}),
    })
}

fn tuple(values: MultiValue, count: usize, bytes: usize) -> mlua::Result<Vec<Json>> {
    let mut budget = SnapshotBudget {
        values: count,
        bytes,
    };
    let length = values.len();
    values
        .into_iter()
        .take(count)
        .map(|v| value(v, 0, &mut budget))
        .chain((length > count).then(|| Ok(json!({"type":"omitted","count":length-count}))))
        .collect()
}

fn key_label(key: &Value) -> Json {
    match key {
        Value::String(s) if s.as_bytes().len() <= 256 => {
            let bytes = s.as_bytes();
            match std::str::from_utf8(&bytes) {
                Ok(text) => json!(text),
                Err(_) => json!({"type":"bytes","value":bytes.as_ref()}),
            }
        }
        v => json!({"type":v.type_name()}),
    }
}

fn environment(
    lua: &Lua,
    base: Table,
    fixture: Table,
    trace: Rc<RefCell<Trace>>,
) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    env.raw_set("_G", env.clone())?;
    let print_trace = trace.clone();
    let print = lua.create_function(move |lua, args: MultiValue| {
        let mut trace = print_trace.borrow_mut();
        if trace.events.len() < MAX_EVENTS {
            trace.push(json!({"event":"print","line":line(lua),"args":tuple(args, 64, 2_048)?}));
        } else {
            trace.dropped += 1;
        }
        Ok(())
    })?;
    let probe_trace = trace.clone();
    env.raw_set("__slatt_probe", lua.create_function(move |lua, (label, values): (Value, MultiValue)| {
        let mut trace = probe_trace.borrow_mut();
        if trace.events.len() < MAX_EVENTS {
            trace.push(json!({"event":"probe","line":line(lua),"label":key_label(&label),"args":tuple(values, 128, 2_048)?}));
        } else {
            trace.dropped += 1;
        }
        Ok(())
    })?)?;
    let capture_trace = trace.clone();
    env.raw_set(
        "__slatt_dump",
        lua.create_function(move |_, (label, value): (mlua::LuaString, Value)| {
            if label.as_bytes().len() > 64 {
                return Err(mlua::Error::runtime("invalid Slatt capture label"));
            }
            capture_trace
                .borrow_mut()
                .capture(label.to_string_lossy(), value)
        })?,
    )?;
    let meta = lua.create_table()?;
    let read_trace = trace.clone();
    meta.raw_set(
        "__index",
        lua.create_function(move |lua, (_, key): (Table, Value)| {
            let mut resolved: Value = fixture.raw_get(key.clone())?;
            if resolved.is_nil() {
                resolved = base.raw_get(key.clone())?;
            }
            if resolved.is_nil()
                && matches!(&key, Value::String(name) if name.as_bytes().as_ref() == b"print")
            {
                resolved = Value::Function(print.clone());
            }
            read_trace
                .borrow_mut()
                .lookup(&key, line(lua), !resolved.is_nil());
            Ok(resolved)
        })?,
    )?;
    meta.raw_set(
        "__newindex",
        lua.create_function(move |lua, (table, key, item): (Table, Value, Value)| {
            trace
                .borrow_mut()
                .push(json!({"event":"global_insert","key":key_label(&key),"line":line(lua)}));
            table.raw_set(key, item)
        })?,
    )?;
    env.set_metatable(Some(meta))?;
    Ok(env)
}

pub struct Capture {
    pub report: Json,
    pub artifacts: BTreeMap<String, Vec<u8>>,
    expression_captured: bool,
}

pub fn trace(source: &str, fixture: &str, name: &str) -> Result<Json, String> {
    Ok(capture(source, fixture, name)?.report)
}

pub fn capture(source: &str, fixture: &str, name: &str) -> Result<Capture, String> {
    run_capture(source, fixture, name, None, DEFAULT_TIMEOUT_MS)
}

pub fn capture_expression(
    source: &str,
    begin: i64,
    end: i64,
    fixture: &str,
    label: &str,
    timeout_ms: i64,
) -> Result<Vec<u8>, String> {
    let begin = usize::try_from(begin).map_err(|_| "invalid expression byte span")?;
    let end = usize::try_from(end).map_err(|_| "invalid expression byte span")?;
    let expression = source
        .get(begin..end)
        .filter(|value| !value.is_empty())
        .ok_or("invalid expression byte span")?;
    if source.contains("__slatt_capture_stop") {
        return Err("source uses the reserved expression capture function".into());
    }
    let wrapped = format!(
        "{} __slatt_capture_stop(({expression})) {}",
        &source[..begin],
        &source[end..]
    );
    let mut run = run_capture(
        &wrapped,
        fixture,
        "=capture expression",
        Some(label),
        timeout_ms,
    )?;
    if run.expression_captured
        && run.report["error"]
            .as_str()
            .is_some_and(|msg| msg.contains(CAPTURE_STOP))
        && let Some(bytes) = run.artifacts.remove(label)
    {
        return Ok(bytes);
    }
    Err(format!(
        "expression capture did not stop at {label:?}: {}",
        run.report["error"]
    ))
}

fn run_capture(
    source: &str,
    fixture: &str,
    name: &str,
    stop_label: Option<&str>,
    timeout_ms: i64,
) -> Result<Capture, String> {
    if source.len() > MAX_INPUT || fixture.len() > MAX_INPUT {
        return Err("Luau trace input exceeds 4 MiB".into());
    }
    if !(1..=30_000).contains(&timeout_ms) {
        return Err("Luau capture timeout must be 1–30,000 ms".into());
    }
    let lua = Lua::new();
    let map_error = |error: mlua::Error| error.to_string();
    lua.set_memory_limit(64 * 1024 * 1024).map_err(map_error)?;
    // This is Lua's dedicated state. Remove module loaders and all host bindings.
    let globals = lua.globals();
    let keys = globals
        .pairs::<String, Value>()
        .map(|p| p.map(|(key, _)| key))
        .collect::<mlua::Result<Vec<_>>>()
        .map_err(map_error)?;
    for key in keys {
        if !BASE_GLOBALS.contains(&key.as_str()) {
            globals.raw_set(key, Value::Nil).map_err(map_error)?;
        }
    }
    lua.sandbox(true).map_err(map_error)?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    lua.set_interrupt(move |_| {
        if Instant::now() >= deadline {
            Err(mlua::Error::runtime("SLATT Luau trace time limit exceeded"))
        } else {
            Ok(VmState::Continue)
        }
    });
    let trace = Rc::new(RefCell::new(Trace::default()));
    let bindings = lua.create_table().map_err(map_error)?;
    let env = environment(&lua, globals, bindings.clone(), trace.clone()).map_err(map_error)?;
    if let Some(label) = stop_label {
        let label = label.to_owned();
        let captures = trace.clone();
        let stop = lua
            .create_function(move |_, value: Value| -> mlua::Result<()> {
                let mut captures = captures.borrow_mut();
                captures.capture(label.clone(), value)?;
                captures.expression_captured = true;
                Err(mlua::Error::runtime(CAPTURE_STOP))
            })
            .map_err(map_error)?;
        env.raw_set("__slatt_capture_stop", stop)
            .map_err(map_error)?;
    }
    if !fixture.trim().is_empty() {
        let provided = lua
            .load(fixture)
            .set_name("=fixture")
            .set_mode(ChunkMode::Text)
            .set_environment(env.clone())
            .eval::<Table>()
            .map_err(|error| format!("Luau fixture: {error}"))?;
        for pair in provided.pairs::<Value, Value>() {
            let (key, value) = pair.map_err(map_error)?;
            bindings.raw_set(key, value).map_err(map_error)?;
        }
    }
    let result = lua
        .load(source)
        .set_name(name)
        .set_mode(ChunkMode::Text)
        .set_environment(env)
        .eval::<MultiValue>();
    let mut trace = trace.borrow_mut();
    let (returns, error) = match result {
        Ok(values) => (tuple(values, 1_024, 512 * 1024).map_err(map_error)?, None),
        Err(error) => {
            let mut message = error.to_string();
            if message.len() > 8_192 {
                message.truncate(message.floor_char_boundary(8_192));
                message.push_str(" [truncated]");
            }
            (Vec::new(), Some(message))
        }
    };
    let artifacts = std::mem::take(&mut trace.captures);
    let captures: Vec<_> = artifacts
        .iter()
        .map(|(label, bytes)| json!({"label":label,"size":bytes.len()}))
        .collect();
    let events = std::mem::take(&mut trace.events);
    let report = json!({"ok":error.is_none(),"error":error,"returns":returns,
        "events":events,"dropped_events":trace.dropped,"captures":captures});
    Ok(Capture {
        report,
        artifacts,
        expression_captured: trace.expression_captured,
    })
}

fn observations(run: &Capture) -> Result<Json, String> {
    let report = &run.report;
    if report["ok"] != true {
        return Err(report["error"].as_str().unwrap_or("runtime failed").into());
    }
    if report["dropped_events"] != 0 {
        return Err("trace dropped observations".into());
    }
    let observable = |value: &Json| {
        matches!(
            value["type"].as_str(),
            Some("nil" | "boolean" | "integer" | "number" | "string" | "bytes")
        ) && value["truncated"] != true
    };
    let returns = report["returns"].as_array().ok_or("missing return tuple")?;
    if !returns.iter().all(observable) {
        return Err("return tuple contains opaque or truncated values".into());
    }
    let mut effects = Vec::new();
    for event in report["events"].as_array().ok_or("missing trace events")? {
        if event["event"] == "global_insert" {
            return Err("replay cannot certify global writes".into());
        }
        if matches!(event["event"].as_str(), Some("print" | "probe")) {
            if event["event"] == "probe"
                && !event["label"].is_string()
                && event["label"]["type"] != "bytes"
            {
                return Err("probe has an opaque or oversized label".into());
            }
            let args = event["args"].as_array().ok_or("missing event tuple")?;
            if !args.iter().all(observable) {
                return Err("event contains opaque or truncated values".into());
            }
            effects.push(json!({"event":event["event"],"label":event["label"],"args":args}));
        }
    }
    Ok(json!({"returns":returns,"effects":effects}))
}

pub fn replay(original: &str, recovered: &str, fixture: &str) -> Result<Json, String> {
    let left = capture(original, fixture, "=original")?;
    let right = capture(recovered, fixture, "=recovered")?;
    let original = observations(&left);
    let recovered = observations(&right);
    Ok(match (original, recovered) {
        (Ok(original), Ok(recovered)) => {
            let observations_match = original == recovered;
            let captures_match = left.artifacts == right.artifacts;
            json!({"matched":observations_match && captures_match,
                "original":original,"recovered":recovered,"captures_match":captures_match})
        }
        (original, recovered) => {
            json!({"matched":false,"original_error":original.err(),"recovered_error":recovered.err()})
        }
    })
}
