//! `mb-validate`: stdin/file TSV validator for the mb-import container job.
//!
//! Validate mode reads one curated dump file (or chunk of it), writes the
//! normalized valid rows for `COPY` and the poison rows for the dead-letter
//! table, and prints a `valid=<n> poison=<m>` summary. Poison rows are
//! expected data, so they never change the exit code.
//!
//! Plan mode prints the 1-based inclusive chunk ranges still pending after
//! the resume cursor, one `start-end` per line.
//!
//! Exit codes: 0 success, 2 usage or I/O error.

use mb_import_validate::{escape_poison_field, plan_chunks, reason, Table};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};

fn usage() -> ! {
    eprintln!(
        "usage:\n  mb-validate --table artist|release|recording --in <tsv> \\\n      --valid-out <tsv> --poison-out <tsv> --line-offset <n>\n  mb-validate --plan --completed <n> --total <n> --size <n>"
    );
    std::process::exit(2);
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find_map(|w| {
        if w[0] == name {
            Some(w[1].clone())
        } else {
            None
        }
    })
}

fn number(raw: Option<String>, name: &str) -> u64 {
    let raw = raw.unwrap_or_else(|| {
        eprintln!("mb-validate: missing {name}");
        usage();
    });
    raw.parse::<u64>().unwrap_or_else(|_| {
        eprintln!("mb-validate: {name} must be a non-negative integer, got {raw:?}");
        usage();
    })
}

fn run_plan(args: &[String]) {
    let completed = number(flag(args, "--completed"), "--completed");
    let total = number(flag(args, "--total"), "--total");
    let size = number(flag(args, "--size"), "--size");
    if size == 0 {
        eprintln!("mb-validate: --size must be >= 1");
        std::process::exit(2);
    }
    let out = std::io::stdout();
    let mut out = BufWriter::new(out.lock());
    for (start, end) in plan_chunks(completed, total, size) {
        // A closed downstream (e.g. `| head -n 1`) reports BrokenPipe: that
        // is a clean shutdown, not a failed plan. Anything else is exit 2.
        if let Err(e) = writeln!(out, "{start}-{end}") {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return;
            }
            eprintln!("mb-validate: cannot write plan: {e}");
            std::process::exit(2);
        }
    }
    if let Err(e) = out.flush() {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            eprintln!("mb-validate: cannot write plan: {e}");
            std::process::exit(2);
        }
    }
}

fn run_validate(args: &[String]) {
    let table =
        Table::parse(&flag(args, "--table").unwrap_or_else(|| usage())).unwrap_or_else(|| usage());
    let input = flag(args, "--in").unwrap_or_else(|| usage());
    let valid_path = flag(args, "--valid-out").unwrap_or_else(|| usage());
    let poison_path = flag(args, "--poison-out").unwrap_or_else(|| usage());
    let offset = number(flag(args, "--line-offset"), "--line-offset");

    let file = File::open(&input).unwrap_or_else(|e| {
        eprintln!("mb-validate: cannot open {input:?}: {e}");
        std::process::exit(2);
    });
    let mut bytes = Vec::new();
    BufReader::new(file)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|e| {
            eprintln!("mb-validate: cannot read {input:?}: {e}");
            std::process::exit(2);
        });

    let valid_file = File::create(&valid_path).unwrap_or_else(|e| {
        eprintln!("mb-validate: cannot write {valid_path:?}: {e}");
        std::process::exit(2);
    });
    let poison_file = File::create(&poison_path).unwrap_or_else(|e| {
        eprintln!("mb-validate: cannot write {poison_path:?}: {e}");
        std::process::exit(2);
    });
    let mut valid = BufWriter::new(valid_file);
    let mut poison = BufWriter::new(poison_file);

    let mut valid_n: u64 = 0;
    let mut poison_n: u64 = 0;
    // Split on LF only: a raw CR stays inside the field (COPY text data),
    // matching how Postgres reads the same file. A trailing newline ends
    // the last line; it is not an extra empty line.
    let mut parts: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if bytes.last() == Some(&b'\n') {
        parts.pop();
    }
    if bytes.is_empty() {
        parts.clear();
    }
    for (idx, chunk) in parts.iter().enumerate() {
        let line_no = offset + idx as u64 + 1;
        // Every write below is checked: a short valid.tsv (disk full,
        // closed pipe) must exit 2, never print counts that claim the chunk
        // validated cleanly — import.sh advances the cursor on those counts,
        // so a silent short write would skip rows forever on rerun.
        match String::from_utf8(chunk.to_vec()) {
            Err(_) => {
                let lossy = String::from_utf8_lossy(chunk);
                writeln!(
                    poison,
                    "{}\t{}\t{}",
                    reason::INVALID_ENCODING,
                    line_no,
                    escape_poison_field(&lossy)
                )
                .unwrap_or_else(|e| {
                    eprintln!("mb-validate: cannot write {poison_path:?}: {e}");
                    std::process::exit(2);
                });
                poison_n += 1;
            }
            Ok(line) => {
                // Strip one trailing CR from CRLF input so CRLF dumps
                // validate like the LF dumps COPY would read line-wise.
                let line = line.strip_suffix('\r').unwrap_or(&line);
                match mb_import_validate::validate_line(table, line_no, line) {
                    Ok(normalized) => {
                        writeln!(valid, "{normalized}").unwrap_or_else(|e| {
                            eprintln!("mb-validate: cannot write {valid_path:?}: {e}");
                            std::process::exit(2);
                        });
                        valid_n += 1;
                    }
                    Err((why, _)) => {
                        writeln!(poison, "{why}\t{line_no}\t{}", escape_poison_field(line))
                            .unwrap_or_else(|e| {
                                eprintln!("mb-validate: cannot write {poison_path:?}: {e}");
                                std::process::exit(2);
                            });
                        poison_n += 1;
                    }
                }
            }
        }
    }
    // BufWriter swallows flush errors on drop, so flush explicitly: without
    // this a full disk still exits 0 with a truncated output file.
    valid.flush().unwrap_or_else(|e| {
        eprintln!("mb-validate: cannot write {valid_path:?}: {e}");
        std::process::exit(2);
    });
    poison.flush().unwrap_or_else(|e| {
        eprintln!("mb-validate: cannot write {poison_path:?}: {e}");
        std::process::exit(2);
    });
    println!("valid={valid_n} poison={poison_n}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--plan") {
        run_plan(&args);
    } else {
        run_validate(&args);
    }
}
