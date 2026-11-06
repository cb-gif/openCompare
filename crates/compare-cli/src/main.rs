use clap::{Args, Parser, Subcommand, ValueEnum};
use compare_core::{compare, CompareOptions, DocumentSnapshot};
use compare_fs::{compare_directories, load_document, EntryStatus, TextEncoding};
use serde_json::json;
use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    sync::atomic::AtomicBool,
};

#[derive(Parser)]
#[command(
    name = "bcompare",
    version,
    about = "Local file and directory comparison powered by Rust"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare text; exits 0 for equal, 1 for different, 2 for errors.
    Diff(DiffArgs),
    /// Recursively compare directories by exact file contents.
    Dir(DirArgs),
}

#[derive(Args)]
struct DiffArgs {
    left: PathBuf,
    right: PathBuf,
    #[arg(long)]
    ignore_trailing_whitespace: bool,
    #[arg(long)]
    ignore_case: bool,
    #[arg(long)]
    ignore_line_endings: bool,
    #[arg(long, value_enum)]
    left_encoding: Option<Encoding>,
    #[arg(long, value_enum)]
    right_encoding: Option<Encoding>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct DirArgs {
    left: PathBuf,
    right: PathBuf,
    #[arg(long)]
    differences_only: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Encoding {
    Utf8,
    Utf16le,
    Utf16be,
    Gbk,
}
impl From<Encoding> for TextEncoding {
    fn from(value: Encoding) -> Self {
        match value {
            Encoding::Utf8 => Self::Utf8,
            Encoding::Utf16le => Self::Utf16Le,
            Encoding::Utf16be => Self::Utf16Be,
            Encoding::Gbk => Self::Gbk,
        }
    }
}

fn main() -> ExitCode {
    let mut output = io::BufWriter::new(io::stdout().lock());
    match run(Cli::parse(), &mut output).and_then(|code| {
        output.flush()?;
        Ok(code)
    }) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli, output: &mut impl Write) -> Result<u8, Box<dyn std::error::Error>> {
    match cli.command {
        Command::Diff(args) => {
            let left = load_document(&args.left, args.left_encoding.map(Into::into))?;
            let right = load_document(&args.right, args.right_encoding.map(Into::into))?;
            let options = CompareOptions {
                ignore_trailing_whitespace: args.ignore_trailing_whitespace,
                ignore_case: args.ignore_case,
                ignore_line_endings: args.ignore_line_endings,
            };
            let diff = compare(&left.document, &right.document, options);
            if args.json {
                let hunks: Vec<_> = diff
                    .hunks
                    .iter()
                    .map(|h| {
                        json!({ "left": { "start": h.left.start, "end": h.left.end },
                    "right": { "start": h.right.start, "end": h.right.end } })
                    })
                    .collect();
                serde_json::to_writer_pretty(
                    &mut *output,
                    &json!({ "equal": hunks.is_empty(), "hunks": hunks,
                    "options": options, "coarse_alignment": diff.coarse_alignment,
                    "left_encoding": left.format.encoding, "right_encoding": right.format.encoding }),
                )?;
                writeln!(output)?;
            } else if diff.hunks.is_empty() {
                writeln!(output, "No differences under selected rules.")?;
            } else {
                writeln!(
                    output,
                    "--- {}\n+++ {}",
                    args.left.display(),
                    args.right.display()
                )?;
                for h in &diff.hunks {
                    let start = |range: &std::ops::Range<usize>| {
                        range.start + usize::from(!range.is_empty())
                    };
                    writeln!(
                        output,
                        "@@ -{},{} +{},{} @@",
                        start(&h.left),
                        h.left.len(),
                        start(&h.right),
                        h.right.len()
                    )?;
                    print_lines(output, '-', &left.document, h.left.clone())?;
                    print_lines(output, '+', &right.document, h.right.clone())?;
                }
            }
            Ok(u8::from(!diff.hunks.is_empty()))
        }
        Command::Dir(args) => {
            let report =
                compare_directories(&args.left, &args.right, &AtomicBool::new(false), |_| {})?;
            let code = if report.error_count() > 0 {
                2
            } else {
                u8::from(report.different_count() > 0)
            };
            if args.json {
                serde_json::to_writer_pretty(&mut *output, &report)?;
                writeln!(output)?;
            } else {
                for entry in &report.entries {
                    if args.differences_only
                        && matches!(entry.status, EntryStatus::Equal | EntryStatus::Directory)
                    {
                        continue;
                    }
                    writeln!(
                        output,
                        "{:12?} {}{}",
                        entry.status,
                        entry.relative_path.display(),
                        entry
                            .error
                            .as_ref()
                            .map_or(String::new(), |e| format!("  ({e})"))
                    )?;
                }
                writeln!(
                    output,
                    "{} entries, {} differences, {} errors",
                    report.entries.len(),
                    report.different_count(),
                    report.error_count()
                )?;
            }
            Ok(code)
        }
    }
}

fn print_lines(
    output: &mut impl Write,
    prefix: char,
    document: &DocumentSnapshot,
    lines: std::ops::Range<usize>,
) -> io::Result<()> {
    for index in lines {
        let line = document.line(index);
        write!(output, "{prefix}{line}")?;
        if !line.ends_with('\n') {
            writeln!(output, "\n\\ No newline at end of file")?;
        }
    }
    Ok(())
}
