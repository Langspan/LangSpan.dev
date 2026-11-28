//! Each ADR in `docs/adr/` has a number of its own, and its title
//! (`# ADR NNNN: ...`) repeats its file name's number. A file whose title
//! starts `# Moved:` only points to an ADR's new number, so an old link keeps
//! working, and holds no number (issue #633).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// The problems in the ADRs given as (file name, first line).
fn problems(adrs: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut owners: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, title) in adrs {
        if title.starts_with("# Moved:") {
            continue;
        }
        let number = name.split('-').next().unwrap_or_default();
        if !title.starts_with(&format!("# ADR {number}: ")) {
            problems.push(format!(
                "{name}: its title does not start `# ADR {number}: `"
            ));
        }
        owners.entry(number).or_default().push(name);
    }
    for (number, names) in owners {
        if names.len() > 1 {
            problems.push(format!("ADR {number} is used by {}", names.join(", ")));
        }
    }
    problems
}

#[test]
fn the_check_finds_a_shared_number_and_a_wrong_title() {
    let adr = |name: &str, title: &str| (name.to_string(), title.to_string());
    assert!(
        problems(&[
            adr("0001-a.md", "# ADR 0001: A"),
            adr("0001-b.md", "# Moved: ADR 0002, B"),
            adr("0002-b.md", "# ADR 0002: B"),
        ])
        .is_empty()
    );
    assert_eq!(
        problems(&[
            adr("0001-a.md", "# ADR 0001: A"),
            adr("0001-b.md", "# ADR 0001: B"),
            adr("0003-c.md", "# ADR 0002: C"),
        ]),
        [
            "0003-c.md: its title does not start `# ADR 0003: `",
            "ADR 0001 is used by 0001-a.md, 0001-b.md",
        ]
    );
}

#[test]
fn every_adr_has_a_number_of_its_own() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/adr");
    let mut adrs: Vec<(String, String)> = fs::read_dir(&dir)
        .expect("list docs/adr")
        .map(|entry| entry.expect("read docs/adr").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .map(|path| {
            let text = fs::read_to_string(&path).expect("read an ADR");
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, text.lines().next().unwrap_or_default().to_string())
        })
        .collect();
    adrs.sort();
    let problems = problems(&adrs);
    assert!(
        problems.is_empty(),
        "Give each ADR the next free number, and leave a `# Moved:` file at an \
         old path:\n{}",
        problems.join("\n")
    );
}
