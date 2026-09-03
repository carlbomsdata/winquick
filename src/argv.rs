//! Turning a Unix argv into one Windows command line.
//!
//! This looks like a one-liner and is not. The text WinQuick produces is
//! written into a batch file and run by `cmd.exe`, so it passes through two
//! parsers with different rules before it reaches the program:
//!
//! ```text
//!   argv from the user's shell
//!        -> this module
//!        -> cmd.exe parses the batch line  (quotes group, & | < > are operators)
//!        -> CreateProcess hands one string to the program
//!        -> the program's C runtime splits it back into argv
//! ```
//!
//! The mistake v0.2.0 made was applying one algorithm to both. It always used
//! C-runtime quoting — backslash-doubling before a quote — which is right for
//! the *last* step and wrong for the middle one. `cmd` has never understood
//! `\"`, so `winquick run -- cmd /c 'echo say "hi"'` printed `say \"hi\"`, and
//! `type "C:\Program Files\x"` failed outright.
//!
//! Removing the escaping does not fix it either: that breaks
//! `pwsh -Command 'Write-Output "quoted"'`, because pwsh really is a program
//! whose C runtime expects those backslashes. Both behaviours are needed; which
//! one applies depends on who consumes the text.
//!
//! So the first argument decides:
//!
//! * **`cmd` / `cmd.exe`** — everything after its switches is *cmd syntax the
//!   user wrote*. Their quotes are theirs, and `&`, `|` and `>` are meant to
//!   work. If they wrote it as one argument it is passed through untouched; if
//!   their shell split it into words, each word containing spaces is regrouped
//!   so `dir C:\Program Files` still reaches cmd as one path.
//! * **anything else** — a real executable. Quote each argument by the C-runtime
//!   rules so its argv arrives exactly as given, which also happens to protect
//!   cmd's metacharacters, since they end up inside quotes.
//!
//! What this cannot fix is `%`. The command is delivered as a batch file, so
//! `%PATH%` expands and a `for` loop needs `%%i`, exactly as in any `.cmd`
//! file. Removing that would mean launching the program without cmd in the
//! path, which needs compiled code in the guest — see docs/architecture.md.

/// Build the command line for one run.
pub fn join(argv: &[String]) -> String {
    if !is_cmd(argv.first().map(String::as_str)) {
        let line = argv.iter().map(|a| crt_arg(a)).collect::<Vec<_>>().join(" ");
        return shield_from_cmd(&line);
    }

    // cmd's own switches (/c, /k, /q ...) come first; what follows is the
    // payload the user wants cmd to run.
    let rest = &argv[1..];
    let split = rest.iter().position(|a| !a.starts_with('/')).unwrap_or(rest.len());
    let (switches, payload) = rest.split_at(split);

    let mut out = String::from("cmd");
    for s in switches {
        out.push(' ');
        out.push_str(s);
    }
    // One payload argument means the user wrote a cmd command line and quoted
    // it for their own shell: `cmd /c 'echo A & echo B'`. It is already exactly
    // what cmd should see, and touching it is what broke v0.2.0.
    //
    // Several arguments mean their shell split the command into words, so each
    // word that contains spaces has to be regrouped or `dir C:\Program Files`
    // would arrive as two arguments.
    if payload.len() == 1 {
        out.push(' ');
        out.push_str(&payload[0]);
    } else {
        for a in payload {
            out.push(' ');
            out.push_str(&cmd_arg(a));
        }
    }
    out
}

/// Is this invocation `cmd` itself, however the user spelled it?
fn is_cmd(first: Option<&str>) -> bool {
    let Some(f) = first else { return false };
    let base = f.rsplit(['\\', '/']).next().unwrap_or(f);
    base.eq_ignore_ascii_case("cmd") || base.eq_ignore_ascii_case("cmd.exe")
}

/// An argument being handed to `cmd` for cmd to parse.
///
/// The user is writing shell syntax, so their quotes and operators survive
/// untouched. The only thing added is grouping for an argument that their own
/// shell already split off and that contains spaces — without it,
/// `dir C:\Program Files` would reach cmd as two arguments.
fn cmd_arg(a: &str) -> String {
    if a.is_empty() {
        // An empty argument still has to occupy a slot.
        return "\"\"".to_string();
    }
    if a.contains([' ', '\t']) && !a.contains('"') {
        return format!("\"{a}\"");
    }
    a.to_string()
}

