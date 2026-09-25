//! How reliably a model turns a plain-language mistake report into working
//! rules ("Report a Mistake" and "Felix, it keeps writing…").
//!
//!     cargo run --release --example rules_eval -- chatgpt [model] [repeats]
//!     cargo run --release --example rules_eval -- local [model] [repeats]
//!
//! Each case is a report, the dictation it's about, and checks the rules
//! weren't written with: the reported mistake in another sentence, and
//! ordinary speech that must stay as it is. A case passes when the answer
//! parses, its own tests pass and every check passes. Checks on sound-alikes
//! pass when the local model could get them right (it decides those live).
//! Everything is made up (Sam Rivera is fictional).

use handy_app_lib::meetings::llm::Llm;
use handy_app_lib::rules::{self, TestCase};
use std::time::Instant;

struct Case {
    name: &'static str,
    report: &'static str,
    /// The last dictation: as transcribed, as pasted.
    last: (&'static str, &'static str),
    /// (said, expect) checks the rules must pass.
    checks: &'static [(&'static str, &'static str)],
    /// Rules alone can't fix it: expect needs_code_change and no change.
    needs_code: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "misspelled term",
        report: "it wrote cube control instead of kubectl",
        last: ("run cube control get pods", "Run cube control get pods."),
        checks: &[
            (
                "then cube control apply the file",
                "then kubectl apply the file",
            ),
            ("the control panel is open", "the control panel is open"),
        ],
        needs_code: false,
    },
    Case {
        name: "casing",
        report: "github should always be written GitHub",
        last: ("push it to github", "Push it to github."),
        checks: &[("open a github issue", "open a GitHub issue")],
        needs_code: false,
    },
    Case {
        name: "multi-word mishearing",
        report: "super base should be Supabase, it's the database we use",
        last: ("store it in super base", "Store it in super base."),
        checks: &[
            ("the super base dashboard", "the Supabase dashboard"),
            ("a super basic idea", "a super basic idea"),
        ],
        needs_code: false,
    },
    Case {
        name: "person's name",
        report: "my coworker's name is Sam Rivera, it keeps writing Sam Riviera",
        last: ("ask Sam Riviera about it", "Ask Sam Riviera about it."),
        checks: &[("cc Sam Riviera on the email", "cc Sam Rivera on the email")],
        needs_code: false,
    },
    Case {
        name: "name that is a common word",
        report: "Tauri, the app framework, keeps coming out as tory",
        last: (
            "the tory build failed again",
            "The tory build failed again.",
        ),
        checks: &[
            ("rebuild the tory app", "rebuild the Tauri app"),
            ("tory config", "Tauri config"),
        ],
        needs_code: false,
    },
    Case {
        name: "garbled name",
        report: "kubernetes comes out as cooper netties",
        last: (
            "deploy it on cooper netties",
            "Deploy it on cooper netties.",
        ),
        checks: &[("our cooper netties cluster", "our Kubernetes cluster")],
        needs_code: false,
    },
    Case {
        name: "dictated report, itself misheard",
        report: "it keeps writing post gress instead of post gress, the database",
        last: (
            "migrate the post gress tables",
            "Migrate the post gress tables.",
        ),
        checks: &[("back up post gress tonight", "back up Postgres tonight")],
        needs_code: false,
    },
    Case {
        name: "extend an existing sound-alike",
        report: "cloud desktop should be Claude Desktop, the app",
        last: ("open cloud desktop", "Open cloud desktop."),
        checks: &[
            ("restart cloud desktop", "restart Claude Desktop"),
            ("open cloud code", "open Claude Code"),
            ("our cloud bill went up", "our cloud bill went up"),
        ],
        needs_code: false,
    },
    Case {
        name: "vague report",
        report: "that last one was wrong",
        last: ("check the llama server logs", "Check the Lama server logs."),
        checks: &[("the llama server is down", "the llama server is down")],
        needs_code: false,
    },
    Case {
        name: "not a rules problem",
        report: "it cuts off the last word when I stop talking",
        last: ("send it over to the", "Send it over to the"),
        checks: &[],
        needs_code: true,
    },
    Case {
        name: "number formatting request",
        report: "when I say version three point five it should write v3.5",
        last: (
            "upgrade to version three point five",
            "Upgrade to version three point five.",
        ),
        checks: &[("we're on version three point five now", "we're on v3.5 now")],
        needs_code: false,
    },
    Case {
        name: "acronym",
        report: "it writes l l m instead of LLM",
        last: ("the l l m call is slow", "The l l m call is slow."),
        checks: &[("pick a smaller l l m", "pick a smaller LLM")],
        needs_code: false,
    },
];

