//! Whose each line of a diff is, by the line journal: hunks split between sessions, and owners.

use super::commit::diff;
use super::commit_diff::{FileDiff, Hunk, Piece, body_lines, old_first};
use super::commit_mine::{git_bounded, splits_only_whole};
use crate::journal::{self, Entry};
use langspan_core::FileState;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::Instant,
};

/// Whose a hunk, or a whole file, is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    Own,
    /// No line of it is this session's; these sessions wrote some of it.
    Others(BTreeSet<String>),
    /// No session wrote any of it.
    Nobody,
    /// Not known: why.
    Unknown(&'static str),
}

/// This session or one of its subagents (a subagent's ID is its parent's
/// followed by `.` and the agent's own).
pub(crate) fn is_own(me: &str, session: &str) -> bool {
    session == me
        || session
            .strip_prefix(me)
            .is_some_and(|rest| rest.starts_with('.'))
}

pub(super) fn hunk_owner(me: &str, hunk: &Hunk, path: Option<&journal::FileLines<'_>>) -> Owner {
    let empty = BTreeSet::new();
    let lines = hunk
        .removed
        .iter()
        .map(|key| {
            path.and_then(|p| p.removed.get(key.as_str()))
                .unwrap_or(&empty)
        })
        .chain(hunk.added.iter().map(|key| {
            path.and_then(|p| p.added.get(key.as_str()))
                .unwrap_or(&empty)
        }));
    // A piece split off as other sessions' takes the lines this session
    // wrote too with it (`split_hunk`).
    let theirs_piece = hunk.piece.is_some_and(|piece| piece.side == Side::Theirs);
    let (mut total, mut own, mut shared) = (0, 0, 0);
    let mut others = BTreeSet::new();
    for claimants in lines {
        total += 1;
        let theirs: Vec<&&str> = claimants.iter().filter(|s| !is_own(me, s)).collect();
        let mine = claimants.iter().any(|s| is_own(me, s)) && !(theirs_piece && !theirs.is_empty());
        if mine {
            own += 1;
            if !theirs.is_empty() {
                shared += 1;
            }
        } else {
            others.extend(theirs.into_iter().map(|s| (*s).to_owned()));
        }
    }
    let unknown_own = path.is_some_and(|p| p.unknown.iter().any(|s| is_own(me, s)));
    if own == 0 {
        return match (others.is_empty(), unknown_own) {
            (false, _) => Owner::Others(others),
            (true, true) => {
                Owner::Unknown("this session changed this file in lines that are not known")
            }
            (true, false) => Owner::Nobody,
        };
    }
    if own == total {
        return if shared == total {
            Owner::Unknown("each of its lines was written by this session and by another")
        } else {
            Owner::Own
        };
    }
    Owner::Unknown("part of it was written by this session and part by another session or by none")
}

/// Why a hunk that holds this session's lines and another session's is not
/// split between them.
pub(super) const REPLACED: &str = "here a line of one session's replaced another session's, so this hunk cannot be split between them";

/// Whose one line of a hunk is, for splitting it between sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Side {
    /// This session's (another's too, perhaps).
    Mine,
    /// Only other sessions'.
    Theirs,
}

/// `None` when no session wrote the line.
fn side(me: &str, claimants: Option<&BTreeSet<&str>>) -> Option<Side> {
    let claimants = claimants.filter(|c| !c.is_empty())?;
    Some(if claimants.iter().any(|s| is_own(me, s)) {
        Side::Mine
    } else {
        Side::Theirs
    })
}

/// Runs of one side, by length.
fn runs(sides: &[Side]) -> Vec<(Side, usize)> {
    let mut runs: Vec<(Side, usize)> = Vec::new();
    for side in sides {
        match runs.last_mut() {
            Some((last, count)) if last == side => *count += 1,
            _ => runs.push((*side, 1)),
        }
    }
    runs
}

