//! The line of landings waiting for a push turn: who is still in it, how
//! callers see it, and `TakeTurn` when the landing asks to queue.

use crate::{
    CoreError, Decision, Event, LeftQueue, MAX_TURN_QUEUE, MAX_TURN_WAIT_MS, Outcome,
    QueuedLanding, State, TURN_WAITER_SILENCE_MS, Turn, TurnQueueing, TurnWaiter, error,
    validate_request_id,
};

/// Whether `waiter` is still in line at `now_ms`.
pub(crate) fn still_waiting(waiter: &TurnWaiter, generation: u64, now_ms: u64) -> bool {
    waiter.generation == generation && waiter.deadline_ms > now_ms && waiter.seen_until_ms > now_ms
}

/// The landings in line for `key` at `now_ms`, first first.
pub(crate) fn waiting_for(state: &State, key: &str, now_ms: u64) -> Vec<TurnWaiter> {
    state
        .turn_queues
        .get(key)
        .into_iter()
        .flatten()
        .filter(|waiter| still_waiting(waiter, state.generation, now_ms))
        .cloned()
        .collect()
}

/// The line as callers see it.
pub(crate) fn line(waiting: &[TurnWaiter], now_ms: u64) -> Vec<QueuedLanding> {
    waiting
        .iter()
        .enumerate()
        .map(|(index, waiter)| QueuedLanding {
            ticket: waiter.ticket.clone(),
            session_id: waiter.session_id.clone(),
            position: u32::try_from(index + 1).unwrap_or(u32::MAX),
            waited_ms: now_ms.saturating_sub(waiter.joined_ms),
            wait_left_ms: waiter.deadline_ms.saturating_sub(now_ms),
        })
        .collect()
}

/// `TakeTurn` with a queue: take the turn if it is free and this landing is
/// first in line (or nobody is), otherwise join the line or stay in it.
pub(super) fn queue_for_turn(
    state: &State,
    workspace_id: &str,
    onto: &str,
    session_id: &Option<String>,
    expires_at_ms: u64,
    queueing: &TurnQueueing,
    now_ms: u64,
) -> Result<Decision, CoreError> {
    let TurnQueueing {
        ticket,
        wait_ms,
        joined,
    } = queueing;
    validate_request_id(ticket)?;
    if !(1..=MAX_TURN_WAIT_MS).contains(wait_ms) {
        return Err(error(
            "invalid_wait",
            "A landing waits 1 ms to 1 hour for the push turn",
        ));
    }
    let key = turn_key(workspace_id, onto);
    let held = state
        .turns
        .get(&key)
        .filter(|held| held.expires_at_ms > now_ms);
    if let Some(held) = held.filter(|held| held.turn_id == *ticket) {
        return Ok(Decision {
            events: Vec::new(),
            outcome: Outcome::Turn { turn: held.clone() },
        });
    }
    let mut events = Vec::new();
    // Waiters past their time leave before anyone is placed.
    for waiter in state.turn_queues.get(&key).into_iter().flatten() {
        if !still_waiting(waiter, state.generation, now_ms) {
            events.push(Event::TurnQueueLeft {
                workspace_id: workspace_id.into(),
                onto: onto.into(),
                ticket: waiter.ticket.clone(),
                reason: if waiter.deadline_ms <= now_ms {
                    LeftQueue::TimedOut
                } else {
                    LeftQueue::Stopped
                },
            });
        }
    }
    let mut waiting = waiting_for(state, &key, now_ms);
    let place = waiting.iter().position(|waiter| waiter.ticket == *ticket);
    // A landing taken out of line (cancelled, silent too long, or the
    // coordinator restarted) is told so, even when the turn is free: a
    // cancelled landing never goes on to push.
    if *joined && place.is_none() {
        return Err(error(
            "not_queued",
            "This landing is no longer waiting for the push turn: it was cancelled, it did not ask again within 15 s, or the coordinator restarted. Nothing was fetched or pushed; start the landing again to wait again",
        ));
    }
    if held.is_none() && waiting.first().is_none_or(|first| first.ticket == *ticket) {
        if let Some(expired) = state.turns.get(&key) {
            events.push(Event::TurnExpired {
                workspace_id: workspace_id.into(),
                onto: onto.into(),
                turn_id: expired.turn_id.clone(),
            });
        }
        let turn = Turn {
            turn_id: ticket.clone(),
            workspace_id: workspace_id.into(),
            onto: onto.into(),
            session_id: session_id.clone(),
            taken_ms: now_ms,
            expires_at_ms,
            generation: state.generation,
        };
        events.push(Event::TurnTaken { turn: turn.clone() });
        return Ok(Decision {
            events,
            outcome: Outcome::Turn { turn },
        });
    }
    let holder = held.cloned();
    let held_ms = held.map(|held| now_ms.saturating_sub(held.taken_ms));
    let seen_until_ms = now_ms.saturating_add(TURN_WAITER_SILENCE_MS);
    let position = match place {
        Some(index) => {
            // Asked again: refreshed once half its silence allowance is used,
            // so a waiting landing writes an event every few seconds, not on
            // every ask.
            let waiter = &mut waiting[index];
            if waiter.seen_until_ms.saturating_sub(now_ms) < TURN_WAITER_SILENCE_MS / 2 {
                waiter.seen_until_ms = seen_until_ms;
                events.push(Event::TurnQueueSeen {
                    workspace_id: workspace_id.into(),
                    onto: onto.into(),
                    ticket: ticket.clone(),
                    seen_until_ms,
                });
            }
            index + 1
        }
        None if waiting.len() >= MAX_TURN_QUEUE => {
            return Ok(Decision {
                events,
                outcome: Outcome::TurnUnavailable {
                    holder,
                    held_ms,
                    waiting: line(&waiting, now_ms),
                    queue_full: true,
                },
            });
        }
        None => {
            let waiter = TurnWaiter {
                ticket: ticket.clone(),
                workspace_id: workspace_id.into(),
                onto: onto.into(),
                session_id: session_id.clone(),
                joined_ms: now_ms,
                deadline_ms: now_ms.saturating_add(*wait_ms),
                seen_until_ms,
                generation: state.generation,
            };
            events.push(Event::TurnQueueJoined {
                waiter: waiter.clone(),
            });
            waiting.push(waiter);
            waiting.len()
        }
    };
    Ok(Decision {
        events,
        outcome: Outcome::TurnWaiting {
            position: u32::try_from(position).unwrap_or(u32::MAX),
            holder,
            held_ms,
            waiting: line(&waiting, now_ms),
        },
    })
}

/// Workspace IDs contain no colon, so the key is unambiguous.
pub(super) fn turn_key(workspace_id: &str, onto: &str) -> String {
    format!("{workspace_id}:{onto}")
}