#[derive(Default)]
struct Tally {
    runs: u32,
    passed: u32,
    errors: u32,
    retried: u32,
    seconds: f64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let backend = args.first().map_or("chatgpt", String::as_str);
    let repeats: u32 = args.get(2).and_then(|r| r.parse().ok()).unwrap_or(1);
    let only = std::env::var("CASE").ok();
    let (llm, effort) = match backend {
        "local" => (
            Llm::Local {
                model: args.get(1).cloned().unwrap_or("qwen3.5:4b".into()),
                keep_loaded: true,
            },
            "low",
        ),
        _ => (
            Llm::Chatgpt {
                model: args.get(1).cloned().unwrap_or("gpt-6-sol".into()),
            },
            "medium",
        ),
    };
    println!("{} × {repeats}\n", llm.label());
    let mut tally = Tally::default();
    tauri::async_runtime::block_on(async {
        for case in CASES {
            if only.as_deref().is_some_and(|o| !case.name.contains(o)) {
                continue;
            }
            for _ in 0..repeats {
                tally.runs += 1;
                let started = Instant::now();
                let result = rules::draft(
                    &llm,
                    effort,
                    rules::STARTER,
                    case.report,
                    &[(case.last.0.into(), case.last.1.into())],
                )
                .await;
                let secs = started.elapsed().as_secs_f64();
                tally.seconds += secs;
                let draft = match result {
                    Ok(d) => d,
                    Err(e) => {
                        tally.errors += 1;
                        println!("✗ {:<32} {secs:5.1}s  error: {e}", case.name);
                        continue;
                    }
                };
                if draft.attempts > 1 {
                    tally.retried += 1;
                }
                let p = &draft.proposal;
                let mut problems = Vec::new();
                if let Some(e) = &p.error {
                    problems.push(format!("bad rules: {e}"));
                }
                for t in p.tests.iter().filter(|t| !t.passed) {
                    problems.push(format!("own test {:?} → {:?}", t.said, t.got));
                }
                let changed: Vec<String> = p
                    .diff
                    .iter()
                    .filter(|l| l.kind != "same")
                    .map(|l| format!("{}{}", if l.kind == "added" { "+ " } else { "- " }, l.text))
                    .collect();
                if case.needs_code {
                    if !p.needs_code_change {
                        problems.push("didn't say it needs a code change".into());
                    }
                    if changed.iter().any(|l| {
                        !l.contains("[[test]]")
                            && !l.starts_with("+ said")
                            && !l.starts_with("+ expect")
                            && l.trim() != "+"
                    }) {
                        problems.push("changed rules anyway".into());
                    }
                } else {
                    let checks: Vec<TestCase> = case
                        .checks
                        .iter()
                        .map(|(said, expect)| TestCase {
                            said: (*said).into(),
                            expect: (*expect).into(),
                            ..Default::default()
                        })
                        .collect();
                    match rules::check_against(&p.rules, &checks) {
                        Ok(results) => {
                            for r in results.iter().filter(|r| !r.passed) {
                                problems.push(format!(
                                    "check {:?} → {:?} (want {:?})",
                                    r.said, r.got, r.expect
                                ));
                            }
                        }
                        Err(e) => problems.push(format!("bad rules: {e}")),
                    }
                }
                let retry = if draft.attempts > 1 { " (retried)" } else { "" };
                if problems.is_empty() {
                    tally.passed += 1;
                    println!("✓ {:<32} {secs:5.1}s{retry}", case.name);
                } else {
                    println!("✗ {:<32} {secs:5.1}s{retry}", case.name);
                    for p in &problems {
                        println!("    {p}");
                    }
                }
                if std::env::var_os("SHOW").is_some() || !problems.is_empty() {
                    println!("    explanation: {}", p.explanation);
                    for l in changed.iter().take(24) {
                        println!("    {l}");
                    }
                }
            }
        }
    });
    println!(
        "\n{}/{} passed, {} errors, {} needed a retry, {:.1}s average",
        tally.passed,
        tally.runs,
        tally.errors,
        tally.retried,
        tally.seconds / tally.runs.max(1) as f64
    );
}