/// Whether, by the journal, a line one of this session's writes added was
/// removed by another session's, or the reverse. Lines are known by text,
/// not place, so this holds for the whole file.
fn replaced_across(me: &str, path: &journal::FileLines<'_>) -> bool {
    let mine = |set: &BTreeSet<&str>| set.iter().any(|s| is_own(me, s));
    let theirs = |set: &BTreeSet<&str>| set.iter().any(|s| !is_own(me, s));
    path.added.iter().any(|(key, adders)| {
        path.removed.get(key).is_some_and(|removers| {
            (mine(adders) && theirs(removers)) || (theirs(adders) && mine(removers))
        })
    })
}

/// Why a hunk whose sessions' lines can be ordered more than one way is not
/// split between them.
pub(super) const ORDER: &str = "here this session's lines and another session's could be in more than one order, so this hunk cannot be split between them";

/// The most runs of removed lines times runs of added lines in one hunk
/// that `pieces` orders; past it, the hunk is not split. It bounds the
/// search's table (three entries per pair) to a few megabytes.
const MAX_RUN_PAIRS: usize = 1 << 16;

/// A hunk's pieces, in order: each piece's side and how many lines it
/// removes and adds. A zero-context hunk lists every removed line before
/// every added one, so where an added run goes among the other session's
/// removed lines is not in the diff. Each piece takes one whole run of
/// removed lines, one whole run of added lines of the same side, or both;
/// pieces alternate between the sides. Of every such order, those where the
/// most pieces both remove and add (an added run takes the place of its
/// side's removed run) are the likely ones. Only when exactly one order is
/// likeliest is the hunk split; otherwise `ORDER`. For example, when this
/// session's added run could take the place of either of two of its removed
/// runs, one before another session's removed line and one after it, the
/// pairing has two answers, and which is right decides on which side of the
/// other session's line this session's line goes.
pub(super) fn pieces(
    removed: &[Side],
    added: &[Side],
) -> Result<Vec<(Side, usize, usize)>, &'static str> {
    let (r, a) = (runs(removed), runs(added));
    let (rn, an) = (r.len(), a.len());
    if (rn + 1).saturating_mul(an + 1) > MAX_RUN_PAIRS {
        return Err(ORDER);
    }
    // `best[(i, j, last)]`: from runs `r[i..]` and `a[j..]`, after a piece
    // of side `last` (0: none yet, 1: Mine, 2: Theirs), the most pieces
    // that both remove and add, how many orders reach it (2 meaning two or
    // more), and the first piece of one of them. Filled from the end.
    #[derive(Clone, Copy)]
    struct Best {
        pairs: usize,
        orders: u8,
        next: Option<(Side, bool, bool)>,
    }
    let none = Best {
        pairs: 0,
        orders: 0,
        next: None,
    };
    let at = |i: usize, j: usize, last: usize| (i * (an + 1) + j) * 3 + last;
    let code = |side: Side| match side {
        Side::Mine => 1,
        Side::Theirs => 2,
    };
    let mut best = vec![none; (rn + 1) * (an + 1) * 3];
    for i in (0..=rn).rev() {
        for j in (0..=an).rev() {
            for last in 0..3 {
                if i == rn && j == an {
                    best[at(i, j, last)] = Best {
                        pairs: 0,
                        orders: 1,
                        next: None,
                    };
                    continue;
                }
                let mut here = none;
                for side in [Side::Mine, Side::Theirs] {
                    if code(side) == last {
                        continue;
                    }
                    let can_remove = r.get(i).is_some_and(|run| run.0 == side);
                    let can_add = a.get(j).is_some_and(|run| run.0 == side);
                    for (take_r, take_a) in [(true, true), (true, false), (false, true)] {
                        if (take_r && !can_remove) || (take_a && !can_add) {
                            continue;
                        }
                        let then =
                            best[at(i + usize::from(take_r), j + usize::from(take_a), code(side))];
                        if then.orders == 0 {
                            continue;
                        }
                        let pairs = then.pairs + usize::from(take_r && take_a);
                        if here.orders == 0 || pairs > here.pairs {
                            here = Best {
                                pairs,
                                orders: then.orders,
                                next: Some((side, take_r, take_a)),
                            };
                        } else if pairs == here.pairs {
                            here.orders = (here.orders + then.orders).min(2);
                        }
                    }
                }
                best[at(i, j, last)] = here;
            }
        }
    }
    match best[at(0, 0, 0)].orders {
        0 => return Err(REPLACED),
        1 => {}
        _ => return Err(ORDER),
    }
    let (mut i, mut j, mut last) = (0, 0, 0);
    let mut out = Vec::new();
    while let Some((side, take_r, take_a)) = best[at(i, j, last)].next {
        out.push((
            side,
            if take_r { r[i].1 } else { 0 },
            if take_a { a[j].1 } else { 0 },
        ));
        (i, j, last) = (i + usize::from(take_r), j + usize::from(take_a), code(side));
    }
    Ok(out)
}

