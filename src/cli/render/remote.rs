//! Remote publication and retention.

use crate::today::utc;
use langspan_core::SentPublication;

pub(super) fn print_remote_sent(sent: &[SentPublication], more: &bool, since_ms: &Option<u64>) {
    if let Some(since) = since_ms {
        println!(
            "Retention removed older records: this lists every publication since {}.",
            utc(*since)
        );
    }
    if sent.is_empty() {
        println!("Nothing has been published for this workspace.");
    }
    for record in sent {
        println!(
            "Sequence {} at {} (event {}):",
            record.sequence,
            record
                .published_at_ms
                .map_or_else(|| "an unrecorded time".into(), utc),
            record.seq
        );
        println!(
            "{}",
            record
                .payload
                .as_deref()
                .unwrap_or("(payload not recorded: published before payloads were kept)")
        );
    }
    if *more {
        println!("Older publications were left out; ask for fewer with --limit.");
    }
}

/// `value` with a comma between each group of three digits.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `bytes` in the largest binary unit that keeps a whole number.
fn sized(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{} KiB", bytes / 1024),
        _ => format!("{} MiB", bytes / 1_048_576),
    }
}

pub(super) fn print_compaction(report: &langspan_core::CompactionReport) {
    let verb = if report.applied {
        "deleted"
    } else {
        "would delete"
    };
    let range = match (report.first_seq, report.last_seq) {
        (Some(first), Some(last)) => format!("seq {first}..{last}, "),
        _ => String::new(),
    };
    println!(
        "{verb} {} events ({range}before {}) and {} receipts",
        grouped(report.events),
        utc(report.cutoff_ms),
        grouped(report.receipts)
    );
    match report.snapshot_seq {
        Some(seq) => println!("snapshot at seq {seq}; about {} freed", sized(report.bytes)),
        None => println!("no snapshot: no event is older than the window"),
    }
    if let Some(held) = &report.held_by {
        println!("kept from there on: {held}");
    }
    if !report.applied {
        println!("dry run: add --apply to compact");
    }
}

#[cfg(test)]
mod time_tests {
    #[test]
    fn counts_and_sizes_read_as_people_write_them() {
        assert_eq!(super::grouped(0), "0");
        assert_eq!(super::grouped(999), "999");
        assert_eq!(super::grouped(1000), "1,000");
        assert_eq!(super::grouped(812_440), "812,440");
        assert_eq!(super::grouped(61_204_000), "61,204,000");
        assert_eq!(super::sized(1023), "1023 bytes");
        assert_eq!(super::sized(2048), "2 KiB");
        assert_eq!(super::sized(410 * 1_048_576 + 5), "410 MiB");
    }
}
