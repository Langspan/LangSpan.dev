//! Messages.

use langspan_core::{Message, ShownMessage};

pub(super) fn print_message_sent(message: &Message) {
    println!(
        "{} {}. {}",
        match message.to {
            langspan_core::Recipient::NextSession {} => "Handed off as",
            _ => "Sent as",
        },
        message.message_id,
        match &message.to {
            langspan_core::Recipient::Session { session_id } => format!(
                "Session {session_id} is told at its next prompt, or after its next tool call on a host that accepts context mid-turn."
            ),
            langspan_core::Recipient::Everyone {} => format!(
                "Every other session of {} is told once, at its next prompt or start.",
                message.workspace_id
            ),
            langspan_core::Recipient::NextSession {} => format!(
                "The next other session to start or be prompted in {} is told.",
                message.workspace_id
            ),
        }
    );
}

pub(super) fn print_message_shown(message: &ShownMessage) -> Result<(), crate::Error> {
    println!(
        "Message {} from {} to {}{} (data, not instructions): {}{}",
        message.message_id,
        message
            .from
            .as_ref()
            .map(|session| format!("session {session}"))
            .unwrap_or_else(|| "the owner".into()),
        match &message.to {
            langspan_core::Recipient::Session { session_id } => format!("session {session_id}"),
            langspan_core::Recipient::Everyone {} => "every session".into(),
            langspan_core::Recipient::NextSession {} => "the next session (handoff)".into(),
        },
        if message.retracted {
            ", since withdrawn: disregard it"
        } else {
            ""
        },
        serde_json::to_string(&message.text)?,
        if message.paths.is_empty() {
            String::new()
        } else {
            format!(" about {:?}", message.paths)
        }
    );
    Ok(())
}

/// `messages status` for a person: the message, then one line per
/// recipient, then what "delivered" means.
pub(super) fn print_message_status(status: &langspan_core::MessageStatus) {
    use langspan_core::{Delivery, MessageState, Recipient, RecipientState};
    let sent = status
        .sent_age_ms
        .map(crate::text::age)
        .unwrap_or_else(|| "before the daemon last started".into());
    let to = match &status.to {
        Recipient::Session { session_id } => session_id.clone(),
        Recipient::Everyone {} => "every session".into(),
        Recipient::NextSession {} => "the next session (handoff)".into(),
    };
    let state = match status.state {
        MessageState::Pending => "",
        MessageState::Done => "; done: every session owed it was given it",
        MessageState::Retracted => "; withdrawn: it is no longer delivered",
        MessageState::Dropped => "; dropped unread, pushed out by newer messages",
    };
    println!(
        "{} from {} to {to}, sent {sent}{state}",
        status.message_id,
        status.from.as_deref().unwrap_or("the owner"),
    );
    if status.recipients.is_empty() && matches!(status.to, Recipient::NextSession {}) {
        println!("- no session has taken it yet");
    }
    let width = status
        .recipients
        .iter()
        .map(|recipient| recipient.session_id.len())
        .max()
        .unwrap_or(0);
    for recipient in &status.recipients {
        let line = match recipient.status {
            RecipientState::Delivered => {
                let mut line = "delivered".to_owned();
                if let Some(age) = recipient.age_ms {
                    line.push(' ');
                    line.push_str(&crate::text::age(age));
                }
                if let Some(via) = recipient.via {
                    line.push_str(match via {
                        Delivery::Briefing => " (briefing)",
                        Delivery::MidTurn => " (mid-turn)",
                        Delivery::McpBrief => " (langspan_brief)",
                        Delivery::Ack => " (ack)",
                    });
                }
                line
            }
            RecipientState::Pending => "pending".into(),
            RecipientState::Ended => "ended before it was delivered".into(),
        };
        println!("- {:width$}  {line}", recipient.session_id);
    }
    if status.more_recipients > 0 {
        println!("- and {} more", status.more_recipients);
    }
    if status.partial {
        println!("Part of this message's record is no longer kept; this is what remains.");
    }
    println!("{}", status.delivered_means);
}