/// Split each of `file`'s hunks that holds this session's lines and other
/// sessions' lines, and no line no session wrote, at the boundaries between
/// them (issue #106), as `git add -p` lets a person split a hunk. Per hunk
/// after the split: why it was left whole although it holds both, if so.
#[cfg(test)]
pub(super) fn split_between_sessions(
    me: &str,
    file: &mut FileDiff,
    path: Option<&journal::FileLines<'_>>,
) -> Vec<Option<&'static str>> {
    split_counted(me, file, path, None)
}

/// Per line key, how many more copies this session's journaled writes of
/// `path` added than removed.
fn own_copies(me: &str, path: &str, entries: &[Entry]) -> BTreeMap<String, i64> {
    let mut net: BTreeMap<String, i64> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|e| e.path == path && is_own(me, &e.session_id))
    {
        for (keys, sign) in entry
            .lines
            .iter()
            .flat_map(|lines| [(&lines.added, 1), (&lines.removed, -1)])
        {
            for key in keys {
                *net.entry(key.clone()).or_default() += sign;
            }
        }
    }
    net
}

/// The copies of each line key a file's diff adds and removes, and how many
/// of them have gone with other sessions' pieces: what tells whether a line
/// both this session and another wrote may go with the other's lines.
struct Shared<'a> {
    own: &'a BTreeMap<String, i64>,
    added: BTreeMap<String, i64>,
    removed: BTreeMap<String, i64>,
}

impl<'a> Shared<'a> {
    fn new(own: &'a BTreeMap<String, i64>, hunks: &[Hunk]) -> Self {
        let mut shared = Self {
            own,
            added: BTreeMap::new(),
            removed: BTreeMap::new(),
        };
        for hunk in hunks {
            for key in &hunk.added {
                *shared.added.entry(key.clone()).or_default() += 1;
            }
            for key in &hunk.removed {
                *shared.removed.entry(key.clone()).or_default() += 1;
            }
        }
        shared
    }

    /// Give `added` and `removed` (keys of lines both wrote) to other
    /// sessions' pieces, if every copy this session's writes account for is
    /// still left for its own: `false`, and nothing given, if not.
    fn give(&mut self, added: &[&String], removed: &[&String]) -> bool {
        let own = |key: &str| self.own.get(key).copied().unwrap_or_default();
        let mut added_left = self.added.clone();
        let mut removed_left = self.removed.clone();
        for key in added {
            let left = added_left.entry((*key).clone()).or_default();
            *left -= 1;
            if *left < own(key) {
                return false;
            }
        }
        for key in removed {
            let left = removed_left.entry((*key).clone()).or_default();
            *left -= 1;
            if *left < -own(key) {
                return false;
            }
        }
        (self.added, self.removed) = (added_left, removed_left);
        true
    }
}

/// Each line's side where its claimants decide it: `None` for a line this
/// session and another both wrote. Such a line takes the side of the lines
/// next to it in its list (another line both wrote is skipped): the one
/// side on both ends, or the only end there is. Lines between the two
/// sides, or with none, stay this session's.
fn sides_around(claimed: &[Option<Side>]) -> Vec<Side> {
    let mut before = Vec::with_capacity(claimed.len());
    let mut last = None;
    for side in claimed {
        before.push(last);
        last = side.or(last);
    }
    let mut out = vec![Side::Mine; claimed.len()];
    let mut next = None;
    for index in (0..claimed.len()).rev() {
        out[index] = match (claimed[index], before[index], next) {
            (Some(side), _, _) => side,
            (None, Some(a), Some(b)) if a == b => a,
            (None, Some(a), None) | (None, None, Some(a)) => a,
            (None, _, _) => Side::Mine,
        };
        next = claimed[index].or(next);
    }
    out
}

