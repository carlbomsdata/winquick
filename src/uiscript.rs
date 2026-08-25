//! The little language `winquick ui-test` runs.
//!
//! A UI test is a sequence of the same verbs `winquick desktop` takes, one per
//! line, plus two things that only make sense in a script: `screenshot`, whose
//! output has to land on the Mac rather than in the guest, and `expect`, which
//! turns a UI Automation property into a pass or a fail.
//!
//! ```text
//! launch app\MyApp.exe
//! wait-window --title "My App"
//! screenshot before.png
//! type --automation-id NameBox --text "Tobias"
//! click --automation-id SaveButton
//! expect --automation-id StatusText --expect-name "Saved: Tobias"
//! screenshot after.png
//! ```
//!
//! Parsing is deliberately separate from running it, so the interesting part —
//! quoting, comments, which verb owns which argument — is testable without a
//! virtual machine anywhere in sight.

use anyhow::{bail, Result};

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Capture to a file on the host.
    Screenshot { file: String, args: Vec<String> },
    /// Read a property through UI Automation and compare it.
    Expect { selector: Vec<String>, check: Check },
    /// Wait, for the rare case where a UI settles on a timer.
    Sleep { ms: u64 },
    /// Anything else: passed to the guest bridge unchanged.
    Bridge(Vec<String>),
}

/// What an `expect` line asserts.
#[derive(Debug, PartialEq, Eq)]
pub struct Check {
    pub field: Field,
    pub expected: String,
    /// `--expect-contains` is a substring match; the rest are exact.
    pub contains: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Field {
    Name,
    Value,
    ToggleState,
}

impl Field {
    /// The key this field has in the bridge's element JSON.
    pub fn json_key(self) -> &'static str {
        match self {
            Field::Name => "name",
            Field::Value => "value",
            Field::ToggleState => "toggleState",
        }
    }
}

#[derive(Debug)]
pub struct Script {
    pub steps: Vec<Step>,
}

pub fn parse(text: &str) -> Result<Script> {
    let mut steps = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words = tokenize(line)
            .map_err(|e| anyhow::anyhow!("line {}: {e}", n + 1))?;
        let step = build(words).map_err(|e| anyhow::anyhow!("line {}: {e}", n + 1))?;
        steps.push(step);
    }
    if steps.is_empty() {
        bail!("the script has no steps");
    }
    Ok(Script { steps })
}

fn build(words: Vec<String>) -> Result<Step> {
    let verb = words[0].as_str();
    let rest = &words[1..];
    match verb {
        "screenshot" => {
            let file = rest
                .first()
                .filter(|w| !w.starts_with("--"))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("screenshot needs a file name"))?;
            Ok(Step::Screenshot { file, args: rest[1..].to_vec() })
        }
        "sleep" => {
            let ms = rest
                .first()
                .ok_or_else(|| anyhow::anyhow!("sleep needs a duration in milliseconds"))?
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("sleep takes a whole number of milliseconds"))?;
            Ok(Step::Sleep { ms })
        }
        "expect" => {
            let mut selector = Vec::new();
            let mut check: Option<Check> = None;
            let mut i = 0;
            while i < rest.len() {
                let (field, contains) = match rest[i].as_str() {
                    "--expect-name" => (Field::Name, false),
                    "--expect-value" => (Field::Value, false),
                    "--expect-toggle" => (Field::ToggleState, false),
                    "--expect-contains" => (Field::Value, true),
                    "--expect-name-contains" => (Field::Name, true),
                    _ => {
                        selector.push(rest[i].clone());
                        i += 1;
                        continue;
                    }
                };
                let expected = rest
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("{} needs a value", rest[i]))?
                    .clone();
                if check.is_some() {
                    bail!("expect takes one assertion per line");
                }
                check = Some(Check { field, expected, contains });
                i += 2;
            }
            let check = check.ok_or_else(|| {
                anyhow::anyhow!(
                    "expect needs one of --expect-name, --expect-value, --expect-toggle, \
                     --expect-contains or --expect-name-contains"
                )
            })?;
            if selector.is_empty() {
                bail!("expect needs a selector, such as --automation-id");
            }
            Ok(Step::Expect { selector, check })
        }
        _ => Ok(Step::Bridge(words)),
    }
}

