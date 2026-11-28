//! Planning a host's settings with LangSpan's hook entries replaced or
//! removed, and every other entry kept where it was.

use serde_json::{Map, Value, json};

use super::{
    CURSOR_CONFIG_VERSION, GEMINI_REMOVED, Registration, UserRegistration, Vendor, is_managed_for,
    is_user_managed_for,
};

fn remove_managed(
    vendor: Vendor,
    settings: &mut Map<String, Value>,
    prune_empty: bool,
    ours: &dyn Fn(&Value) -> bool,
) -> usize {
    let mut removed = 0;
    let Some(Value::Object(hooks)) = settings.get_mut(vendor.table()) else {
        return 0;
    };
    for event in vendor.hook_events() {
        let Some(Value::Array(groups)) = hooks.get_mut(event) else {
            continue;
        };
        let before_event = removed;
        if vendor.flat(event) {
            let before = groups.len();
            groups.retain(|hook| !ours(hook));
            removed += before - groups.len();
        }
        groups.retain_mut(|group| {
            if let Some(Value::Array(entries)) = group.get_mut("hooks") {
                let before = entries.len();
                entries.retain(|hook| !ours(hook));
                let changed = before - entries.len();
                removed += changed;
                // An unrelated empty group belongs to the user. Only remove a
                // group made empty by removing one of our own entries.
                return changed == 0 || !entries.is_empty();
            }
            true
        });
        if prune_empty && removed > before_event && groups.is_empty() {
            hooks.shift_remove(event);
        }
    }
    if prune_empty && removed > 0 && hooks.is_empty() {
        // Cursor's `version` stays: LangSpan cannot tell whether it added it.
        // A user-level file left holding only it is removed by `uninstall`
        // (`json_left_empty`).
        settings.shift_remove(vendor.table());
    }
    removed
}

/// Return the settings with LangSpan's entries replaced (install) or removed.
pub fn plan(existing: Option<Value>, registration: Option<&Registration>) -> Result<Value, String> {
    plan_for(Vendor::ClaudeCode, existing, registration)
}

/// Plan one host's registration without changing another host's entries or
/// trust configuration. Codex inherits project trust from the native host.
pub fn plan_for(
    vendor: Vendor,
    existing: Option<Value>,
    registration: Option<&Registration>,
) -> Result<Value, String> {
    let command = registration.map(|r| move |event: &str| r.command_for_event(vendor, Some(event)));
    plan_hooks(
        vendor,
        existing,
        command.as_ref().map(|c| c as &dyn Fn(&str) -> String),
        &|hook| is_managed_for(vendor, hook),
        false,
        registration.is_some_and(|r| r.outline_reads),
    )
}

/// The same plan for the host's user-level file. Only user-level entries
/// are replaced or removed; anything else in the file is left as it is.
pub fn plan_user_for(
    vendor: Vendor,
    existing: Option<Value>,
    registration: Option<&UserRegistration>,
) -> Result<Value, String> {
    let command = registration.map(|r| move |event: &str| r.command_for_event(vendor, Some(event)));
    plan_hooks(
        vendor,
        existing,
        command.as_ref().map(|c| c as &dyn Fn(&str) -> String),
        &|hook| is_user_managed_for(vendor, hook),
        true,
        registration.is_some_and(|r| r.outline_reads),
    )
}

/// What the between-step watcher's command adds to the prompt hook's own
/// (issue #214).
const WATCH_WORDS: &str = " --mode between-steps";

/// The event whose hook group also holds the host's between-step watcher:
/// only Claude Code documents a background hook that wakes the model
/// (`asyncRewake`, <https://code.claude.com/docs/en/hooks>).
fn watcher_event(vendor: Vendor) -> Option<&'static str> {
    (vendor == Vendor::ClaudeCode).then_some("UserPromptSubmit")
}

/// The watcher entry for the prompt hook `command`: the same command with
/// `--mode between-steps`, run in the background, past its own bound.
fn watcher_entry(vendor: Vendor, command: &str, user: bool) -> Option<Map<String, Value>> {
    let body = vendor.unmark(command, user)?;
    let mut entry = Map::new();
    entry.insert("type".into(), "command".into());
    entry.insert(
        "command".into(),
        vendor.mark(&format!("{body}{WATCH_WORDS}"), user).into(),
    );
    entry.insert("asyncRewake".into(), true.into());
    entry.insert(
        "timeout".into(),
        crate::hosts::WATCHER_TIMEOUT_SECONDS.into(),
    );
    Some(entry)
}

/// Whether `hook` is a watcher LangSpan wrote: a background entry whose
/// command is one of `ours` with `--mode between-steps` added.
fn is_watcher(vendor: Vendor, hook: &Value, user: bool, ours: &dyn Fn(&Value) -> bool) -> bool {
    watcher_event(vendor).is_some()
        && hook.get("asyncRewake") == Some(&json!(true))
        && hook
            .get("command")
            .and_then(Value::as_str)
            .and_then(|command| vendor.unmark(command, user))
            .and_then(|body| body.strip_suffix(WATCH_WORDS))
            .is_some_and(|body| {
                ours(&json!({"type": "command", "command": vendor.mark(body, user)}))
            })
}

/// `command` gives the generated command for each event.
fn plan_hooks(
    vendor: Vendor,
    existing: Option<Value>,
    command: Option<&dyn Fn(&str) -> String>,
    ours: &dyn Fn(&Value) -> bool,
    user: bool,
    outline_reads: bool,
) -> Result<Value, String> {
    let ours = &|hook: &Value| ours(hook) || is_watcher(vendor, hook, user, ours);
    let mut settings = match existing {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("settings file must contain a JSON object".into()),
    };
    let table = vendor.table();
    if let Some(hooks) = settings.get(table)
        && !hooks.is_object()
    {
        return Err(format!(
            "settings `{table}` must be a JSON object; nothing was changed"
        ));
    }
    if vendor.removed() && command.is_some() {
        return Err(GEMINI_REMOVED.into());
    }
    // Retain existing key positions when replacing a registration. Removal
    // prunes only containers emptied by our own entries, using ordered removal.
    remove_managed(vendor, &mut settings, command.is_none(), ours);
    if let Some(command) = command {
        if vendor == Vendor::Cursor && !settings.contains_key("version") {
            settings.insert("version".into(), json!(CURSOR_CONFIG_VERSION));
        }
        let hooks = settings
            .entry(table)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| format!("settings `{table}` must be a JSON object"))?;
        let tool_events = vendor.events().len();
        for (index, event) in vendor.hook_events().into_iter().enumerate() {
            let groups = hooks
                .entry(event)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| format!("settings `{table}.{event}` must be an array"))?;
            let mut entry = vendor.hook_entry(event, command(event));
            // Cursor puts the matcher on the entry; the others on its group.
            let mut group = Map::new();
            let holder = if vendor.flat(event) {
                &mut entry
            } else {
                &mut group
            };
            if index < tool_events {
                holder.insert(
                    "matcher".into(),
                    vendor.matcher_for(event, outline_reads).into(),
                );
            }
            if vendor.flat(event) {
                groups.push(Value::Object(entry));
            } else {
                let watcher = (watcher_event(vendor) == Some(event))
                    .then(|| watcher_entry(vendor, &command(event), user))
                    .flatten();
                let entries = std::iter::once(entry).chain(watcher).map(Value::Object);
                group.insert("hooks".into(), Value::Array(entries.collect()));
                groups.push(Value::Object(group));
            }
        }
    }
    Ok(Value::Object(settings))
}