/// `split_between_sessions`; with `own`, the copies of each line this
/// session's writes account for, a line both it and another session wrote
/// may go with the other session's lines around it (issue #141).
fn split_counted(
    me: &str,
    file: &mut FileDiff,
    path: Option<&journal::FileLines<'_>>,
    own: Option<&BTreeMap<String, i64>>,
) -> Vec<Option<&'static str>> {
    let (Some(path), false) = (path, splits_only_whole(file)) else {
        return vec![None; file.hunks.len()];
    };
    let mut shared = own.map(|own| Shared::new(own, &file.hunks));
    let mut hunks = Vec::with_capacity(file.hunks.len());
    let mut why = Vec::with_capacity(file.hunks.len());
    for (of, hunk) in std::mem::take(&mut file.hunks).into_iter().enumerate() {
        match split_hunk(me, of, &hunk, path, shared.as_mut()) {
            Ok(Some(split)) => {
                why.extend(split.iter().map(|_| None));
                hunks.extend(split);
            }
            Ok(None) => {
                hunks.push(hunk);
                why.push(None);
            }
            Err(reason) => {
                hunks.push(hunk);
                why.push(Some(reason));
            }
        }
    }
    file.hunks = hunks;
    why
}

/// One hunk's pieces, or `None` when it is not split. `of` is its index in
/// its file's diff.
fn split_hunk<'a>(
    me: &str,
    of: usize,
    hunk: &'a Hunk,
    path: &journal::FileLines<'_>,
    shared: Option<&mut Shared<'_>>,
) -> Result<Option<Vec<Hunk>>, &'static str> {
    let sides = |keys: &[String], claims: &BTreeMap<&str, BTreeSet<&str>>| {
        keys.iter()
            .map(|key| side(me, claims.get(key.as_str())))
            .collect::<Option<Vec<Side>>>()
    };
    // A line no session wrote keeps the hunk whole: it is not known.
    let (Some(mut removed), Some(mut added)) = (
        sides(&hunk.removed, &path.removed),
        sides(&hunk.added, &path.added),
    ) else {
        return Ok(None);
    };
    // Lines both this session and another wrote go with the lines around
    // them, while this session's own copies of them are left elsewhere.
    let mut given = false;
    if let Some(shared) = shared {
        let both = |keys: &[String], claims: &BTreeMap<&str, BTreeSet<&str>>| -> Vec<bool> {
            keys.iter()
                .map(|key| {
                    claims.get(key.as_str()).is_some_and(|c| {
                        c.iter().any(|s| is_own(me, s)) && c.iter().any(|s| !is_own(me, s))
                    })
                })
                .collect()
        };
        let resolve = |sides: &[Side], both: &[bool]| -> Vec<Side> {
            let claimed: Vec<Option<Side>> = sides
                .iter()
                .zip(both)
                .map(|(side, both)| (!both).then_some(*side))
                .collect();
            sides_around(&claimed)
        };
        let (both_removed, both_added) = (
            both(&hunk.removed, &path.removed),
            both(&hunk.added, &path.added),
        );
        let (to_removed, to_added) = (
            resolve(&removed, &both_removed),
            resolve(&added, &both_added),
        );
        // The lines both wrote that go with other sessions' lines.
        let given_of = |keys: &'a [String], both: &[bool], to: &[Side]| -> Vec<&'a String> {
            keys.iter()
                .zip(both.iter().zip(to))
                .filter(|(_, (both, side))| **both && **side == Side::Theirs)
                .map(|(key, _)| key)
                .collect()
        };
        let given_removed = given_of(&hunk.removed, &both_removed, &to_removed);
        let given_added = given_of(&hunk.added, &both_added, &to_added);
        if shared.give(&given_added, &given_removed) {
            given = !given_added.is_empty() || !given_removed.is_empty();
            (removed, added) = (to_removed, to_added);
        }
    }
    let all: Vec<Side> = removed.iter().chain(&added).copied().collect();
    let lines = body_lines(&hunk.body);
    let (minus, plus) = lines.split_at(hunk.removed.len().min(lines.len()));
    let as_listed = lines.len() == all.len()
        && minus.iter().all(|l| l.first() == Some(&b'-'))
        && plus.iter().all(|l| l.first() == Some(&b'+'));
    if !as_listed {
        return Ok(None);
    }
    if !all.contains(&Side::Mine) {
        // Wholly other sessions' once the lines both wrote go with them:
        // one piece, so that those lines are not counted as this session's.
        return Ok(given.then(|| {
            cut(
                hunk,
                of,
                &[(Side::Theirs, hunk.removed.len(), hunk.added.len())],
            )
        }));
    }
    if !all.contains(&Side::Theirs) {
        return Ok(None);
    }
    if replaced_across(me, path) {
        return Err(REPLACED);
    }
    Ok(Some(cut(hunk, of, &pieces(&removed, &added)?)))
}

