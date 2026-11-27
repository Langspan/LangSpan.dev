//! Option values shared by the drivers in main.rs and the library.

use std::collections::BTreeMap;

/// An `on`/`off` option that is on unless switched off.
/// `--outline-reads on|off` (roadmap A11): off unless switched on, because
/// it changes what a host's first whole read of a large file returns.
pub fn outline_reads(options: &mut BTreeMap<String, String>) -> Result<bool, crate::Error> {
    match options.remove("--outline-reads").as_deref() {
        None | Some("off") => Ok(false),
        Some("on") => Ok(true),
        Some(_) => Err("--outline-reads must be on or off".into()),
    }
}

pub fn switch(options: &mut BTreeMap<String, String>, name: &str) -> Result<bool, crate::Error> {
    match options.remove(name).as_deref() {
        None | Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(_) => Err(format!("{name} must be on or off").into()),
    }
}

/// `--presence on|off` and `--presence-minutes N` (roadmap A13).
pub fn presence(
    options: &mut BTreeMap<String, String>,
) -> Result<crate::install::Presence, crate::Error> {
    Ok(crate::install::Presence {
        shown: switch(options, "--presence")?,
        minutes: match options.remove("--presence-minutes") {
            Some(value) => crate::install::Presence::minutes(&value)?,
            None => None,
        },
    })
}

pub fn no_unused_options(options: &BTreeMap<String, String>) -> Result<(), crate::Error> {
    if let Some((name, _)) = options.first_key_value() {
        return Err(format!("option {name} is not valid for this command").into());
    }
    Ok(())
}