/// Hide cmd's metacharacters from cmd, without disturbing what the program sees.
///
/// The command line is delivered inside a batch file, so `cmd` parses it before
/// the program ever runs — and cmd tracks quoting by counting `"`, knowing
/// nothing of the C runtime's `\"`. So in
///
/// ```text
///     pwsh -Command "Write-Output \"a&b\""
/// ```
///
/// cmd reads the `\"` as the *closing* quote, decides `&` is unquoted, and
/// splits the line into two commands. The program then never runs, and the user
/// sees `'b\""' is not recognized`.
///
/// So walk the finished line the way cmd will, and `^`-escape any metacharacter
/// that cmd would consider unquoted. cmd strips the carets before the program
/// is started, so its argv is unchanged.
///
/// `%` is deliberately left alone: batch expansion is documented behaviour, not
/// something this layer gets to redefine.
fn shield_from_cmd(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                out.push(ch);
            }
            '&' | '|' | '<' | '>' | '^' | '(' | ')' if !in_quotes => {
                out.push('^');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// An argument being handed to a real executable, quoted by the rules its C
/// runtime uses to split the command line back up.
///
/// Backslashes are only special immediately before a quote: there, each one
/// must be doubled and the quote itself escaped. Everywhere else a backslash is
/// literal, which is why Windows paths survive without mangling.
fn crt_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for ch in a.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('"');
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    // A trailing run of backslashes sits against the closing quote, so it has
    // to be doubled too or the quote would be escaped instead of closing.
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }
    fn j(parts: &[&str]) -> String {
        join(&v(parts))
    }

    // ---- cmd context: the user is writing cmd syntax -------------------

    /// The v0.2.0 bug, from the dogfood report. `cmd` has never understood the
    /// C-runtime's `\"`, so escaping a quote for it corrupts the command.
    #[test]
    fn cmd_keeps_the_users_quotes_verbatim() {
        assert_eq!(j(&["cmd", "/c", r#"echo say "hi""#]), r#"cmd /c echo say "hi""#);
        assert_eq!(
            j(&["cmd", "/c", r#"type "C:\Program Files\x.txt""#]),
            r#"cmd /c type "C:\Program Files\x.txt""#
        );
    }

    /// The other half: a path the user's own shell split into one argument
    /// still has to reach cmd as one argument.
    #[test]
    fn cmd_groups_an_unquoted_argument_containing_spaces() {
        assert_eq!(
            j(&["cmd", "/c", "dir", r"C:\Program Files"]),
            r#"cmd /c dir "C:\Program Files""#
        );
    }

    /// Operators are the point of asking for cmd in the first place.
    #[test]
    fn cmd_leaves_operators_alone() {
        assert_eq!(j(&["cmd", "/c", "echo A & echo B"]), "cmd /c echo A & echo B");
        assert_eq!(j(&["cmd", "/c", "dir | find \"x\""]), "cmd /c dir | find \"x\"");
        assert_eq!(j(&["cmd", "/c", "echo x > out.txt"]), "cmd /c echo x > out.txt");
        assert_eq!(j(&["cmd", "/c", "(echo a) && (echo b)"]), "cmd /c (echo a) && (echo b)");
    }

    #[test]
    fn cmd_is_recognised_however_it_is_spelled() {
        assert!(is_cmd(Some("cmd")));
        assert!(is_cmd(Some("cmd.exe")));
        assert!(is_cmd(Some("CMD.EXE")));
        assert!(is_cmd(Some(r"C:\Windows\System32\cmd.exe")));
        assert!(!is_cmd(Some("cmdlet")));
        assert!(!is_cmd(Some("pwsh")));
        assert!(!is_cmd(None));
    }

    // ---- native context: the program's C runtime parses it -------------

    /// PowerShell is a real executable, so its arguments must survive the C
    /// runtime intact. Removing this escaping was the fix that looked obvious
    /// and turned `Write-Output "quoted string"` into `string`.
    #[test]
    fn a_native_program_gets_crt_quoting() {
        assert_eq!(
            j(&["pwsh", "-Command", r#"Write-Output "quoted string""#]),
            r#"pwsh -Command "Write-Output \"quoted string\"""#
        );
    }

    #[test]
    fn plain_arguments_are_untouched() {
        assert_eq!(j(&["dotnet", "--version"]), "dotnet --version");
        assert_eq!(j(&["dotnet", "build", "-c", "Release"]), "dotnet build -c Release");
    }

    #[test]
    fn spaces_are_grouped() {
        assert_eq!(j(&["app.exe", "two words"]), r#"app.exe "two words""#);
    }

    /// A path ending in a backslash must not escape the closing quote.
    #[test]
    fn a_trailing_backslash_is_doubled_against_the_quote() {
        assert_eq!(j(&["app.exe", r"C:\dir with space\"]), r#"app.exe "C:\dir with space\\""#);
        assert_eq!(j(&["app.exe", r"plain\"]), r"app.exe plain\");
    }

    /// Backslashes are only special immediately before a quote.
    #[test]
    fn interior_backslashes_stay_single() {
        assert_eq!(j(&["app.exe", r"C:\a\b\c.txt"]), r"app.exe C:\a\b\c.txt");
        assert_eq!(j(&["app.exe", r#"a\"b"#]), r#"app.exe "a\\\"b""#);
        assert_eq!(j(&["app.exe", r#"a\\"b"#]), r#"app.exe "a\\\\\"b""#);
    }

    #[test]
    fn an_empty_argument_keeps_its_slot() {
        assert_eq!(j(&["app.exe", "", "after"]), r#"app.exe "" after"#);
        assert_eq!(j(&["cmd", "/c", "echo", ""]), r#"cmd /c echo """#);
    }

    #[test]
    fn unicode_passes_through_both_contexts() {
        assert_eq!(j(&["app.exe", "åäö-日本語"]), "app.exe åäö-日本語");
        assert_eq!(j(&["cmd", "/c", "echo åäö-日本語"]), "cmd /c echo åäö-日本語");
        assert_eq!(j(&["app.exe", "two 日本語 words"]), r#"app.exe "two 日本語 words""#);
    }

    /// A native program's argument must not be re-interpreted by cmd.
    ///
    /// Quoting alone does not achieve this, which is what the first version of
    /// this module got wrong: cmd counts `"` and does not understand `\"`, so
    /// after an escaped quote it believes it is *outside* quotes and treats the
    /// next `&` as an operator. Every metacharacter cmd would read as unquoted
    /// has to be `^`-escaped.
    #[test]
    fn metacharacters_reaching_a_native_program_are_protected() {
        for meta in ["a&b", "a|b", "a>b", "a<b", "a^b", "a(b)c"] {
            let line = j(&["app.exe", meta]);
            assert!(!cmd_would_split(&line), "{meta} reached cmd unprotected as {line}");
        }
        assert_eq!(j(&["app.exe", "a&b"]), r"app.exe a^&b");
        assert_eq!(j(&["app.exe", "a & b"]), r#"app.exe "a & b""#);
    }

    /// The exact failure found by the v0.2.1 release gate: an argument holding
    /// both a quote and a metacharacter. cmd used to split this into two
    /// commands and report `'b\""' is not recognized`.
    #[test]
    fn a_quote_and_a_metacharacter_together_still_reach_the_program() {
        let line = j(&["pwsh", "-NoProfile", "-Command", r#"Write-Output "a&b""#]);
        assert!(!cmd_would_split(&line), "cmd would still split: {line}");
        assert_eq!(line, r#"pwsh -NoProfile -Command "Write-Output \"a^&b\"""#);
        for meta in ['&', '|', '<', '>'] {
            let arg = format!("Write-Output \"x{meta}y\"");
            let l = j(&["pwsh", "-Command", &arg]);
            assert!(!cmd_would_split(&l), "{meta}: {l}");
        }
    }

    /// cmd's own view of the line: quoting toggles on every `"`, and any
    /// metacharacter seen outside quotes and not preceded by `^` splits it.
    fn cmd_would_split(line: &str) -> bool {
        let mut in_quotes = false;
        let mut escaped = false;
        for ch in line.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '^' if !in_quotes => escaped = true,
                '"' => in_quotes = !in_quotes,
                '&' | '|' | '<' | '>' if !in_quotes => return true,
                _ => {}
            }
        }
        false
    }

    /// Percent is not this module's to solve: the command is delivered as a
    /// batch file, so `%` means what it means in a batch file. The test records
    /// that we deliberately do not mangle it.
    #[test]
    fn percent_is_passed_through_unchanged() {
        assert_eq!(j(&["cmd", "/c", "echo %PATH%"]), "cmd /c echo %PATH%");
        assert_eq!(
            j(&["cmd", "/c", "for /L %%i in (1,1,3) do @echo %%i"]),
            "cmd /c for /L %%i in (1,1,3) do @echo %%i"
        );
    }

    /// The whole point of the two contexts: the same argument is rendered
    /// differently depending on who will parse it.
    #[test]
    fn the_same_argument_is_rendered_for_its_consumer() {
        let arg = r#"say "hi""#;
        assert_eq!(j(&["cmd", "/c", arg]), r#"cmd /c say "hi""#);
        assert_eq!(j(&["pwsh", "-c", arg]), r#"pwsh -c "say \"hi\"""#);
    }
}