/// `hunk`, the `of`th of its file's diff, cut into `layout`'s pieces: each
/// a side and how many of the hunk's removed and added lines it takes, in
/// order. The hunk lists its removed lines, then its added ones.
pub(super) fn cut(hunk: &Hunk, of: usize, layout: &[(Side, usize, usize)]) -> Vec<Hunk> {
    let lines = body_lines(&hunk.body);
    let (minus, plus) = lines.split_at(hunk.removed.len().min(lines.len()));
    let new_first = hunk.new_start + u32::from(hunk.new_len == 0);
    let (mut rs, mut as_) = (0, 0);
    let mut out = Vec::new();
    for &(side, rc, ac) in layout {
        let (old_at, new_at) = (old_first(hunk) + rs as u32, new_first + as_ as u32);
        out.push(Hunk {
            piece: Some(Piece { of, side }),
            old_start: old_at - u32::from(rc == 0),
            old_len: rc as u32,
            new_start: new_at - u32::from(ac == 0),
            new_len: ac as u32,
            removed: hunk.removed[rs..rs + rc].to_vec(),
            added: hunk.added[as_..as_ + ac].to_vec(),
            body: minus[rs..rs + rc]
                .iter()
                .chain(&plus[as_..as_ + ac])
                .flat_map(|line| line.iter().copied())
                .collect(),
        });
        (rs, as_) = (rs + rc, as_ + ac);
    }
    out
}

/// The owner of a file attributed whole: the session whose journaled write
/// left it as it is now.
fn whole_owner(
    me: &str,
    current: Option<&FileState>,
    path: Option<&journal::FileLines<'_>>,
) -> Owner {
    let Some(path) = path else {
        return Owner::Nobody;
    };
    let latest = current.and_then(|current| {
        path.versions
            .iter()
            .filter(|(_, _, after)| *after == current)
            .max_by_key(|(at, _, _)| *at)
    });
    match latest {
        Some((_, session, _)) if is_own(me, session) => Owner::Own,
        Some((_, session, _)) => Owner::Others(BTreeSet::from([(*session).to_owned()])),
        None if path.versions.iter().any(|(_, s, _)| is_own(me, s)) => {
            Owner::Unknown("this session changed this file, and it has changed again since")
        }
        None => Owner::Nobody,
    }
}

/// How one file splits between this session and the rest.
pub(super) struct FileOwners {
    /// Per hunk, or one entry for a file attributed whole.
    pub(super) owners: Vec<Owner>,
    /// For each owner: other sessions that wrote some of it where this
    /// session did not, whoever else did. What the guard holds a command for.
    pub(super) foreign: Vec<BTreeSet<String>>,
    /// Attributed as one whole file.
    pub(super) whole: bool,
}

