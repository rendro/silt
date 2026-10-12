//! What a REPL session keeps, and what an entry of it costs, grows with
//! what the entries declare, not with the entries times what each of
//! them saw.
//!
//! The session is one of the kinds of entry a person types, in rounds
//! of seven: a `let`, a function that reads it, a type, an impl for the
//! type, an entry that prints (the round's marker line), an entry that
//! fails to check, and an entry that starts a task. Every round declares
//! new names, so each entry sees more than the one before it.
//!
//! No size and no time is asserted, only that the last thousand entries
//! add at most so many times what the first thousand add.

/// The entries of a session of `rounds` rounds, and the number of
/// entries entered when each round's marker line is printed. With
/// `waiting`, the task of each round waits for as long as the session
/// lasts; without, it is joined in its entry.
fn session(rounds: usize, waiting: bool) -> (Vec<String>, Vec<usize>) {
    let mut entries = vec![
        "import task".to_string(),
        "trait Named { fn name(self) -> String }".to_string(),
    ];
    if waiting {
        entries.push("import channel".to_string());
        entries.push("let never = channel.new(0)".to_string());
    }
    let mut markers = Vec::new();
    for i in 0..rounds {
        entries.push(format!("let v{i} = {i} * 2"));
        entries.push(format!("fn f{i}(x) {{ x + v{i} }}"));
        entries.push(format!("type T{i} {{ a: Int }}"));
        entries.push(format!(
            "trait Named for T{i} {{ fn name(self) -> String {{ \"t{i}\" }} }}"
        ));
        entries.push(format!(
            "println(\"M {{f{i}(1)}} {{T{i} {{ a: 1 }}.name()}}\")"
        ));
        markers.push(entries.len());
        entries.push(format!("let bad{i}: Int = \"s\""));
        entries.push(match waiting {
            true => format!("let h{i} = task.spawn {{ -> channel.receive(never) }}"),
            false => format!("task.join(task.spawn {{ -> f{i}(2) }})"),
        });
    }
    (entries, markers)
}

/// The memory `silt repl` has resident (`VmRSS` of its process, in kB,
/// which Linux gives) while it is fed the session: read when it has
/// printed the marker of the first round that ends at or after each of
/// `at` entries.
#[cfg(target_os = "linux")]
fn resident_at(rounds: usize, waiting: bool, at: &[usize]) -> Vec<u64> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let (entries, markers) = session(rounds, waiting);
    let text = entries.join("\n") + "\n";
    let mut silt = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("repl")
        .env("SILT_HISTORY_FILE", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // (The entries that fail to check say so, round after round.)
        .stderr(Stdio::null())
        .spawn()
        .expect("silt repl starts");
    let mut stdin = silt.stdin.take().expect("the session's input");
    let feed = std::thread::spawn(move || {
        stdin
            .write_all(text.as_bytes())
            .expect("the session is written");
    });
    let mut resident = Vec::new();
    let mut seen = 0;
    for line in BufReader::new(silt.stdout.take().expect("the session's output")).lines() {
        let line = line.expect("a line of the session's output");
        if !line.starts_with("M ") {
            continue;
        }
        assert_eq!(line, format!("M {} t{seen}", 1 + seen * 2));
        if at
            .get(resident.len())
            .is_some_and(|at| markers[seen] >= *at)
        {
            let status = std::fs::read_to_string(format!("/proc/{}/status", silt.id()))
                .expect("the status of the session's process");
            resident.push(
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("VmRSS:"))
                    .and_then(|kb| kb.trim().trim_end_matches("kB").trim().parse().ok())
                    .expect("VmRSS, in kB"),
            );
        }
        seen += 1;
    }
    feed.join().expect("the session was written");
    assert!(silt.wait().expect("silt repl ends").success());
    assert_eq!(seen, rounds, "every round printed its marker");
    assert_eq!(resident.len(), at.len());
    resident
}

/// The session kept the analysis of every entry: each entry's scope,
/// the types of its names and its declarations with the import of every
/// name it saw. 3,000 entries held 497 MB, and the last thousand added
/// four times what the first thousand did. It keeps the analysis of the
/// last entry that ran, and 3,000 entries hold 36 MB.
#[test]
#[cfg(target_os = "linux")]
fn the_last_thousand_entries_add_what_the_first_thousand_do() {
    let resident = resident_at(430, false, &[1, 1_000, 2_000, 3_000]);
    let first = resident[1].saturating_sub(resident[0]);
    let last = resident[3].saturating_sub(resident[2]);
    assert!(
        last <= 2 * first,
        "entries 2,001 to 3,000 added {last} kB, the first thousand {first} kB \
         (resident, in kB: {resident:?})"
    );
}

/// The same with a task left waiting in every round. Each such task
/// kept the VM's global slots and tables as they were when it was
/// started, a copy for each: 3,000 entries held 806 MB. The tasks have
/// the slots and the tables of the session's VM in common.
#[test]
#[cfg(target_os = "linux")]
fn a_task_left_waiting_in_every_round_adds_what_it_holds_itself() {
    let resident = resident_at(430, true, &[1, 1_000, 2_000, 3_000]);
    let first = resident[1].saturating_sub(resident[0]);
    let last = resident[3].saturating_sub(resident[2]);
    assert!(
        last <= 2 * first,
        "entries 2,001 to 3,000 added {last} kB, the first thousand {first} kB \
         (resident, in kB: {resident:?})"
    );
}

/// What the compiler and the VM copy to take an entry in: the rows of
/// the session's global tables (slot names, definitions, methods,
/// traits) and the values of the VM's global slots. Every entry copied
/// all of them, several times, so the thousand entries after 2,000
/// copied five times what the first thousand did. An entry adds its
/// own rows to tables that are shared and to slots that have room, and
/// a row is copied again only where two parts of a table are made one
/// or the slots get more room: a number of times that grows with the
/// logarithm of the session's length.
///
/// What is counted is copied rows, on this thread (the session is
/// evaluated here, not by `silt repl`): no time, and nothing that
/// depends on the machine. The count comes in bursts (two layers made
/// one, the slots given twice the room), so another mix of entries
/// moves the ratio of two thousands; what stands behind the bound is
/// that the rows copied per row added grow with the logarithm of the
/// rows.
#[test]
#[cfg(feature = "test-hooks")]
fn the_last_thousand_entries_copy_what_the_first_thousand_do() {
    use silt::repl::Repl;
    use silt::session::ProjectSetup;

    let (entries, _) = session(430, true);
    let mut repl = Repl::new(ProjectSetup::None);
    let mut copied = vec![silt::layers::rows_copied()];
    for (n, entry) in entries.iter().enumerate() {
        let failing = entry.starts_with("let bad");
        assert_eq!(repl.eval(entry).committed, !failing, "{entry}");
        if [1_000, 2_000, 3_000].contains(&(n + 1)) {
            copied.push(silt::layers::rows_copied());
        }
    }
    let first = copied[1] - copied[0];
    let last = copied[3] - copied[2];
    assert!(
        last <= 2 * first,
        "entries 2,001 to 3,000 copied {last} rows, the first thousand {first} \
         (rows copied so far at entry 0, 1,000, 2,000 and 3,000: {copied:?})"
    );
}