/// Split a line into words, honouring double quotes.
///
/// Backslash escapes are deliberately absent: Windows paths are full of
/// backslashes and treating them as escapes makes every realistic `launch` line
/// wrong.
fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut started = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if in_quotes {
        bail!("unterminated quote");
    }
    if started {
        out.push(cur);
    }
    if out.is_empty() {
        bail!("no verb on this line");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(text: &str) -> Vec<Step> {
        parse(text).unwrap().steps
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let s = steps("# a comment\n\n   \nwindows\n  # another\n");
        assert_eq!(s, vec![Step::Bridge(vec!["windows".into()])]);
    }

    #[test]
    fn quoted_arguments_stay_one_word() {
        let s = steps(r#"type --automation-id NameBox --text "Tobias Carlbom""#);
        assert_eq!(
            s,
            vec![Step::Bridge(vec![
                "type".into(),
                "--automation-id".into(),
                "NameBox".into(),
                "--text".into(),
                "Tobias Carlbom".into(),
            ])]
        );
    }

    /// Windows paths are mostly backslashes; treating them as escapes would
    /// break every `launch` line.
    #[test]
    fn backslashes_are_literal() {
        let s = steps(r"launch app\Demo App\MyApp.exe");
        match &s[0] {
            Step::Bridge(a) => assert_eq!(a[1], r"app\Demo"),
            other => panic!("{other:?}"),
        }
        let s = steps(r#"launch "app\Demo App\MyApp.exe""#);
        match &s[0] {
            Step::Bridge(a) => assert_eq!(a[1], r"app\Demo App\MyApp.exe"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_quoted_string_is_still_an_argument() {
        let s = steps(r#"type --automation-id NameBox --text """#);
        match &s[0] {
            Step::Bridge(a) => assert_eq!(a.last().unwrap(), ""),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn screenshot_takes_a_file_then_options() {
        let s = steps(r#"screenshot after.png --title "My App""#);
        assert_eq!(
            s,
            vec![Step::Screenshot {
                file: "after.png".into(),
                args: vec!["--title".into(), "My App".into()],
            }]
        );
    }

    #[test]
    fn screenshot_without_a_file_is_an_error() {
        assert!(parse("screenshot --title X").is_err());
    }

    #[test]
    fn expect_splits_the_selector_from_the_assertion() {
        let s = steps(r#"expect --automation-id StatusText --expect-name "Saved: Tobias""#);
        assert_eq!(
            s,
            vec![Step::Expect {
                selector: vec!["--automation-id".into(), "StatusText".into()],
                check: Check {
                    field: Field::Name,
                    expected: "Saved: Tobias".into(),
                    contains: false,
                },
            }]
        );
    }

    /// `--name` is a selector and `--expect-name` is an assertion. Confusing the
    /// two would silently look up the wrong element.
    #[test]
    fn name_selects_while_expect_name_asserts() {
        let s = steps(r#"expect --name Save --expect-value hello"#);
        match &s[0] {
            Step::Expect { selector, check } => {
                assert_eq!(selector, &vec!["--name".to_string(), "Save".to_string()]);
                assert_eq!(check.field, Field::Value);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn expect_requires_both_a_selector_and_an_assertion() {
        assert!(parse("expect --automation-id StatusText").is_err());
        assert!(parse("expect --expect-name Saved").is_err());
    }

    #[test]
    fn expect_rejects_two_assertions_on_one_line() {
        assert!(parse("expect --automation-id X --expect-name a --expect-value b").is_err());
    }

    #[test]
    fn contains_is_recorded_separately_from_the_field() {
        let s = steps("expect --automation-id S --expect-contains Design");
        match &s[0] {
            Step::Expect { check, .. } => {
                assert!(check.contains);
                assert_eq!(check.field, Field::Value);
            }
            other => panic!("{other:?}"),
        }
        let s = steps("expect --automation-id S --expect-name-contains Design");
        match &s[0] {
            Step::Expect { check, .. } => {
                assert!(check.contains);
                assert_eq!(check.field, Field::Name);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sleep_parses_milliseconds() {
        assert_eq!(steps("sleep 250"), vec![Step::Sleep { ms: 250 }]);
        assert!(parse("sleep soon").is_err());
        assert!(parse("sleep").is_err());
    }

    #[test]
    fn an_unterminated_quote_is_an_error_with_a_line_number() {
        let e = parse("windows\ntype --text \"oops").unwrap_err().to_string();
        assert!(e.contains("line 2"), "{e}");
    }

    #[test]
    fn an_empty_script_is_an_error() {
        assert!(parse("# nothing but comments\n\n").is_err());
    }
}