/// Of a hunk's lines, those no own session wrote: the sessions that did.
fn foreign_sessions(
    me: &str,
    hunk: &Hunk,
    path: Option<&journal::FileLines<'_>>,
) -> BTreeSet<String> {
    let Some(path) = path else {
        return BTreeSet::new();
    };
    let removed = hunk
        .removed
        .iter()
        .filter_map(|key| path.removed.get(key.as_str()));
    let added = hunk
        .added
        .iter()
        .filter_map(|key| path.added.get(key.as_str()));
    removed
        .chain(added)
        .filter(|claimants| !claimants.iter().any(|s| is_own(me, s)))
        .flatten()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Why a hunk of this session's lines is not committed when another session
/// also changed the file in lines that are not known.
const UNSEEN_COPY: &str = "another session changed this file in lines that are not known, and some of this hunk's lines appear more often than this session wrote them";
const COMMITTED_COPY: &str = "another session changed this file in lines that are not known, and this session's writes of some of this hunk's lines may already be committed";
const UNCOUNTED_COPY: &str = "another session changed this file in lines that are not known, and the file's changes since HEAD, or the commits made since this session wrote it, could not be read";

/// The most moves of `HEAD` read back to find what was committed since this
/// session's writes of a file.
const MAX_MOVES: usize = 256;

/// How one attribution reads Git: by when, and the commit `commit --mine`
/// just made, if any, whose changes are this session's own.
#[derive(Clone, Copy)]
pub(super) struct Reading<'a> {
    pub(super) deadline: Instant,
    pub(super) made: Option<&'a str>,
}

/// What decides whether this session's writes account for every copy of a
/// hunk's lines, when another session changed the file in lines that are not
/// known (issue #99 and its review).
struct Copies {
    /// Per line key: how many more copies this session's journaled writes
    /// added than removed (negative when it removed more).
    net: BTreeMap<String, i64>,
    /// The file's whole diff from `HEAD` to the working tree, whatever part
    /// of it a command would take. `None` when it could not be read.
    full: Option<FileDiff>,
    /// Line keys this session's writes added (or removed) that a move of
    /// `HEAD` since the first of those writes also added (or removed): a
    /// commit through Git may have taken that copy, and the journal still
    /// counts it. `None` when the moves could not be read.
    committed: Option<BTreeSet<String>>,
}

impl Copies {
    fn read(me: &str, root: &Path, file: &FileDiff, entries: &[Entry], reading: Reading) -> Self {
        let mut net: BTreeMap<String, i64> = BTreeMap::new();
        let (mut added, mut removed) = (BTreeSet::new(), BTreeSet::new());
        let mut since: Option<u64> = None;
        for entry in entries
            .iter()
            .filter(|e| e.path == file.path && is_own(me, &e.session_id))
        {
            since = Some(since.map_or(entry.at_ms, |at| at.min(entry.at_ms)));
            for key in entry.lines.iter().flat_map(|lines| &lines.added) {
                *net.entry(key.clone()).or_default() += 1;
                added.insert(key.clone());
            }
            for key in entry.lines.iter().flat_map(|lines| &lines.removed) {
                *net.entry(key.clone()).or_default() -= 1;
                removed.insert(key.clone());
            }
        }
        let full = if file.untracked {
            Some(file.clone())
        } else {
            let literal = format!(":(literal){}", file.path);
            diff(root, &["HEAD"], &[literal], reading.deadline)
                .ok()
                .map(|files| {
                    files
                        .into_iter()
                        .find(|f| f.path == file.path)
                        .unwrap_or_else(|| FileDiff {
                            hunks: Vec::new(),
                            ..file.clone()
                        })
                })
        };
        let committed = match since {
            Some(since) => committed_since(root, &file.path, since, (&added, &removed), reading),
            None => Some(BTreeSet::new()),
        };
        Self {
            net,
            full,
            committed,
        }
    }

    /// Whether this session's uncommitted writes account for every copy, in
    /// the file's whole diff from `HEAD`, of each line `hunk` adds and
    /// removes; if not, why. With another session's write of unknown lines
    /// in the file, a line it may also have written is told apart only by
    /// count.
    fn accounted(&self, file: &FileDiff, hunk: &Hunk) -> Result<(), &'static str> {
        let (Some(full), Some(committed)) = (&self.full, &self.committed) else {
            return Err(UNCOUNTED_COPY);
        };
        if hunk
            .added
            .iter()
            .chain(&hunk.removed)
            .any(|key| committed.contains(key))
        {
            return Err(COMMITTED_COPY);
        }
        // The diff a command takes may be part of the whole (only the staged
        // changes), or, rarely, hold a staged line the working tree has
        // since lost: the larger count of the two.
        let count = |key: &str, side: fn(&Hunk) -> &Vec<String>| {
            [file, full]
                .iter()
                .map(|f| {
                    f.hunks
                        .iter()
                        .flat_map(side)
                        .filter(|k| k.as_str() == key)
                        .count() as i64
                })
                .max()
                .unwrap_or_default()
        };
        let net = |key: &str| self.net.get(key).copied().unwrap_or_default();
        let all = hunk
            .added
            .iter()
            .all(|key| count(key, |h| &h.added) <= net(key))
            && hunk
                .removed
                .iter()
                .all(|key| count(key, |h| &h.removed) <= -net(key));
        if all { Ok(()) } else { Err(UNSEEN_COPY) }
    }
}

/// Of `added` and `removed` (line keys of `path` this session's writes
/// added and removed, the first at `since_ms`), those that a move of `HEAD`
/// at or after that write also added or removed. `HEAD`'s reflog gives the
/// moves; its times are this machine's clock, as the journal's are, and in
/// whole seconds, so a move in the same second as the write counts as after
/// it. `None` when that cannot be told: no reflog, more moves than are read,
/// or a version Git cannot give in time. The move to `reading.made`, the
/// commit `commit --mine` just made of this session's own lines, is not one.
fn committed_since(
    root: &Path,
    path: &str,
    since_ms: u64,
    (added, removed): (&BTreeSet<String>, &BTreeSet<String>),
    reading: Reading,
) -> Option<BTreeSet<String>> {
    let deadline = reading.deadline;
    let limit = format!("-n{}", MAX_MOVES + 1);
    let listed = git_bounded(
        root,
        &[
            "reflog",
            "show",
            "--format=%H %gd",
            "--date=unix",
            &limit,
            "HEAD",
            "--",
        ],
        None,
        deadline,
        1024 * 1024,
    )
    .ok()?;
    let text = String::from_utf8(listed).ok()?;
    // Newest first: each move's commit, and when it was made.
    let mut moves: Vec<(&str, u64)> = Vec::new();
    for line in text.lines() {
        let (commit, selector) = line.split_once(' ')?;
        let seconds = selector
            .strip_prefix("HEAD@{")?
            .strip_suffix('}')?
            .parse::<u64>()
            .ok()?;
        moves.push((commit, seconds));
    }
    // With no reflog, no move since the write can be ruled out.
    if moves.is_empty() {
        return None;
    }
    let mut found = BTreeSet::new();
    for (index, (commit, seconds)) in moves.iter().enumerate() {
        if seconds.saturating_mul(1000).saturating_add(999) < since_ms {
            return Some(found);
        }
        // The move before this one is where it came from; the oldest move
        // read came from nothing only if the whole reflog was read.
        let from = match moves.get(index + 1) {
            Some((from, _)) => Some(*from),
            None if moves.len() > MAX_MOVES => return None,
            None => None,
        };
        if from == Some(*commit) || reading.made == Some(*commit) {
            continue;
        }
        let before = match from {
            Some(from) => journal::head_content(root, path, Some(from), deadline)?,
            None => Vec::new(),
        };
        let after = journal::head_content(root, path, Some(commit), deadline)?;
        let lines = journal::diff_lines(&before, &after)?;
        found.extend(lines.added.into_iter().filter(|key| added.contains(key)));
        found.extend(
            lines
                .removed
                .into_iter()
                .filter(|key| removed.contains(key)),
        );
    }
    Some(found)
}

/// Whose each of `file`'s hunks is, after splitting those that hold this
/// session's lines and another's (so `file.hunks` may change).
pub(super) fn file_owners(
    me: &str,
    root: &Path,
    file: &mut FileDiff,
    claims: &BTreeMap<&str, journal::FileLines<'_>>,
    entries: &[Entry],
    current: &impl Fn(&Path, &str) -> Option<FileState>,
    reading: Reading,
) -> FileOwners {
    let path = claims.get(file.path.as_str());
    // A file too large to journal by line, or one this session changed in
    // lines that are not known, is attributable only whole. Another
    // session's write of unknown lines is not a reason to: this session's
    // hunks are still told apart by their lines (issue #99).
    let oversize = fs::symlink_metadata(root.join(&file.path))
        .is_ok_and(|m| m.len() > journal::MAX_FILE_BYTES);
    let unknown_own = path.is_some_and(|p| p.unknown.iter().any(|s| is_own(me, s)));
    let whole = file.whole || oversize || unknown_own;
    if whole {
        let others: BTreeSet<String> = path
            .map(|p| {
                p.versions
                    .iter()
                    .filter(|(_, s, _)| !is_own(me, s))
                    .map(|(_, s, _)| (*s).to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut owner = whole_owner(me, current(root, &file.path).as_ref(), path);
        if owner == Owner::Own && !others.is_empty() {
            owner = Owner::Unknown(
                "another session also changed this file, and its lines cannot be told apart",
            );
        }
        let foreign = if owner == Owner::Nobody {
            BTreeSet::new()
        } else {
            others
        };
        return FileOwners {
            owners: vec![owner],
            foreign: vec![foreign],
            whole: true,
        };
    }
    // Other sessions that changed the file in lines that are not known: any
    // hunk not wholly this session's may be theirs.
    let unseen: BTreeSet<String> = path
        .map(|p| p.unknown.iter().map(|s| (*s).to_owned()).collect())
        .unwrap_or_default();
    let own = own_copies(me, &file.path, entries);
    let unsplit = split_counted(me, file, path, Some(&own));
    let file = &*file;
    let mut copies: Option<Copies> = None;
    let mut owners = Vec::new();
    let mut foreign = Vec::new();
    for (hunk, unsplit) in file.hunks.iter().zip(unsplit) {
        let mut owner = match (hunk_owner(me, hunk, path), unsplit) {
            (Owner::Unknown(_), Some(why)) => Owner::Unknown(why),
            (owner, _) => owner,
        };
        let mut theirs = foreign_sessions(me, hunk, path);
        if !unseen.is_empty() {
            owner = match owner {
                // Read once per file, and only for a hunk that needs it.
                Owner::Own => match copies
                    .get_or_insert_with(|| Copies::read(me, root, file, entries, reading))
                    .accounted(file, hunk)
                {
                    Ok(()) => Owner::Own,
                    Err(why) => Owner::Unknown(why),
                },
                Owner::Nobody => Owner::Others(unseen.clone()),
                Owner::Others(mut sessions) => {
                    sessions.extend(unseen.iter().cloned());
                    Owner::Others(sessions)
                }
                unknown => unknown,
            };
            if owner != Owner::Own {
                theirs.extend(unseen.iter().cloned());
            }
        }
        owners.push(owner);
        foreign.push(theirs);
    }
    FileOwners {
        owners,
        foreign,
        whole: false,
    }
}

/// Whether `session`, or the session it is a subagent of, is live.
pub(crate) fn is_live(live: &BTreeSet<String>, session: &str) -> bool {
    let mut name = session;
    loop {
        if live.contains(name) {
            return true;
        }
        match name.rsplit_once('.') {
            // `source.host` is the shortest session ID.
            Some((parent, _)) if parent.contains('.') => name = parent,
            _ => return false,
        }
    }
}

/// Where a hunk is: its first line and how many lines, in the working file,
/// or where it was in `HEAD`'s when it only removes lines.
pub(super) fn span(hunk: &Hunk) -> (u32, u32) {
    if hunk.new_len == 0 {
        (hunk.old_start, hunk.old_len)
    } else {
        (hunk.new_start, hunk.new_len)
    }
}

/// Lines `start`-`end` of a hunk, for people.
pub(super) fn lines_of(hunk: &Hunk) -> String {
    let (start, len) = span(hunk);
    if len <= 1 {
        format!("line {start}")
    } else {
        format!("lines {start}-{}", start + len - 1)
    }
}
